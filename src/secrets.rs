//! API keys in the operating system's keychain instead of a plain-text file.
//!
//! The webview can store and delete a key but never read one back. The backend gets its keys from the environment the
//! shell starts it with, so a key is only ever in memory in this process and the backend.

use std::path::Path;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;

const SERVICE: &str = "com.dagsterlabs.designer";

/// The environment variables whose values are kept in the keychain. Anything else stays in the backend's `.env`.
pub const SECRET_ENV_VARS: &[&str] = &["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "LOCAL_LLM_API_KEY"];

pub fn is_secret_name(name: &str) -> bool {
    SECRET_ENV_VARS.contains(&name)
}

/// Somewhere secrets can live. The real one is the OS keychain; tests use memory.
pub trait SecretStore {
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
    fn delete(&self, name: &str) -> Result<(), String>;
}

pub struct KeychainStore;

impl KeychainStore {
    fn entry(name: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, name).map_err(|e| e.to_string())
    }
}

impl SecretStore for KeychainStore {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        match Self::entry(name)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        Self::entry(name)?.set_password(value).map_err(|e| e.to_string())
    }
    fn delete(&self, name: &str) -> Result<(), String> {
        match Self::entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

impl KeychainStore {
    /// Store a value, or remove the entry when it is blank, so the keychain never holds an empty "key".
    pub fn set_checked(&self, name: &str, value: &str) -> Result<(), String> {
        if value.trim().is_empty() {
            self.delete(name)
        } else {
            self.set(name, value)
        }
    }
}

/// Whether the keychain actually works here (a Linux box without a secret service, for one, won't): write, read back, delete.
pub fn keychain_works(store: &dyn SecretStore) -> bool {
    let probe = "__designer_probe__";
    let ok = store.set(probe, "1").is_ok() && matches!(store.get(probe), Ok(Some(v)) if v == "1");
    let _ = store.delete(probe);
    ok
}

/// Every secret in the store, as environment variables for the backend. Unreadable or missing entries are skipped.
pub fn env_for_backend(store: &dyn SecretStore) -> Vec<(String, String)> {
    SECRET_ENV_VARS
        .iter()
        .filter_map(|name| match store.get(name) {
            Ok(Some(v)) if !v.is_empty() => Some((name.to_string(), v)),
            _ => None,
        })
        .collect()
}

fn parse_env_line(line: &str) -> Option<(&str, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim_start();
    let (key, value) = trimmed.split_once('=')?;
    let value = value.trim();
    let unquoted = match (value.chars().next(), value.chars().last()) {
        (Some(a), Some(b)) if value.len() >= 2 && (a == '\'' || a == '"') && a == b => &value[1..value.len() - 1],
        _ => value,
    };
    Some((key.trim(), unquoted.to_string()))
}

/// Move the secrets in a `.env` file into the store, then drop them from the file. Returns the names that moved.
///
/// A line is only removed once its value is safely in the store, so a keychain that fails leaves the file as it was. When
/// the store already has a value, the store wins (it is the newer one) and the file's copy is just dropped. Other lines,
/// comments and ordering are left alone.
pub fn migrate_env_file(path: &Path, store: &dyn SecretStore) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let mut moved = vec![];
    let mut kept: Vec<&str> = vec![];
    for line in text.lines() {
        match parse_env_line(line) {
            Some((key, value)) if is_secret_name(key) => {
                let safe = if matches!(store.get(key), Ok(Some(_))) {
                    true
                } else if value.is_empty() {
                    true // nothing worth keeping
                } else {
                    store.set(key, &value).is_ok()
                };
                if safe {
                    moved.push(key.to_string());
                } else {
                    kept.push(line);
                }
            }
            _ => kept.push(line),
        }
    }
    if moved.is_empty() {
        return moved;
    }
    let mut out = kept.join("\n");
    if text.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    if std::fs::write(path, out).is_err() {
        return vec![];
    }
    moved
}

/// In-memory store for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

#[cfg(test)]
impl SecretStore for MemoryStore {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        self.0.lock().unwrap().insert(name.into(), value.into());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), String> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BrokenStore;
    impl SecretStore for BrokenStore {
        fn get(&self, _: &str) -> Result<Option<String>, String> { Err("locked".into()) }
        fn set(&self, _: &str, _: &str) -> Result<(), String> { Err("locked".into()) }
        fn delete(&self, _: &str) -> Result<(), String> { Err("locked".into()) }
    }

    fn env_file(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn only_the_listed_names_are_secrets() {
        assert!(is_secret_name("OPENAI_API_KEY") && is_secret_name("ANTHROPIC_API_KEY") && is_secret_name("LOCAL_LLM_API_KEY"));
        assert!(!is_secret_name("LOCAL_LLM_BASE_URL") && !is_secret_name("PATH") && !is_secret_name("ANTHROPIC_WORKSPACE_ID"));
    }

    #[test]
    fn migration_moves_secrets_and_leaves_everything_else() {
        let (_d, path) = env_file("# keys\nOPENAI_API_KEY='sk-1'\nLOCAL_LLM_BASE_URL='http://localhost:11434'\nANTHROPIC_API_KEY=\"sk-ant\"\n\nOTHER=1\n");
        let store = MemoryStore::default();
        let moved = migrate_env_file(&path, &store);
        assert_eq!(moved, vec!["OPENAI_API_KEY", "ANTHROPIC_API_KEY"]);
        assert_eq!(store.get("OPENAI_API_KEY").unwrap().as_deref(), Some("sk-1"));
        assert_eq!(store.get("ANTHROPIC_API_KEY").unwrap().as_deref(), Some("sk-ant"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# keys\nLOCAL_LLM_BASE_URL='http://localhost:11434'\n\nOTHER=1\n");
    }

    #[test]
    fn migration_understands_export_and_unquoted_values() {
        let (_d, path) = env_file("export OPENAI_API_KEY=sk-plain\n");
        let store = MemoryStore::default();
        assert_eq!(migrate_env_file(&path, &store), vec!["OPENAI_API_KEY"]);
        assert_eq!(store.get("OPENAI_API_KEY").unwrap().as_deref(), Some("sk-plain"));
    }

    #[test]
    fn a_broken_keychain_leaves_the_file_untouched() {
        let original = "OPENAI_API_KEY='sk-1'\nOTHER=1\n";
        let (_d, path) = env_file(original);
        assert!(migrate_env_file(&path, &BrokenStore).is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn the_keychains_newer_value_wins_over_the_files_older_one() {
        let (_d, path) = env_file("OPENAI_API_KEY='old-from-file'\n");
        let store = MemoryStore::default();
        store.set("OPENAI_API_KEY", "newer-in-keychain").unwrap();
        assert_eq!(migrate_env_file(&path, &store), vec!["OPENAI_API_KEY"]);
        assert_eq!(store.get("OPENAI_API_KEY").unwrap().as_deref(), Some("newer-in-keychain"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn no_file_or_no_secrets_changes_nothing() {
        let store = MemoryStore::default();
        assert!(migrate_env_file(Path::new("/definitely/not/here/.env"), &store).is_empty());
        let (_d, path) = env_file("A=1\n# OPENAI_API_KEY=commented-out\n");
        assert!(migrate_env_file(&path, &store).is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A=1\n# OPENAI_API_KEY=commented-out\n");
    }

    #[test]
    fn an_empty_value_is_dropped_without_storing_it() {
        let (_d, path) = env_file("OPENAI_API_KEY=''\nOTHER=1\n");
        let store = MemoryStore::default();
        assert_eq!(migrate_env_file(&path, &store), vec!["OPENAI_API_KEY"]);
        assert_eq!(store.get("OPENAI_API_KEY").unwrap(), None);
    }

    #[test]
    fn running_the_migration_twice_is_harmless() {
        let (_d, path) = env_file("OPENAI_API_KEY='sk-1'\n");
        let store = MemoryStore::default();
        assert_eq!(migrate_env_file(&path, &store).len(), 1);
        assert!(migrate_env_file(&path, &store).is_empty());
        assert_eq!(store.get("OPENAI_API_KEY").unwrap().as_deref(), Some("sk-1"));
    }

    #[test]
    fn the_backend_environment_has_only_what_is_stored() {
        let store = MemoryStore::default();
        store.set("OPENAI_API_KEY", "sk-1").unwrap();
        store.set("LOCAL_LLM_API_KEY", "").unwrap();
        store.set("NOT_ON_THE_LIST", "x").unwrap();
        assert_eq!(env_for_backend(&store), vec![("OPENAI_API_KEY".to_string(), "sk-1".to_string())]);
        assert!(env_for_backend(&BrokenStore).is_empty());
    }

    /// Talks to the real OS keychain, using a throwaway name (never an API key). Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn the_real_keychain_round_trips_a_value() {
        let store = KeychainStore;
        let name = "__designer_test_entry__";
        store.set(name, "value-1").unwrap();
        assert_eq!(store.get(name).unwrap().as_deref(), Some("value-1"));
        store.set(name, "value-2").unwrap();
        assert_eq!(store.get(name).unwrap().as_deref(), Some("value-2"));
        store.delete(name).unwrap();
        assert_eq!(store.get(name).unwrap(), None);
        store.delete(name).unwrap(); // deleting what isn't there is fine
        assert!(keychain_works(&store));
    }

    #[test]
    fn the_keychain_probe_detects_a_working_and_a_broken_store() {
        assert!(keychain_works(&MemoryStore::default()));
        assert!(!keychain_works(&BrokenStore));
    }
}
