//! Saved login, held in the login keychain.
//!
//! The Electron host this replaces encrypts a JSON file with `safeStorage` and
//! writes it beside its other state. That still ends at the keychain — it is
//! where the encryption key lives — but by way of a file the app has to create,
//! permission, atomically replace and delete, none of which the keychain needs.
//! A generic-password item is the same secret with none of that: the ACL is the
//! system's, the item shows up in Keychain Access under its own name, and
//! deleting it is one call rather than an unlink that has to tolerate a missing
//! file.
//!
//! One item holds both fields, because the client asks for the pair or for
//! neither. The account name is not itself a secret, but splitting it out would
//! mean two items that can disagree.

use security_framework::base::Error;
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
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
    /// The prompt was answered with Cancel.
    Canceled,
    /// There was no way to put a prompt on screen — a locked screen, or no
    /// window session at all. Nobody declined anything; nobody was asked.
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
                 Sign in again; answering Always Allow saves being asked next time."
            }
            Self::NoPrompt => {
                "the keychain could not ask permission — the screen is locked, or this \
                 is running without a window session — so the saved login was not read."
            }
        }
    }
}

/// The keychain identifies the application allowed to open an item by its code
/// signature. Cargo links an ad-hoc signature whose hash changes on every
/// build, so under one of those every rebuild is a different application and
/// the saved login turns unreadable — while looking exactly like never having
/// logged in at all. Telling those two apart is the entire reason this module
/// inspects status codes instead of using `.ok()`.
const DENIED_HELP: &str = "a saved login exists, but this build is not allowed to open it. \
    It was saved by a build with a different code signature; sign in once more to hand it \
    to this one. See scripts/signed-run for why that happens and how it is avoided.";

/// The identifier `scripts/signed-run` and `scripts/bundle` both set, and the
/// one the ACL of a saved item ends up naming. Cargo's ad-hoc linker signature
/// uses `gwnative-<build hash>` instead, which is a different identifier after
/// every relink.
const IDENTIFIER: &str = "com.gwnative.app";

/// Say so, once at startup, when this build cannot keep a saved login.
///
/// Waiting for the read to fail is too late and too quiet: by then the person
/// has already been asked for their system password by a dialog that names an
/// application they have never heard of, and whichever way they answer it the
/// account does not appear. The failure is decided at link time, so it can be
/// reported at startup — before the client asks — and named for what it is.
///
/// `cargo run` goes through the runner and is signed. `cargo build` is not,
/// which is the whole reason this check exists: the binary it leaves behind
/// runs perfectly well and silently loses the login on the next rebuild.
pub fn check_identity() {
    use std::str::FromStr;

    use security_framework::os::macos::code_signing::{Flags, SecCode, SecRequirement};

    let Ok(requirement) = SecRequirement::from_str(&format!("identifier \"{IDENTIFIER}\"")) else {
        return;
    };
    let Ok(code) = SecCode::for_self(Flags::NONE) else {
        return;
    };
    if code.check_validity(Flags::NONE, &requirement).is_ok() {
        return;
    }
    note!(
        "[keychain] this build is ad-hoc signed, so the saved login will not survive the \
         next rebuild — the account stops appearing and macOS asks for your system \
         password instead. Run it through scripts/signed-run (`cargo run`) or \
         scripts/bundle to sign it with a stable identity."
    );
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

/// The login keychain, which is what every caller outside the tests wants.
struct System;

impl Vault for System {
    fn get(&self) -> Result<Vec<u8>, Error> {
        get_generic_password(SERVICE, ACCOUNT)
    }
    fn set(&self, value: &[u8]) -> Result<(), Error> {
        set_generic_password(SERVICE, ACCOUNT, value)
    }
    fn delete(&self) -> Result<(), Error> {
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
                Some(denial) => note!("[keychain] {}", denial.help()),
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
    fn only_a_refusal_is_worth_a_signature_explanation() {
        // Cancel and a locked screen are not this program's fault, and telling
        // someone to read scripts/signed-run about either wastes their time.
        assert!(Denial::Refused.help().contains("code signature"));
        for denial in [Denial::Canceled, Denial::NoPrompt] {
            assert!(!denial.help().contains("code signature"), "{denial:?}");
        }
        assert_eq!(Denial::of(&Error::from_code(ITEM_NOT_FOUND)), None);
    }

    #[test]
    fn an_unreadable_item_is_discarded_rather_than_failing_every_launch() {
        let vault = Fake::default();
        *vault.item.borrow_mut() = Some(b"not json".to_vec());
        assert!(load_from(&vault).is_none());
        assert_eq!(vault.calls(), ["get", "delete"]);
        assert!(vault.item.borrow().is_none());
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
