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

use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
use serde::{Deserialize, Serialize};

/// Shown in Keychain Access as the item's name, so it says what it is.
const SERVICE: &str = "gwnative (Guild Wars)";
const ACCOUNT: &str = "login";

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
    let raw = get_generic_password(SERVICE, ACCOUNT).ok()?;
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
    set_generic_password(SERVICE, ACCOUNT, &encoded).map_err(|e| e.to_string())
}

/// Deleting what was never there is the caller's intended end state, so a
/// missing item is not an error worth reporting.
pub fn clear() {
    let _ = delete_generic_password(SERVICE, ACCOUNT);
}
