//! macOS Keychain storage for the SQLCipher database key.
//!
//! On first run a 32-byte random key is generated with `OsRng` and stored as
//! a generic password in the user's login keychain (service
//! `com.lilnotes`, account `db-key`). Subsequent launches retrieve it.
//! If the key is deleted from the Keychain, the encrypted DB becomes
//! unreadable — which is the correct privacy behavior (effectively a factory
//! reset of all meeting/voiceprint data).

use rand::RngCore;
use security_framework::passwords::{get_generic_password, set_generic_password};
use security_framework_sys::base::errSecItemNotFound;

const SERVICE: &str = "com.lilnotes";
const ACCOUNT: &str = "db-key";

/// Get the 32-byte DB key from the Keychain, generating and storing it on
/// first run.
///
/// Only the `errSecItemNotFound` (OSStatus -25300) case is treated as
/// "first run" and allowed to regenerate a key. Any other Keychain error
/// (access denied, keychain locked, corrupted entry) is propagated: if we
/// regenerated in those cases, the new key would be unable to decrypt the
/// existing encrypted database — silent, unrecoverable data loss.
pub fn db_key() -> Result<Vec<u8>, String> {
    match get_generic_password(SERVICE, ACCOUNT) {
        Ok(key) if key.len() == 32 => Ok(key),
        Ok(_) => Err("stored DB key is not 32 bytes — delete it and relaunch".into()),
        Err(e) if e.code() == errSecItemNotFound => {
            // Genuinely first run — no key yet. Generate + store.
            let mut key = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut key);
            set_generic_password(SERVICE, ACCOUNT, &key)
                .map_err(|e| format!("cannot store DB key in Keychain: {e}"))?;
            Ok(key.to_vec())
        }
        Err(e) => Err(format!("cannot read DB key from Keychain: {e}")),
    }
}
