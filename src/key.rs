//! The key of the store (SQLCipher), and where it lives.
//!
//! A random secret kept in the OS keyring (Secret Service on Linux, Keychain on macOS,
//! Credential Manager on Windows), or `$GMESSAGES_TUI_PASSPHRASE`.

use std::error::Error;
use std::fmt;
use std::path::Path;

const KEYRING_SERVICE: &str = "gmessages-tui";
const PASSPHRASE_VAR: &str = "GMESSAGES_TUI_PASSPHRASE";

#[derive(Debug)]
pub enum KeyError {
    Empty,
    Locked(String),
    Missing(String),
    Refused(String),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "{PASSPHRASE_VAR} is empty"),
            Self::Locked(reason) => write!(f, "keyring locked: {reason}"),
            Self::Missing(reason) => write!(f, "store key missing: {reason}"),
            Self::Refused(reason) => write!(f, "cannot create store key: {reason}"),
        }
    }
}

impl Error for KeyError {}

fn keyring_error(error: keyring::Error) -> KeyError {
    match error {
        keyring::Error::NoStorageAccess(_) => KeyError::Locked(error.to_string()),
        _ => KeyError::Missing(error.to_string()),
    }
}

/// The key of the store at `path`: `$GMESSAGES_TUI_PASSPHRASE`, else its keyring entry
/// (a new random key if there is none and no store yet).
pub fn store_key(path: &Path) -> Result<String, KeyError> {
    if let Ok(passphrase) = std::env::var(PASSPHRASE_VAR) {
        return if passphrase.is_empty() {
            Err(KeyError::Empty)
        } else {
            Ok(passphrase)
        };
    }
    let exists = path.exists();
    let user = path.to_string_lossy();
    let entry = keyring::Entry::new(KEYRING_SERVICE, &user).map_err(keyring_error)?;
    match entry.get_password() {
        Ok(key) => Ok(key),
        Err(keyring::Error::NoEntry) if exists => Err(KeyError::Missing(
            "no keyring entry for existing store".into(),
        )),
        Err(keyring::Error::NoEntry) => {
            let key = random_key()?;
            entry
                .set_password(&key)
                .map_err(|e| KeyError::Refused(e.to_string()))?;
            Ok(key)
        }
        Err(e) => Err(keyring_error(e)),
    }
}

/// Deletes the keyring entry of the store at `path`, if any.
pub fn forget(path: &Path) -> Result<(), Box<dyn Error>> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, &path.to_string_lossy())?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("cannot delete the store key from the keyring: {e}").into()),
    }
}

fn random_key() -> Result<String, KeyError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| KeyError::Refused(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_passphrase_is_validated_before_keyring_access() {
        let previous = std::env::var_os(PASSPHRASE_VAR);
        let path = Path::new("/nonexistent/store-key-test");

        // No other test reads this variable; keep both cases in one test.
        unsafe { std::env::set_var(PASSPHRASE_VAR, "") };
        assert!(matches!(store_key(path), Err(KeyError::Empty)));
        unsafe { std::env::set_var(PASSPHRASE_VAR, "test passphrase") };
        assert_eq!(store_key(path).unwrap(), "test passphrase");

        if let Some(value) = previous {
            unsafe { std::env::set_var(PASSPHRASE_VAR, value) };
        } else {
            unsafe { std::env::remove_var(PASSPHRASE_VAR) };
        }
    }
}
