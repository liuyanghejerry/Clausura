use crate::checkpoint::CheckpointStore;
use crate::types::{CheckpointError, Message, Role, Snapshot, SnapshotMeta};

/// High-level snapshot manager for agent state persistence.
pub struct SnapshotManager {
    store: CheckpointStore,
    auto_save_interval: u32,
}

impl SnapshotManager {
    /// Create a new SnapshotManager with the given CheckpointStore.
    pub fn new(store: CheckpointStore) -> Self {
        Self {
            store,
            auto_save_interval: 3,
        }
    }

    /// Create with custom auto-save interval.
    pub fn with_auto_save_interval(store: CheckpointStore, interval: u32) -> Self {
        Self {
            store,
            auto_save_interval: interval,
        }
    }

    /// Save current state as a snapshot.
    pub fn save_snapshot(
        &self,
        thread_id: &str,
        messages: &[Message],
        truncated: bool,
    ) -> Result<uuid::Uuid, CheckpointError> {
        self.store.save(thread_id, messages, truncated)
    }

    /// Restore the most recent snapshot for a thread.
    /// If `resume` is true, injects a continuation message.
    pub fn restore_snapshot(
        &self,
        thread_id: &str,
        resume: bool,
    ) -> Result<Option<Snapshot>, CheckpointError> {
        let result = self.store.load(thread_id)?;
        match result {
            Some((checkpoint_id, mut messages, truncated, version)) => {
                if resume {
                    messages.push(Message::new(
                        Role::User,
                        "You were interrupted. Continue from where you left off.".to_string(),
                    ));
                }
                let meta = SnapshotMeta {
                    thread_id: thread_id.to_string(),
                    checkpoint_id,
                    created_at: chrono::Utc::now(),
                    version,
                    truncated,
                };
                Ok(Some(Snapshot {
                    id: checkpoint_id,
                    messages,
                    meta,
                }))
            }
            None => Ok(None),
        }
    }

    /// Restore a specific checkpoint by ID.
    pub fn restore_at(
        &self,
        thread_id: &str,
        checkpoint_id: &uuid::Uuid,
    ) -> Result<Option<Snapshot>, CheckpointError> {
        let result = self.store.load_at(thread_id, checkpoint_id)?;
        match result {
            Some((messages, truncated, version)) => {
                let meta = SnapshotMeta {
                    thread_id: thread_id.to_string(),
                    checkpoint_id: *checkpoint_id,
                    created_at: chrono::Utc::now(),
                    version,
                    truncated,
                };
                Ok(Some(Snapshot {
                    id: *checkpoint_id,
                    messages,
                    meta,
                }))
            }
            None => Ok(None),
        }
    }

    /// List snapshots for a thread.
    pub fn list_snapshots(
        &self,
        thread_id: &str,
        limit: u32,
    ) -> Result<Vec<SnapshotMeta>, CheckpointError> {
        self.store.list(thread_id, limit)
    }

    /// List snapshots across all threads.
    pub fn list_all_snapshots(&self, limit: u32) -> Result<Vec<SnapshotMeta>, CheckpointError> {
        self.store.list_all(limit)
    }

    /// Delete a specific checkpoint by ID.
    pub fn delete_checkpoint(&self, checkpoint_id: &uuid::Uuid) -> Result<(), CheckpointError> {
        self.store.delete_checkpoint(checkpoint_id)
    }

    /// Delete all snapshots for a thread.
    pub fn delete_thread(&self, thread_id: &str) -> Result<(), CheckpointError> {
        self.store.delete_thread(thread_id)
    }

    /// Check if auto-save should trigger based on iteration count.
    pub fn should_auto_save(&self, iteration: u32) -> bool {
        iteration > 0 && iteration.is_multiple_of(self.auto_save_interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Role;
    use tempfile::TempDir;

    fn setup_manager() -> (SnapshotManager, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("snapshots.db");
        let store = CheckpointStore::open_at(db_path).unwrap();
        let manager = SnapshotManager::new(store);
        (manager, tmp)
    }

    fn make_messages() -> Vec<Message> {
        vec![
            Message::new(Role::System, "System prompt"),
            Message::new(Role::User, "Review this code"),
            Message::new(Role::Assistant, "I found issues"),
        ]
    }

    #[test]
    fn test_save_and_restore_roundtrip() {
        let (manager, _tmp) = setup_manager();
        let msgs = make_messages();
        let cid = manager.save_snapshot("thread-1", &msgs, false).unwrap();

        let restored = manager.restore_snapshot("thread-1", false).unwrap();
        assert!(restored.is_some());
        let snapshot = restored.unwrap();
        assert_eq!(snapshot.id, cid);
        assert_eq!(snapshot.messages.len(), 3);
    }

    #[test]
    fn test_restore_missing_thread() {
        let (manager, _tmp) = setup_manager();
        let restored = manager.restore_snapshot("ghost", false).unwrap();
        assert!(restored.is_none());
    }

    #[test]
    fn test_restore_with_resume_injects_message() {
        let (manager, _tmp) = setup_manager();
        let msgs = make_messages();
        manager.save_snapshot("thread-2", &msgs, false).unwrap();

        let restored = manager.restore_snapshot("thread-2", true).unwrap();
        let snapshot = restored.unwrap();
        assert_eq!(snapshot.messages.len(), 4);
        assert_eq!(
            snapshot.messages.last().unwrap().content,
            "You were interrupted. Continue from where you left off."
        );
    }

    #[test]
    fn test_list_snapshots() {
        let (manager, _tmp) = setup_manager();
        manager
            .save_snapshot("thread-3", &make_messages(), false)
            .unwrap();
        manager
            .save_snapshot("thread-3", &make_messages(), true)
            .unwrap();

        let list = manager.list_snapshots("thread-3", 10).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_delete_thread() {
        let (manager, _tmp) = setup_manager();
        manager
            .save_snapshot("thread-4", &make_messages(), false)
            .unwrap();
        manager.delete_thread("thread-4").unwrap();

        let restored = manager.restore_snapshot("thread-4", false).unwrap();
        assert!(restored.is_none());
    }

    #[test]
    fn test_should_auto_save() {
        let tmp = TempDir::new().unwrap();
        let store = CheckpointStore::open_at(tmp.path().join("test.db")).unwrap();
        let manager = SnapshotManager::with_auto_save_interval(store, 3);

        assert!(!manager.should_auto_save(0));
        assert!(!manager.should_auto_save(1));
        assert!(!manager.should_auto_save(2));
        assert!(manager.should_auto_save(3));
        assert!(manager.should_auto_save(6));
        assert!(!manager.should_auto_save(7));
    }

    #[test]
    fn test_restore_at_specific_checkpoint() {
        let (manager, _tmp) = setup_manager();
        let msgs = make_messages();
        let cid = manager.save_snapshot("thread-5", &msgs, false).unwrap();

        let restored = manager.restore_at("thread-5", &cid).unwrap();
        assert!(restored.is_some());
        let snapshot = restored.unwrap();
        assert_eq!(snapshot.messages.len(), 3);
    }

    #[test]
    fn test_restore_at_invalid_checkpoint() {
        let (manager, _tmp) = setup_manager();
        let fake_id = uuid::Uuid::nil();
        let restored = manager.restore_at("thread-6", &fake_id).unwrap();
        assert!(restored.is_none());
    }

    #[test]
    fn test_truncated_flag_propagated() {
        let (manager, _tmp) = setup_manager();
        let msgs = make_messages();
        manager.save_snapshot("thread-7", &msgs, true).unwrap();

        let list = manager.list_snapshots("thread-7", 10).unwrap();
        assert!(list[0].truncated);
    }

    #[test]
    fn test_restore_propagates_version_from_db() {
        let (manager, _tmp) = setup_manager();
        let msgs = make_messages();
        let cid = manager.save_snapshot("ver-thread", &msgs, false).unwrap();

        let restored = manager
            .restore_snapshot("ver-thread", false)
            .unwrap()
            .unwrap();
        assert_eq!(restored.meta.version, 1);

        let restored_at = manager.restore_at("ver-thread", &cid).unwrap().unwrap();
        assert_eq!(restored_at.meta.version, 1);
    }

    #[test]
    fn test_delete_single_checkpoint() {
        let (manager, _tmp) = setup_manager();
        let cid = manager
            .save_snapshot("del-thread", &make_messages(), false)
            .unwrap();
        manager.delete_checkpoint(&cid).unwrap();
        // Should no longer be listed
        let list = manager.list_snapshots("del-thread", 10).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn test_list_all_snapshots_across_threads() {
        let (manager, _tmp) = setup_manager();
        manager
            .save_snapshot("t1", &make_messages(), false)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        manager.save_snapshot("t2", &make_messages(), true).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        manager
            .save_snapshot("t1", &make_messages(), false)
            .unwrap();

        let all = manager.list_all_snapshots(10).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].thread_id, "t1");
        assert_eq!(all[1].thread_id, "t2");
        assert_eq!(all[2].thread_id, "t1");
    }
}
