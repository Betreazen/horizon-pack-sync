//! Who may download, and which build the game server runs.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::codes::{hash_code, new_code};

const CODES_FILE: &str = "codes.json";
const STATE_FILE: &str = "state.json";
const MAX_NAME_LENGTH: usize = 64;

#[derive(thiserror::Error, Debug)]
#[error("{0}")]
pub struct StoreError(pub String);

impl StoreError {
    fn io(what: &str, path: &Path, error: impl std::fmt::Display) -> Self {
        Self(format!("cannot {what} {}: {error}", path.display()))
    }
}

/// What the API needs to know. The hub keeps it in files; a site keeps it with its accounts.
#[async_trait]
pub trait AccessStore: Send + Sync {
    /// Whether this hash belongs to a player's current code.
    async fn is_player(&self, code_hash: &str) -> Result<bool, StoreError>;
    /// The build the game server last confirmed it runs.
    async fn confirmed(&self) -> Result<Option<i64>, StoreError>;
    async fn set_confirmed(&self, sequence: i64) -> Result<(), StoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Player {
    pub name: String,
    pub code_hash: String,
    /// Seconds since 1970.
    pub created: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Codes {
    players: Vec<Player>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    confirmed: Option<i64>,
}

/// Two small JSON files in one folder. They are read on every request, so codes added or
/// revoked from the command line take effect without a restart.
pub struct FileStore {
    dir: PathBuf,
    writing: Mutex<()>,
}

impl FileStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            writing: Mutex::new(()),
        }
    }

    async fn read<T: for<'de> Deserialize<'de> + Default>(
        &self,
        file: &str,
    ) -> Result<T, StoreError> {
        let path = self.dir.join(file);
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| StoreError::io("parse", &path, e))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
            Err(error) => Err(StoreError::io("read", &path, error)),
        }
    }

    async fn write<T: Serialize>(&self, file: &str, value: &T) -> Result<(), StoreError> {
        let path = self.dir.join(file);
        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(|e| StoreError::io("create", &self.dir, e))?;
        let text =
            serde_json::to_vec_pretty(value).map_err(|e| StoreError::io("encode", &path, e))?;
        let temporary = path.with_extension("tmp");
        tokio::fs::write(&temporary, text)
            .await
            .map_err(|e| StoreError::io("write", &temporary, e))?;
        tokio::fs::rename(&temporary, &path)
            .await
            .map_err(|e| StoreError::io("replace", &path, e))
    }

    /// Issues a code for `name`, cancelling the player's previous one, and returns it once.
    pub async fn add_player(&self, name: &str) -> Result<String, StoreError> {
        let name = name.trim();
        if name.is_empty()
            || name.chars().count() > MAX_NAME_LENGTH
            || name.chars().any(char::is_control)
        {
            return Err(StoreError(format!(
                "a name needs 1 to {MAX_NAME_LENGTH} printable characters"
            )));
        }
        let _guard = self.writing.lock().await;
        let mut codes: Codes = self.read(CODES_FILE).await?;
        let code = new_code();
        codes.players.retain(|player| player.name != name);
        codes.players.push(Player {
            name: name.to_owned(),
            code_hash: hash_code(&code),
            created: now(),
        });
        self.write(CODES_FILE, &codes).await?;
        Ok(code)
    }

    /// Whether a player of that name had a code.
    pub async fn revoke_player(&self, name: &str) -> Result<bool, StoreError> {
        let _guard = self.writing.lock().await;
        let mut codes: Codes = self.read(CODES_FILE).await?;
        let before = codes.players.len();
        codes.players.retain(|player| player.name != name.trim());
        if codes.players.len() == before {
            return Ok(false);
        }
        self.write(CODES_FILE, &codes).await?;
        Ok(true)
    }

    pub async fn players(&self) -> Result<Vec<Player>, StoreError> {
        Ok(self.read::<Codes>(CODES_FILE).await?.players)
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

#[async_trait]
impl AccessStore for FileStore {
    async fn is_player(&self, code_hash: &str) -> Result<bool, StoreError> {
        let codes: Codes = self.read(CODES_FILE).await?;
        Ok(codes
            .players
            .iter()
            .any(|player| player.code_hash == code_hash))
    }

    async fn confirmed(&self) -> Result<Option<i64>, StoreError> {
        Ok(self.read::<State>(STATE_FILE).await?.confirmed)
    }

    async fn set_confirmed(&self, sequence: i64) -> Result<(), StoreError> {
        let _guard = self.writing.lock().await;
        self.write(
            STATE_FILE,
            &State {
                confirmed: Some(sequence),
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_new_code_works_and_replaces_the_old_one() {
        let dir = tempfile::tempdir().expect("dir");
        let store = FileStore::new(dir.path());
        let old = store.add_player("alex").await.expect("code");
        let new = store.add_player("alex").await.expect("code");
        assert!(!store.is_player(&hash_code(&old)).await.expect("read"));
        assert!(store.is_player(&hash_code(&new)).await.expect("read"));
        assert_eq!(store.players().await.expect("read").len(), 1);
    }

    #[tokio::test]
    async fn a_revoked_code_stops_working() {
        let dir = tempfile::tempdir().expect("dir");
        let store = FileStore::new(dir.path());
        let code = store.add_player("alex").await.expect("code");
        assert!(store.revoke_player("alex").await.expect("revoke"));
        assert!(!store.is_player(&hash_code(&code)).await.expect("read"));
        assert!(!store.revoke_player("alex").await.expect("revoke"));
    }

    #[tokio::test]
    async fn codes_are_stored_only_as_hashes() {
        let dir = tempfile::tempdir().expect("dir");
        let store = FileStore::new(dir.path());
        let code = store.add_player("alex").await.expect("code");
        let file = std::fs::read_to_string(dir.path().join(CODES_FILE)).expect("file");
        assert!(!file.contains(&code));
    }

    #[tokio::test]
    async fn the_confirmed_build_survives_a_new_store() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(
            FileStore::new(dir.path()).confirmed().await.expect("read"),
            None
        );
        FileStore::new(dir.path())
            .set_confirmed(4)
            .await
            .expect("write");
        assert_eq!(
            FileStore::new(dir.path()).confirmed().await.expect("read"),
            Some(4)
        );
    }

    #[tokio::test]
    async fn an_empty_or_control_name_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let store = FileStore::new(dir.path());
        assert!(store.add_player("  ").await.is_err());
        assert!(store.add_player("a\nb").await.is_err());
    }
}
