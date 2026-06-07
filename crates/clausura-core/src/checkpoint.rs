use crate::types::{CheckpointError, Message, SnapshotMeta};
use rusqlite::{params, Connection};
use std::path::PathBuf;

/// SQLite-backed checkpoint store for agent state persistence.
pub struct CheckpointStore {
    conn: Connection,
    db_path: PathBuf,
}

impl CheckpointStore {
    /// Open or create the database at `~/.clausura/checkpoints.db`
    pub fn new() -> Result<Self, CheckpointError> {
        let clausura_dir = dirs::home_dir()
            .ok_or_else(|| CheckpointError::DbError("Could not find home directory".into()))?
            .join(".clausura");
        std::fs::create_dir_all(&clausura_dir)
            .map_err(|e| CheckpointError::DbError(format!("Failed to create dir: {}", e)))?;
        let db_path = clausura_dir.join("checkpoints.db");
        Self::open_at(db_path)
    }

    /// Open or create the database at a specific path
    pub fn open_at(db_path: PathBuf) -> Result<Self, CheckpointError> {
        let conn = Connection::open(&db_path)
            .map_err(|e| CheckpointError::DbError(format!("Failed to open DB: {}", e)))?;

        // Enable WAL mode for concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(|e| CheckpointError::DbError(format!("Failed pragma: {}", e)))?;

        // Create the schema if it doesn't exist
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS checkpoints (
                thread_id    TEXT NOT NULL,
                checkpoint_id TEXT NOT NULL PRIMARY KEY,
                created_at   TEXT NOT NULL DEFAULT (datetime('now')),
                version      INTEGER NOT NULL DEFAULT 1,
                truncated    INTEGER NOT NULL DEFAULT 0,
                state        BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_thread_time
                ON checkpoints(thread_id, created_at DESC);",
        )
        .map_err(|e| CheckpointError::DbError(format!("Failed schema: {}", e)))?;

        Ok(Self { conn, db_path })
    }

    /// Save messages as a new checkpoint for the given thread.
    pub fn save(
        &self,
        thread_id: &str,
        messages: &[Message],
        truncated: bool,
    ) -> Result<uuid::Uuid, CheckpointError> {
        let checkpoint_id = uuid::Uuid::new_v4();
        let state = rmp_serde::to_vec(messages)
            .map_err(|e| CheckpointError::SerializationError(e.to_string()))?;

        self.conn
            .execute(
                "INSERT INTO checkpoints (thread_id, checkpoint_id, state, version, truncated)
                 VALUES (?1, ?2, ?3, 1, ?4)",
                params![
                    thread_id,
                    checkpoint_id.to_string(),
                    state,
                    truncated as i32
                ],
            )
            .map_err(|e| CheckpointError::DbError(format!("Insert failed: {}", e)))?;

        Ok(checkpoint_id)
    }

    /// Load the most recent checkpoint for a thread.
    #[allow(clippy::type_complexity)]
    pub fn load(
        &self,
        thread_id: &str,
    ) -> Result<Option<(uuid::Uuid, Vec<Message>, bool, u32)>, CheckpointError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT checkpoint_id, state, truncated, version FROM checkpoints
                 WHERE thread_id = ?1
                 ORDER BY created_at DESC, rowid DESC
                 LIMIT 1",
            )
            .map_err(|e| CheckpointError::DbError(e.to_string()))?;

        let result = stmt.query_row(params![thread_id], |row| {
            let id_str: String = row.get(0)?;
            let state_blob: Vec<u8> = row.get(1)?;
            let truncated: i32 = row.get(2)?;
            let version: i32 = row.get(3)?;
            let checkpoint_id = uuid::Uuid::parse_str(&id_str)
                .map_err(|_| rusqlite::Error::InvalidParameterName("Invalid UUID".into()))?;
            let messages: Vec<Message> = rmp_serde::from_slice(&state_blob).map_err(|e| {
                rusqlite::Error::InvalidParameterName(format!("Deserialize failed: {}", e))
            })?;
            Ok((checkpoint_id, messages, truncated != 0, version as u32))
        });

        match result {
            Ok(val) => Ok(Some(val)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(CheckpointError::DbError(e.to_string())),
        }
    }

    /// Load a specific checkpoint by ID.
    #[allow(clippy::type_complexity)]
    pub fn load_at(
        &self,
        thread_id: &str,
        checkpoint_id: &uuid::Uuid,
    ) -> Result<Option<(Vec<Message>, bool, u32)>, CheckpointError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT state, truncated, version FROM checkpoints
                 WHERE thread_id = ?1 AND checkpoint_id = ?2",
            )
            .map_err(|e| CheckpointError::DbError(e.to_string()))?;

        let result = stmt.query_row(params![thread_id, checkpoint_id.to_string()], |row| {
            let state_blob: Vec<u8> = row.get(0)?;
            let truncated: i32 = row.get(1)?;
            let version: i32 = row.get(2)?;
            let messages: Vec<Message> = rmp_serde::from_slice(&state_blob).map_err(|e| {
                rusqlite::Error::InvalidParameterName(format!("Deserialize failed: {}", e))
            })?;
            Ok((messages, truncated != 0, version as u32))
        });

        match result {
            Ok(val) => Ok(Some(val)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(CheckpointError::DbError(e.to_string())),
        }
    }

    /// List checkpoints for a thread (most recent first).
    pub fn list(&self, thread_id: &str, limit: u32) -> Result<Vec<SnapshotMeta>, CheckpointError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT checkpoint_id, created_at, version, truncated FROM checkpoints
                 WHERE thread_id = ?1
                 ORDER BY created_at DESC, rowid DESC
                 LIMIT ?2",
            )
            .map_err(|e| CheckpointError::DbError(e.to_string()))?;

        let metas = stmt
            .query_map(params![thread_id, limit], |row| {
                let id_str: String = row.get(0)?;
                let created_str: String = row.get(1)?;
                let version: i32 = row.get(2)?;
                let truncated: i32 = row.get(3)?;

                let checkpoint_id = uuid::Uuid::parse_str(&id_str).unwrap_or(uuid::Uuid::nil());
                // SQLite datetime('now') returns UTC in format "YYYY-MM-DD HH:MM:SS"
                let created_at =
                    chrono::NaiveDateTime::parse_from_str(&created_str, "%Y-%m-%d %H:%M:%S")
                        .map(|naive| {
                            chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
                                naive,
                                chrono::Utc,
                            )
                        })
                        .unwrap_or_else(|_| chrono::Utc::now());

                Ok(SnapshotMeta {
                    thread_id: thread_id.to_string(),
                    checkpoint_id,
                    created_at,
                    version: version as u32,
                    truncated: truncated != 0,
                })
            })
            .map_err(|e| CheckpointError::DbError(e.to_string()))?;

        let mut result = Vec::new();
        for meta in metas {
            result.push(meta.map_err(|e| CheckpointError::DbError(e.to_string()))?);
        }
        Ok(result)
    }

    /// List all checkpoints across all threads (most recent first).
    pub fn list_all(&self, limit: u32) -> Result<Vec<SnapshotMeta>, CheckpointError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT checkpoint_id, thread_id, created_at, version, truncated FROM checkpoints
                  ORDER BY created_at DESC, rowid DESC
                  LIMIT ?1",
            )
            .map_err(|e| CheckpointError::DbError(e.to_string()))?;

        let metas = stmt
            .query_map(params![limit], |row| {
                let id_str: String = row.get(0)?;
                let thread_str: String = row.get(1)?;
                let created_str: String = row.get(2)?;
                let version: i32 = row.get(3)?;
                let truncated: i32 = row.get(4)?;

                let checkpoint_id = uuid::Uuid::parse_str(&id_str).unwrap_or(uuid::Uuid::nil());
                let created_at =
                    chrono::NaiveDateTime::parse_from_str(&created_str, "%Y-%m-%d %H:%M:%S")
                        .map(|naive| {
                            chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
                                naive,
                                chrono::Utc,
                            )
                        })
                        .unwrap_or_else(|_| chrono::Utc::now());

                Ok(SnapshotMeta {
                    thread_id: thread_str,
                    checkpoint_id,
                    created_at,
                    version: version as u32,
                    truncated: truncated != 0,
                })
            })
            .map_err(|e| CheckpointError::DbError(e.to_string()))?;

        let mut result = Vec::new();
        for meta in metas {
            result.push(meta.map_err(|e| CheckpointError::DbError(e.to_string()))?);
        }
        Ok(result)
    }

    /// Delete a specific checkpoint by ID.
    pub fn delete_checkpoint(&self, checkpoint_id: &uuid::Uuid) -> Result<(), CheckpointError> {
        self.conn
            .execute(
                "DELETE FROM checkpoints WHERE checkpoint_id = ?1",
                params![checkpoint_id.to_string()],
            )
            .map_err(|e| CheckpointError::DbError(e.to_string()))?;
        Ok(())
    }

    /// Delete all checkpoints for a thread.
    pub fn delete_thread(&self, thread_id: &str) -> Result<(), CheckpointError> {
        self.conn
            .execute(
                "DELETE FROM checkpoints WHERE thread_id = ?1",
                params![thread_id],
            )
            .map_err(|e| CheckpointError::DbError(e.to_string()))?;
        Ok(())
    }

    /// Get the database path
    pub fn db_path(&self) -> &PathBuf {
        &self.db_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Role;
    use tempfile::TempDir;

    fn create_test_store() -> (CheckpointStore, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let store = CheckpointStore::open_at(db_path).unwrap();
        (store, tmp)
    }

    fn make_messages() -> Vec<Message> {
        vec![
            Message::new(Role::System, "You are a code reviewer."),
            Message::new(Role::User, "Review this code."),
            Message::new(Role::Assistant, "I found 3 issues."),
        ]
    }

    #[test]
    fn test_save_and_load() {
        let (store, _tmp) = create_test_store();
        let msgs = make_messages();
        let cid = store.save("test-thread", &msgs, false).unwrap();
        let loaded = store.load("test-thread").unwrap();
        assert!(loaded.is_some());
        let (loaded_cid, loaded_msgs, truncated, version) = loaded.unwrap();
        assert_eq!(version, 1);
        assert_eq!(cid, loaded_cid);
        assert_eq!(msgs, loaded_msgs);
        assert!(!truncated);
    }

    #[test]
    fn test_load_nonexistent_thread() {
        let (store, _tmp) = create_test_store();
        let loaded = store.load("ghost").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_load_at() {
        let (store, _tmp) = create_test_store();
        let msgs = make_messages();
        let cid = store.save("test", &msgs, false).unwrap();
        let loaded = store.load_at("test", &cid).unwrap();
        assert!(loaded.is_some());
        let (loaded_msgs, _, version) = loaded.unwrap();
        assert_eq!(version, 1);
        assert_eq!(msgs, loaded_msgs);
    }

    #[test]
    fn test_list() {
        let (store, _tmp) = create_test_store();
        store.save("test", &make_messages(), false).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.save("test", &make_messages(), true).unwrap();

        let list = store.list("test", 10).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].created_at >= list[1].created_at);
        assert!(list[0].truncated);
        assert!(!list[1].truncated);
    }

    #[test]
    fn test_delete_thread() {
        let (store, _tmp) = create_test_store();
        store.save("test", &make_messages(), false).unwrap();
        store.delete_thread("test").unwrap();
        let loaded = store.load("test").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_message_pack_round_trip() {
        let msgs = make_messages();
        let encoded = rmp_serde::to_vec(&msgs).unwrap();
        let decoded: Vec<Message> = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(msgs, decoded);
    }

    #[test]
    fn test_delete_checkpoint() {
        let (store, _tmp) = create_test_store();
        let cid = store.save("del-test", &make_messages(), false).unwrap();
        store.delete_checkpoint(&cid).unwrap();
        // Checkpoint should no longer exist
        let loaded = store.load_at("del-test", &cid).unwrap();
        assert!(loaded.is_none());
        // Thread should still have no checkpoints
        let list = store.list("del-test", 10).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn test_delete_checkpoint_does_not_affect_other_threads() {
        let (store, _tmp) = create_test_store();
        let cid_a = store.save("thread-a", &make_messages(), false).unwrap();
        store.save("thread-b", &make_messages(), false).unwrap();
        store.delete_checkpoint(&cid_a).unwrap();
        // thread-a should be empty
        let list_a = store.list("thread-a", 10).unwrap();
        assert!(list_a.is_empty());
        // thread-b should still have its checkpoint
        let list_b = store.list("thread-b", 10).unwrap();
        assert_eq!(list_b.len(), 1);
    }

    #[test]
    fn test_list_all() {
        let (store, _tmp) = create_test_store();
        store.save("alpha", &make_messages(), false).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.save("beta", &make_messages(), true).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.save("alpha", &make_messages(), false).unwrap();

        let all = store.list_all(10).unwrap();
        assert_eq!(all.len(), 3);
        // Most recent first
        assert_eq!(all[0].thread_id, "alpha");
        assert_eq!(all[1].thread_id, "beta");
        assert_eq!(all[2].thread_id, "alpha");
    }

    #[test]
    fn test_list_all_respects_limit() {
        let (store, _tmp) = create_test_store();
        for i in 0..5 {
            store
                .save(&format!("t{}", i), &make_messages(), false)
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let all = store.list_all(3).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_version_round_trip() {
        let (store, _tmp) = create_test_store();
        let cid = store.save("ver-test", &make_messages(), false).unwrap();
        let loaded = store.load("ver-test").unwrap().unwrap();
        assert_eq!(loaded.3, 1); // version field
        let loaded_at = store.load_at("ver-test", &cid).unwrap().unwrap();
        assert_eq!(loaded_at.2, 1); // version field from load_at
    }
}
