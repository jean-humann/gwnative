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

/// The three ways the keychain says "not for you": the ACL refused, the person
/// dismissed the prompt, or there was no way to put a prompt on screen. They
/// are one condition as far as this module is concerned.
const AUTH_FAILED: i32 = -25293;
const USER_CANCELED: i32 = -128;
const INTERACTION_NOT_ALLOWED: i32 = -25308;

fn denied(e: &Error) -> bool {
    matches!(
        e.code(),
        AUTH_FAILED | USER_CANCELED | INTERACTION_NOT_ALLOWED
    )
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
    eprintln!(
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

pub fn load() -> Option<Credentials> {
    let raw = match get_generic_password(SERVICE, ACCOUNT) {
        Ok(raw) => raw,
        Err(e) if e.code() == ITEM_NOT_FOUND => return None,
        Err(e) if denied(&e) => {
            eprintln!("[keychain] {DENIED_HELP}");
            return None;
        }
        // Anything else is unexpected rather than explicable, so pass the
        // system's own wording through instead of guessing at a cause.
        Err(e) => {
            eprintln!("[keychain] could not read the saved login: {e}");
            return None;
        }
    };
    match serde_json::from_slice::<Credentials>(&raw) {
        Ok(credentials) => Some(credentials),
        // An item that will not parse is one this app cannot use, and leaving it
        // in place would fail the same way on every launch.
        Err(e) => {
            eprintln!("[keychain] stored login is unreadable ({e}); discarding it");
            clear();
            None
        }
    }
}

pub fn store(credentials: &Credentials) -> Result<(), String> {
    if credentials.username.len() > MAX_FIELD || credentials.password.len() > MAX_FIELD {
        return Err("credentials are too long to store".into());
    }
    let encoded = serde_json::to_vec(credentials).map_err(|e| e.to_string())?;
    match set_generic_password(SERVICE, ACCOUNT, &encoded) {
        Ok(()) => Ok(()),
        // Saving over an existing item is an update, and an update asks the
        // same permission a read does. So an item left by a differently signed
        // build refuses the overwrite too, and signing in again — the one thing
        // that would fix it — is exactly what cannot happen. Replacing it does
        // work: removing an item does not require opening it, so the delete
        // goes through where the update would not, and adding it back is an
        // ordinary create that records this build as the owner. Once only; a
        // second refusal is a real failure and says what to remove by hand.
        Err(e) if denied(&e) => {
            if let Err(e) = delete_generic_password(SERVICE, ACCOUNT) {
                return Err(format!(
                    "the saved login belongs to a differently signed build and could not \
                     be replaced ({e}); delete \"{SERVICE}\" in Keychain Access, then sign \
                     in again"
                ));
            }
            set_generic_password(SERVICE, ACCOUNT, &encoded).map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Deleting what was never there is the caller's intended end state, so a
/// missing item is not an error worth reporting.
pub fn clear() {
    let _ = delete_generic_password(SERVICE, ACCOUNT);
}
