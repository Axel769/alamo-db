//! Passwords in the system keychain (§12): `keyring` crate — macOS Keychain,
//! Windows Credential Manager. Never on disk in clear text, never in SQLite.
//!
//! The service name is **derived from the Tauri identifier** (§13.1), never
//! typed by hand: on macOS access is tied to the signature more than to the
//! name, but the name remains the key by which the user finds the entry in
//! Keychain Access. The account is derived from the profile's random tag
//! (`storage`, migration 7), or from the id for legacy profiles: it survives
//! a rename, changes with the server and dies (explicitly, `forget`) when
//! the profile is deleted. Two entries per profile: database password and
//! SSH credential (bastion password or key passphrase, §12).

use keyring::Entry;

/// Account of the database password. `tag` = profile tag; `None` = legacy
/// profile, entry by id. The tag is 32 hex digits: it never matches an id.
pub fn db_account(profile_id: i64, tag: Option<&str>) -> String {
    match tag {
        Some(t) => format!("profilo-{t}"),
        None => format!("profilo-{profile_id}"),
    }
}

/// Account of the SSH credential (password or passphrase, `auth` decides):
/// tied to the SSH PROFILE (migration 5), not to the database profile — the
/// bastion is shared and so is its credential. New `ssh-` namespace on
/// purpose: the `profilo-N-ssh` entries of the inline era (migration 4) must
/// not match by id coincidence.
pub fn ssh_account(ssh_profile_id: i64, tag: Option<&str>) -> String {
    match tag {
        Some(t) => format!("ssh-{t}"),
        None => format!("ssh-{ssh_profile_id}"),
    }
}

fn entry(service: &str, account: &str) -> Result<Entry, String> {
    Entry::new(service, account).map_err(|e| format!("keychain: {e}"))
}

/// Saves (or overwrites) the entry. Call it only after a successful
/// connection: a wrong credential in the keychain is an error that comes
/// back on every connect.
pub fn save(service: &str, account: &str, password: &str) -> Result<(), String> {
    entry(service, account)?
        .set_password(password)
        .map_err(|e| format!("keychain: {e}"))
}

/// `None` = no saved entry (not an error: the dialog asks).
pub fn get(service: &str, account: &str) -> Result<Option<String>, String> {
    match entry(service, account)?.get_password() {
        Ok(p) => Ok(Some(p)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("keychain: {e}")),
    }
}

/// Is there a saved entry? On macOS only the attributes are searched, without
/// reading the secret: reading it asks for Keychain permission for every app
/// not yet authorized, and the connection dialog asks only to know whether
/// to tick the checkbox.
#[cfg(target_os = "macos")]
pub fn exists(service: &str, account: &str) -> Result<bool, String> {
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
    // errSecItemNotFound
    const NOT_FOUND: i32 = -25300;
    match ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(service)
        .account(account)
        .load_attributes(true)
        .limit(Limit::Max(1))
        .search()
    {
        Ok(found) => Ok(!found.is_empty()),
        Err(e) if e.code() == NOT_FOUND => Ok(false),
        Err(e) => Err(format!("keychain: {e}")),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn exists(service: &str, account: &str) -> Result<bool, String> {
    get(service, account).map(|p| p.is_some())
}

/// Removes the entry; its absence is not an error (idempotent — profile
/// deletion calls it too).
pub fn forget(service: &str, account: &str) -> Result<(), String> {
    match entry(service, account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("keychain: {e}")),
    }
}
