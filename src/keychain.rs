//! Saved login, held in the login keychain.
//!
//! A generic-password item lets macOS own encryption, permissions and
//! replacement. The ACL is the system's, the item appears in Keychain Access
//! under its own name, and deletion is one keychain operation rather than a
//! filesystem transaction.
//!
//! One item holds both fields, because the client asks for the pair or for
//! neither. The account name is not itself a secret, but splitting it out would
//! mean two items that can disagree.
//!
//! What the ACL actually keys on
//! -----------------------------
//! Worth writing down, because the obvious guesses are all wrong and the cost
//! of guessing is a dialog asking someone for their macOS login password.
//!
//! The item carries a list of the code allowed to open it. An entry is not a
//! path: the same signed program reads its item from anywhere on disk, so
//! moving the application to `/Applications` does not by itself cost anything.
//! Nor is it the binary: a rebuild with an entirely different code hash still
//! reads, which is what lets an update ship without logging everybody out.
//! What an entry pins is the *designated requirement* — for this program,
//! `identifier "com.gwnative.app"` plus the signing certificate. Change either
//! and the item belongs to somebody else, and the system asks.
//!
//! That is one prompt per identity that ever wrote the item, and the list grows
//! rather than replacing. An ad-hoc `cargo build` binary is a fresh identity on
//! every relink, so a development machine collects entries the way this one
//! did, while a signed build — dev or shipped — matches the entry it made.
//!
//! The data-protection keychain has none of this: access is decided by the
//! signing team, so no list and no prompt. It is also unreachable here. It
//! needs the `keychain-access-groups` entitlement, which needs a provisioning
//! profile embedded in the bundle to authorise it; without one `SecItemAdd`
//! returns `errSecMissingEntitlement`, and with the entitlement but no profile
//! macOS kills the process at launch. That profile would have to be issued per
//! App ID and re-issued before it expired, so the Developer ID build that
//! anybody can download and run would acquire an expiry date. Not worth it for
//! one saved password — so this stays on the legacy keychain, and simply never
//! asks. See `never_asks` below.

use security_framework::base::Error;
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
use security_framework_sys::keychain::SecKeychainSetUserInteractionAllowed;
use serde::{Deserialize, Serialize};

/// Shown in Keychain Access as the item's name, so it says what it is.
const SERVICE: &str = "gwnative (Guild Wars)";
const ACCOUNT: &str = "login";

/// `errSecItemNotFound`: nothing saved yet. The ordinary state on a first run,
/// and the only failure here that deserves no comment.
const ITEM_NOT_FOUND: i32 = -25300;

/// The three ways the keychain says "not for you".
const AUTH_FAILED: i32 = -25293;
const USER_CANCELED: i32 = -128;
const INTERACTION_NOT_ALLOWED: i32 = -25308;

/// Why the keychain said no.
///
/// These used to be one condition, and the message they shared told everyone
/// their login "belongs to a differently signed build". For two of the three
/// that is simply untrue, and it sends someone to read `scripts/signed-run`
/// about a dialog they dismissed half a second ago.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Denial {
    /// The ACL refused: this build is not on the item's list. The signature
    /// story, and the only one of the three that is about this program at all.
    Refused,
    /// The prompt was answered with Cancel. Unreachable while this program
    /// suppresses prompts, and kept because that suppression is one call and
    /// the keychain is not this module's to promise things about.
    Canceled,
    /// The keychain wanted to ask permission and was not allowed to. Since
    /// [`Silent`], that is this program's own doing on every read rather than a
    /// locked screen — the same refusal as [`Self::Refused`], caught one step
    /// earlier and without the dialog.
    NoPrompt,
}

impl Denial {
    fn of(e: &Error) -> Option<Self> {
        match e.code() {
            AUTH_FAILED => Some(Self::Refused),
            USER_CANCELED => Some(Self::Canceled),
            INTERACTION_NOT_ALLOWED => Some(Self::NoPrompt),
            _ => None,
        }
    }

    /// What to tell someone whose saved login did not come back.
    fn help(self) -> &'static str {
        match self {
            Self::Refused => DENIED_HELP,
            Self::Canceled => {
                "the keychain prompt was dismissed, so the saved login was not read. \
                 Sign in again to save it for this build."
            }
            Self::NoPrompt => DENIED_HELP,
        }
    }
}

/// The keychain identifies the application allowed to open an item by its code
/// signature, so a saved login written under a different one is unreadable
/// while looking exactly like never having logged in at all. Telling those two
/// apart is the entire reason this module inspects status codes instead of
/// using `.ok()`.
///
/// It says what to do rather than what went wrong, because signing in again
/// genuinely ends it: [`store_in`] replaces the item, and the replacement
/// records this build. One sign-in, not one dialog per launch.
const DENIED_HELP: &str = "a saved login exists, but it was saved by a build with a different \
    code signature and this one is not on its list. Signing in again replaces it, and it will \
    be read without asking from then on.";

/// The same condition, for the build that can do something about it.
///
/// Only ever printed by an ad-hoc build, where the cause is a `cargo build`
/// that bypassed the runner rather than anything about the installed app. On a
/// signed build it would be advice to go and read a shell script that is not
/// there.
const DEV_HELP: &str = "this build is ad-hoc signed, so it will need saying again after the \
    next relink. Run it through scripts/signed-run (`cargo run`) to sign it with a stable \
    identity.";

/// The identifier `scripts/signed-run` and `scripts/bundle` both set, and the
/// one the ACL of a saved item ends up naming. Cargo's ad-hoc linker signature
/// uses `gwnative-<build hash>` instead, which is a different identifier after
/// every relink.
const IDENTIFIER: &str = "com.gwnative.app";

/// Say so, once at startup, when this build cannot keep a saved login.
///
/// Waiting for the read to fail is too late and too quiet: by then the account
/// has silently not appeared, and the reason is a link-time one that nothing
/// on screen mentions. The failure is decided at link time, so it can be
/// reported at startup — before the client asks — and named for what it is.
///
/// `cargo run` goes through the runner and is signed. `cargo build` is not,
/// which is the whole reason this check exists: the binary it leaves behind
/// runs perfectly well and silently loses the login on the next rebuild.
pub fn check_identity() {
    if stably_signed() {
        return;
    }
    note!(
        "[keychain] this build is ad-hoc signed, so the saved login will not survive the \
         next rebuild — the account simply stops appearing. Run it through \
         scripts/signed-run (`cargo run`) or scripts/bundle to sign it with a stable \
         identity."
    );
}

/// Whether this running code claims the identifier saved items are written
/// under — that is, whether what it saves will still be readable after the next
/// build.
///
/// A signature is the only thing asked about. Where the binary sits does not
/// come into it, and neither does its hash; see the note at the top of this
/// file for why that is the right question and the other two are not.
///
/// Unanswerable counts as fine. Both failures mean the system would not tell us
/// about our own code, and refusing to start over a diagnostic is worse than
/// the diagnostic being wrong.
fn stably_signed() -> bool {
    use std::str::FromStr;

    use security_framework::os::macos::code_signing::{Flags, SecCode, SecRequirement};

    let Ok(requirement) = SecRequirement::from_str(&format!("identifier \"{IDENTIFIER}\"")) else {
        return true;
    };
    let Ok(code) = SecCode::for_self(Flags::NONE) else {
        return true;
    };
    code.check_validity(Flags::NONE, &requirement).is_ok()
}

#[derive(Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Bound well above any real account name or password. A caller that sends more
/// is confused about what this stores, and the keychain is not the place to find
/// out how large an item it will take.
const MAX_FIELD: usize = 4096;

/// The three keychain calls this module makes, named so a test can answer them.
///
/// The interesting behaviour here is entirely in what happens after a refusal,
/// and a refusal is exactly what a test cannot arrange against the real
/// keychain: it would need a second code signature and somebody to press
/// Cancel. Behind this trait the retry is ordinary code with ordinary tests.
trait Vault {
    fn get(&self) -> Result<Vec<u8>, Error>;
    fn set(&self, value: &[u8]) -> Result<(), Error>;
    fn delete(&self) -> Result<(), Error>;
}

/// While this is alive, the keychain answers rather than asking.
///
/// This is the fix for the dialog. Left to itself, an item whose list does not
/// name this build makes macOS put up "Guild Wars wants to use your
/// confidential information stored in ... enter the login keychain password" —
/// over a game, naming a keychain item, asking for the password to the account
/// the person is already logged into. Every answer to it is bad. Deny loses the
/// login. Allow hands a full-disk-encryption-grade password to a dialog nobody
/// can verify, and teaches that doing so is normal. Always Allow does that and
/// adds one more entry to the list that grew this problem.
///
/// So it is never asked. `errSecInteractionNotAllowed` comes back instead, the
/// saved login is treated as absent, and the client shows its own sign-in form
/// — the one that belongs to this application and asks for the password to the
/// account it is actually signing into. Signing in there rewrites the item
/// under this build's signature via [`store_in`], so the cost of a signature
/// change is one sign-in, once, and never a system password.
///
/// The flag is process-wide, hence a guard rather than a call: restoring it
/// leaves the process as it was found for anything that legitimately wants to
/// prompt later, and `Drop` restores it on the panic path too.
struct Silent;

impl Silent {
    fn new() -> Self {
        // Both calls ignore their status: it fails only if there is no
        // keychain services connection at all, in which case the operation
        // being guarded is about to fail anyway and say so properly.
        unsafe { SecKeychainSetUserInteractionAllowed(0) };
        Self
    }
}

impl Drop for Silent {
    fn drop(&mut self) {
        unsafe { SecKeychainSetUserInteractionAllowed(1) };
    }
}

/// The login keychain, which is what every caller outside the tests wants.
struct System;

impl Vault for System {
    fn get(&self) -> Result<Vec<u8>, Error> {
        let _silent = Silent::new();
        get_generic_password(SERVICE, ACCOUNT)
    }
    /// Silent for the same reason `get` is: writing over an existing item is an
    /// update, and an update asks the item's permission exactly as a read does.
    /// The refusal is the useful outcome here — [`store_in`] turns it into a
    /// replace, which is what actually fixes the item.
    fn set(&self, value: &[u8]) -> Result<(), Error> {
        let _silent = Silent::new();
        set_generic_password(SERVICE, ACCOUNT, value)
    }
    /// Removing an item does not require opening it, so this would not have
    /// prompted. Guarded anyway, so that the rule is "this program does not
    /// raise keychain dialogs" rather than a list of the places it might.
    fn delete(&self) -> Result<(), Error> {
        let _silent = Silent::new();
        delete_generic_password(SERVICE, ACCOUNT)
    }
}

pub fn load() -> Option<Credentials> {
    load_from(&System)
}

pub fn store(credentials: &Credentials) -> Result<(), String> {
    store_in(&System, credentials)
}

/// Deleting what was never there is the caller's intended end state, so a
/// missing item is not an error worth reporting.
pub fn clear() {
    let _ = System.delete();
}

fn load_from(vault: &impl Vault) -> Option<Credentials> {
    let raw = match vault.get() {
        Ok(raw) => raw,
        Err(e) => {
            match Denial::of(&e) {
                // A first run. The ordinary state, and nothing to say about it.
                _ if e.code() == ITEM_NOT_FOUND => {}
                Some(denial) => {
                    note!("[keychain] {}", denial.help());
                    // Only to the build that can act on it. On an installed
                    // copy this would be a pointer to a shell script that is
                    // not there, about a cause that is not the reason.
                    if denial != Denial::Canceled && !stably_signed() {
                        note!("[keychain] {DEV_HELP}");
                    }
                }
                // Anything else is unexpected rather than explicable, so pass
                // the system's own wording through instead of guessing at it.
                None => note!("[keychain] could not read the saved login: {e}"),
            }
            return None;
        }
    };
    match serde_json::from_slice::<Credentials>(&raw) {
        Ok(credentials) => Some(credentials),
        // An item that will not parse is one this app cannot use, and leaving it
        // in place would fail the same way on every launch.
        Err(e) => {
            note!("[keychain] stored login is unreadable ({e}); discarding it");
            let _ = vault.delete();
            None
        }
    }
}

fn store_in(vault: &impl Vault, credentials: &Credentials) -> Result<(), String> {
    if credentials.username.len() > MAX_FIELD || credentials.password.len() > MAX_FIELD {
        return Err("credentials are too long to store".into());
    }
    let encoded = serde_json::to_vec(credentials).map_err(|e| e.to_string())?;
    match vault.set(&encoded) {
        Ok(()) => Ok(()),
        // Saving over an existing item is an update, and an update asks the
        // same permission a read does. So an item left by a differently signed
        // build refuses the overwrite too, and signing in again — the one thing
        // that would fix it — is exactly what cannot happen. Replacing it does
        // work: removing an item does not require opening it, so the delete
        // goes through where the update would not, and adding it back is an
        // ordinary create that records this build as the owner.
        //
        // Once only. The second `set` is a create against nothing, so it asks
        // no permission of the old item and a refusal there is not the same
        // condition — no third attempt would help, and the only thing left to
        // say is what to remove by hand.
        Err(e) if Denial::of(&e).is_some() => {
            vault.delete().map_err(|e| by_hand(&e))?;
            vault.set(&encoded).map_err(|e| by_hand(&e))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The last word, for when the app has run out of ways to fix this itself.
///
/// Deliberately says nothing about code signatures: by the time this fires the
/// cause could equally be a dismissed prompt or a locked screen, and the one
/// instruction that works for all three is to remove the item.
fn by_hand(e: &Error) -> String {
    format!(
        "the saved login could not be replaced ({e}); delete \"{SERVICE}\" in \
         Keychain Access, then sign in again"
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A keychain that answers from a script, and remembers what it was asked.
    #[derive(Default)]
    struct Fake {
        /// One answer per `set`, in order. `None` means it succeeded.
        set_answers: RefCell<Vec<Option<i32>>>,
        get_answer: Option<i32>,
        delete_answer: Option<i32>,
        item: RefCell<Option<Vec<u8>>>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl Fake {
        fn refusing(codes: &[i32]) -> Self {
            Self {
                set_answers: RefCell::new(codes.iter().map(|c| Some(*c)).collect()),
                ..Self::default()
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.borrow().clone()
        }
    }

    impl Vault for Fake {
        fn get(&self) -> Result<Vec<u8>, Error> {
            self.calls.borrow_mut().push("get");
            if let Some(code) = self.get_answer {
                return Err(Error::from_code(code));
            }
            self.item
                .borrow()
                .clone()
                .ok_or_else(|| Error::from_code(ITEM_NOT_FOUND))
        }

        fn set(&self, value: &[u8]) -> Result<(), Error> {
            self.calls.borrow_mut().push("set");
            let mut answers = self.set_answers.borrow_mut();
            let answer = if answers.is_empty() {
                None
            } else {
                answers.remove(0)
            };
            match answer {
                Some(code) => Err(Error::from_code(code)),
                None => {
                    *self.item.borrow_mut() = Some(value.to_vec());
                    Ok(())
                }
            }
        }

        fn delete(&self) -> Result<(), Error> {
            self.calls.borrow_mut().push("delete");
            match self.delete_answer {
                Some(code) => Err(Error::from_code(code)),
                None => {
                    *self.item.borrow_mut() = None;
                    Ok(())
                }
            }
        }
    }

    fn credentials() -> Credentials {
        Credentials {
            username: "player@example.com".into(),
            password: "not a real one".into(),
        }
    }

    #[test]
    fn a_refused_update_is_replaced_rather_than_reported() {
        // The whole point of the retry: an item this build cannot open can
        // still be removed and written afresh.
        for code in [AUTH_FAILED, USER_CANCELED, INTERACTION_NOT_ALLOWED] {
            let vault = Fake::refusing(&[code]);
            assert!(store_in(&vault, &credentials()).is_ok(), "code {code}");
            assert_eq!(vault.calls(), ["set", "delete", "set"]);
            assert!(vault.item.borrow().is_some());
        }
    }

    #[test]
    fn a_second_refusal_says_what_to_remove_by_hand() {
        // The comment promised this and the code used to pass the system's
        // wording through instead, which names no item and suggests no action.
        let vault = Fake::refusing(&[AUTH_FAILED, AUTH_FAILED]);
        let error = store_in(&vault, &credentials()).unwrap_err();
        assert!(error.contains(SERVICE), "{error}");
        assert!(error.contains("Keychain Access"), "{error}");
        assert_eq!(vault.calls(), ["set", "delete", "set"], "no third attempt");
    }

    #[test]
    fn a_failed_delete_says_the_same_thing() {
        let vault = Fake {
            delete_answer: Some(AUTH_FAILED),
            ..Fake::refusing(&[AUTH_FAILED])
        };
        let error = store_in(&vault, &credentials()).unwrap_err();
        assert!(error.contains("Keychain Access"), "{error}");
        assert_eq!(vault.calls(), ["set", "delete"], "nothing to write over");
    }

    #[test]
    fn a_suppressed_prompt_is_the_same_condition_as_a_refusal() {
        // Since prompts are suppressed, "there was no way to ask" is how the
        // ACL refusing reaches this program most of the time, so it has to
        // read as the same thing rather than as a locked screen.
        assert_eq!(Denial::Refused.help(), Denial::NoPrompt.help());
        assert!(Denial::Refused.help().contains("code signature"));
        // Cancel is not this program's fault and does not get the signature
        // story, which would send someone off after the wrong cause.
        assert!(!Denial::Canceled.help().contains("code signature"));
        assert_eq!(Denial::of(&Error::from_code(ITEM_NOT_FOUND)), None);
    }

    /// The dialog this module exists to avoid asks for the *system* password,
    /// which is not the password being signed in with and must never be named
    /// as the way out of anything.
    #[test]
    fn nothing_ever_tells_anyone_to_type_their_mac_password() {
        let all = [
            Denial::Refused.help(),
            Denial::Canceled.help(),
            Denial::NoPrompt.help(),
            DEV_HELP,
        ];
        for message in all {
            for wrong in ["system password", "login keychain password", "Always Allow"] {
                assert!(!message.contains(wrong), "{message:?} mentions {wrong:?}");
            }
        }
    }

    /// Not a test of the keychain — a test that this program does not leave a
    /// process-wide switch flipped for whatever runs next.
    #[test]
    fn suppressing_prompts_is_undone_even_when_the_read_panics() {
        let restored = std::panic::catch_unwind(|| {
            let _silent = Silent::new();
            panic!("as a read might");
        });
        assert!(restored.is_err());
        // Nothing readable asserts the flag's value — it has no getter — so
        // what is checked is that a second guard can still be taken and
        // dropped, i.e. that the first one's Drop ran rather than aborting.
        drop(Silent::new());
    }

    #[test]
    fn an_unreadable_item_is_discarded_rather_than_failing_every_launch() {
        let vault = Fake::default();
        *vault.item.borrow_mut() = Some(b"not json".to_vec());
        assert!(load_from(&vault).is_none());
        assert_eq!(vault.calls(), ["get", "delete"]);
        assert!(vault.item.borrow().is_none());
    }

    /// The refusal used to be the rare case. With prompts suppressed it is the
    /// ordinary one for anybody whose item predates the build they are running,
    /// so it has to end in a sign-in form and not in an error — and it must not
    /// throw the item away, since the sign-in is what replaces it properly.
    #[test]
    fn a_refused_read_gives_up_quietly_rather_than_prompting_or_deleting() {
        for code in [AUTH_FAILED, INTERACTION_NOT_ALLOWED, USER_CANCELED] {
            let vault = Fake {
                get_answer: Some(code),
                ..Fake::default()
            };
            assert!(load_from(&vault).is_none(), "code {code}");
            assert_eq!(vault.calls(), ["get"], "code {code}");
        }
    }

    #[test]
    fn a_first_run_is_silent_and_a_round_trip_works() {
        let vault = Fake::default();
        assert!(load_from(&vault).is_none());
        assert_eq!(vault.calls(), ["get"], "nothing deleted, nothing to say");

        store_in(&vault, &credentials()).unwrap();
        let loaded = load_from(&vault).expect("what was stored");
        assert_eq!(loaded.username, "player@example.com");
    }

    #[test]
    fn an_absurd_field_is_refused_before_it_reaches_the_keychain() {
        let vault = Fake::default();
        let long = Credentials {
            username: "a".repeat(MAX_FIELD + 1),
            password: String::new(),
        };
        assert!(store_in(&vault, &long).is_err());
        assert!(vault.calls().is_empty());
    }
}
