//! The operating system's credential store, through the `keyring` crate: Keychain Services on macOS, the
//! Credential Manager on Windows, and the Secret Service (D-Bus) on Linux and the BSDs.
//!
//! Only opened when asked ([`Keychain::open`]); nothing here runs in tests against the real store. Callers
//! fall back to another store when [`Keychain::open`] fails (no D-Bus session, no keyring daemon, locked or
//! unsupported platform).

/// Longest key accepted (the stores limit attribute lengths).
const MAX_KEY_CHARS: usize = 255;

/// A handle on the OS credential store under one service name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keychain {
    service: String,
}

fn err(e: &keyring::Error) -> String {
    e.to_string()
}

impl Keychain {
    /// Opens the platform store for `service` and checks that it answers (a lookup of an entry that does not
    /// exist must report "no entry", not a failure).
    pub fn open(service: &str) -> Result<Keychain, String> {
        if service.trim().is_empty() || service.chars().count() > MAX_KEY_CHARS {
            return Err("invalid keychain service name".into());
        }
        if let Err(e) = keyring::Entry::store_status() {
            return Err(format!("no credential store: {}", err(e)));
        }
        let k = Keychain { service: service.to_owned() };
        k.get("mailcraft-probe/does-not-exist")?;
        Ok(k)
    }

    /// The store's name for Account Settings.
    pub fn store_name() -> &'static str {
        if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else if cfg!(target_os = "windows") {
            "Windows Credential Manager"
        } else {
            "Secret Service keyring"
        }
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, String> {
        if key.is_empty() || key.chars().count() > MAX_KEY_CHARS || key.chars().any(char::is_control) {
            return Err("invalid keychain key".into());
        }
        keyring::Entry::new(&self.service, key).map_err(|e| err(&e))
    }

    /// The secret stored under `key`, `None` when there is none.
    pub fn get(&self, key: &str) -> Result<Option<String>, String> {
        match self.entry(key)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(err(&e)),
        }
    }

    /// Stores (or replaces) the secret under `key`.
    pub fn set(&self, key: &str, secret: &str) -> Result<(), String> {
        self.entry(key)?.set_password(secret).map_err(|e| err(&e))
    }

    /// Deletes the secret under `key` (no error when there is none).
    pub fn remove(&self, key: &str) -> Result<(), String> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(err(&e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_names_without_touching_the_store() {
        assert!(Keychain::open("").is_err());
        assert!(Keychain::open(&"x".repeat(300)).is_err());
        let k = Keychain { service: "MailCraft test".into() };
        assert!(k.entry("").is_err());
        assert!(k.entry("a\nb").is_err());
        assert!(!Keychain::store_name().is_empty());
        assert_eq!(k.service(), "MailCraft test");
    }
}
