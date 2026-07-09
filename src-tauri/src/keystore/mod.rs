//! macOS Keychain storage for the SQLCipher database key.
//!
//! On first run a 32-byte random key is generated with `OsRng` and stored as
//! a generic password in the user's login keychain (service
//! `co.elastic.lilnote`, account `db-key`). Subsequent launches retrieve it.
//! If the key is deleted from the Keychain, the encrypted DB becomes
//! unreadable — which is the correct privacy behavior (effectively a factory
//! reset of all meeting/voiceprint data).

use security_framework::passwords::{get_generic_password, set_generic_password};

const SERVICE: &str = "co.elastic.lilnote";
const ACCOUNT: &str = "db-key";

/// Get the 32-byte DB key from the Keychain, generating and storing it on
/// first run.
pub fn db_key() -> Result<Vec<u8>, String> {
    match get_generic_password(SERVICE, ACCOUNT) {
        Ok(key) if key.len() == 32 => Ok(key),
        Ok(_) => Err("stored DB key is not 32 bytes — delete it and relaunch".into()),
        Err(_) => {
            // Not found (or inaccessible) → generate + store.
            let mut key = [0u8; 32];
            use rand::RngCore;
            rand::rngs::OsRng.fill_bytes(&mut key);
            set_generic_password(SERVICE, ACCOUNT, &key)
                .map_err(|e| format!("cannot store DB key in Keychain: {e}"))?;
            Ok(key.to_vec())
        }
    }
}