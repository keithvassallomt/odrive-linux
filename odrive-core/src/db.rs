use rusqlite::{params, Connection, Result};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct OdriveDb {
    conn: Connection,
}

#[derive(Debug)]
pub struct Placeholder {
    pub id: i32,
    pub local_path: String,
    pub remote_path: Option<String>,
    pub is_folder: bool,
    pub sync_status: String,
}

/// One row in `folder_sync_rules`. The agent has no LIST/REMOVE for its
/// own foldersyncrule storage, so this table is the GUI's source of
/// truth for "did the user set a rule via the Manager?" — useful for
/// rendering the per-mount badge on the dashboard and the Save→Delete
/// button toggle on the per-folder side panel.
#[derive(Debug, Clone)]
pub struct FolderRule {
    pub id: i32,
    pub local_path: String,
    /// Encoded threshold: `0` = never, `-1` = unlimited (`inf`), any
    /// positive integer is the MB value. Use
    /// `FolderSyncThreshold::from_db_value` to decode safely.
    pub threshold_mb: i32,
    pub expand_subfolders: bool,
    pub created_at: i64,
    /// When `Some`, the user has paused this rule via the Manager.
    /// Holds the previous `threshold_mb` (= the value to restore on
    /// resume); while paused, the live `threshold_mb` is forced to
    /// `0` so the upstream agent treats the rule as "never
    /// auto-download" and the rule remains visible (but inert) in the
    /// per-folder editor. `None` = active (the common case).
    pub paused_threshold_mb: Option<i32>,
}

impl FolderRule {
    /// Convenience: a rule is paused iff `paused_threshold_mb` is
    /// `Some`. UI renderers should use this rather than re-checking
    /// the field, so future representation tweaks (e.g. adding a
    /// `paused_at` timestamp) don't require touching every call site.
    pub fn is_paused(&self) -> bool {
        self.paused_threshold_mb.is_some()
    }
}

impl OdriveDb {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS placeholders (
                id INTEGER PRIMARY KEY,
                local_path TEXT NOT NULL UNIQUE,
                remote_path TEXT,
                is_folder BOOLEAN NOT NULL,
                sync_status TEXT NOT NULL
            )",
            [],
        )?;
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS folder_sync_rules (
                id INTEGER PRIMARY KEY,
                local_path TEXT NOT NULL UNIQUE,
                threshold_mb INTEGER NOT NULL,
                expand_subfolders BOOLEAN NOT NULL,
                created_at INTEGER NOT NULL
            )",
            [],
        )?;
        // Cross-process state: who's currently running a sync against
        // which folder. The GUI inserts a row before kicking off
        // `sync_recursive` and deletes it on completion (success or
        // failure). The Nautilus extension reads this on a short poll
        // and paints `odrive-syncing` on matching folders.
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS sync_in_progress (
                id INTEGER PRIMARY KEY,
                local_path TEXT NOT NULL UNIQUE,
                started_at INTEGER NOT NULL
            )",
            [],
        )?;
        self.migrate()?;
        Ok(())
    }

    /// Idempotent forward-only migrations. Run from `init` after the
    /// CREATE TABLE statements so an old database picks up new columns
    /// on the first `OdriveDb::open` call after upgrading.
    ///
    /// Currently:
    /// - Add `paused_threshold_mb INTEGER NULL` to `folder_sync_rules`
    ///   for the pause/resume feature. Existing rows get NULL
    ///   (= "not paused") which preserves prior behaviour.
    fn migrate(&self) -> Result<()> {
        if !self.column_exists("folder_sync_rules", "paused_threshold_mb")? {
            self.conn.execute(
                "ALTER TABLE folder_sync_rules ADD COLUMN paused_threshold_mb INTEGER",
                [],
            )?;
        }
        Ok(())
    }

    fn column_exists(&self, table: &str, column: &str) -> Result<bool> {
        let pragma = format!("PRAGMA table_info({})", table);
        let mut stmt = self.conn.prepare(&pragma)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn upsert_placeholder(&self, local_path: &str, is_folder: bool, status: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO placeholders (local_path, is_folder, sync_status)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(local_path) DO UPDATE SET
                sync_status = excluded.sync_status",
            params![local_path, is_folder, status],
        )?;
        Ok(())
    }

    pub fn get_all_placeholders(&self) -> Result<Vec<Placeholder>> {
        let mut stmt = self.conn.prepare("SELECT id, local_path, remote_path, is_folder, sync_status FROM placeholders")?;
        let rows = stmt.query_map([], |row| {
            Ok(Placeholder {
                id: row.get(0)?,
                local_path: row.get(1)?,
                remote_path: row.get(2)?,
                is_folder: row.get(3)?,
                sync_status: row.get(4)?,
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }
    
    pub fn count_placeholders(&self) -> Result<usize> {
        let count: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM placeholders",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Insert a new rule for `local_path`, or update threshold +
    /// expand_subfolders if one already exists. `created_at` is set
    /// to the current unix timestamp on insert and left untouched on
    /// update (so we can show "rule set on …" in the GUI later).
    pub fn upsert_folder_rule(
        &self,
        local_path: &str,
        threshold_mb: i32,
        expand_subfolders: bool,
    ) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO folder_sync_rules (local_path, threshold_mb, expand_subfolders, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(local_path) DO UPDATE SET
                threshold_mb = excluded.threshold_mb,
                expand_subfolders = excluded.expand_subfolders",
            params![local_path, threshold_mb, expand_subfolders, now],
        )?;
        Ok(())
    }

    /// Return the rule for `local_path`, or `None` if there isn't one.
    pub fn get_folder_rule(&self, local_path: &str) -> Result<Option<FolderRule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, local_path, threshold_mb, expand_subfolders, created_at,
                    paused_threshold_mb
             FROM folder_sync_rules WHERE local_path = ?1",
        )?;
        let row = stmt
            .query_row(params![local_path], |row| {
                Ok(FolderRule {
                    id: row.get(0)?,
                    local_path: row.get(1)?,
                    threshold_mb: row.get(2)?,
                    expand_subfolders: row.get(3)?,
                    created_at: row.get(4)?,
                    paused_threshold_mb: row.get(5)?,
                })
            })
            .ok();
        Ok(row)
    }

    /// Pause `local_path` if a rule exists and isn't already paused.
    /// Stores the current `threshold_mb` in `paused_threshold_mb`
    /// and forces the live `threshold_mb` to `0`. Returns the
    /// previous threshold so the caller can decide what to push to
    /// the upstream agent (typically `FolderSyncThreshold::None`).
    ///
    /// Idempotent: pausing an already-paused or non-existent rule is
    /// a no-op (`Ok(None)`).
    pub fn pause_folder_rule(&self, local_path: &str) -> Result<Option<i32>> {
        let Some(rule) = self.get_folder_rule(local_path)? else {
            return Ok(None);
        };
        if rule.is_paused() {
            return Ok(None);
        }
        self.conn.execute(
            "UPDATE folder_sync_rules
             SET paused_threshold_mb = ?2, threshold_mb = 0
             WHERE local_path = ?1",
            params![local_path, rule.threshold_mb],
        )?;
        Ok(Some(rule.threshold_mb))
    }

    /// Resume a paused rule for `local_path`. Restores the stored
    /// `paused_threshold_mb` to `threshold_mb` and clears
    /// `paused_threshold_mb`. Returns the restored threshold so the
    /// caller can push it back to the upstream agent.
    ///
    /// Idempotent on a non-paused / non-existent rule (`Ok(None)`).
    pub fn resume_folder_rule(&self, local_path: &str) -> Result<Option<i32>> {
        let Some(rule) = self.get_folder_rule(local_path)? else {
            return Ok(None);
        };
        let Some(prior) = rule.paused_threshold_mb else {
            return Ok(None);
        };
        self.conn.execute(
            "UPDATE folder_sync_rules
             SET paused_threshold_mb = NULL, threshold_mb = ?2
             WHERE local_path = ?1",
            params![local_path, prior],
        )?;
        Ok(Some(prior))
    }

    /// Drop the row for `local_path`. Idempotent: deleting a
    /// non-existent rule is not an error. The agent-side rule is NOT
    /// removed by this call — the caller is expected to also push a
    /// `foldersyncrule … 0` to neutralise the upstream rule, since the
    /// agent has no remove command.
    pub fn delete_folder_rule(&self, local_path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM folder_sync_rules WHERE local_path = ?1", params![local_path])?;
        Ok(())
    }

    /// Mark `local_path` as currently syncing. Idempotent — re-marking
    /// an already-tracked path just refreshes the `started_at`
    /// timestamp.
    pub fn mark_sync_in_progress(&self, local_path: &str) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO sync_in_progress (local_path, started_at)
             VALUES (?1, ?2)
             ON CONFLICT(local_path) DO UPDATE SET
                started_at = excluded.started_at",
            params![local_path, now],
        )?;
        Ok(())
    }

    /// Drop the in-progress row for `local_path`. Idempotent.
    pub fn clear_sync_in_progress(&self, local_path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM sync_in_progress WHERE local_path = ?1",
            params![local_path],
        )?;
        Ok(())
    }

    /// Currently-syncing local paths. The Nautilus extension reads this
    /// (with a small TTL cache) on every `update_file_info` to decide
    /// whether to paint the syncing emblem.
    pub fn list_sync_in_progress(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT local_path FROM sync_in_progress")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Every rule the GUI knows about, ordered by local_path so callers
    /// don't have to re-sort for stable display.
    pub fn list_folder_rules(&self) -> Result<Vec<FolderRule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, local_path, threshold_mb, expand_subfolders, created_at,
                    paused_threshold_mb
             FROM folder_sync_rules ORDER BY local_path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(FolderRule {
                id: row.get(0)?,
                local_path: row.get(1)?,
                threshold_mb: row.get(2)?,
                expand_subfolders: row.get(3)?,
                created_at: row.get(4)?,
                paused_threshold_mb: row.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> OdriveDb {
        OdriveDb::open(":memory:").expect("open in-memory db")
    }

    #[test]
    fn fresh_db_is_empty() {
        let db = fresh_db();
        assert_eq!(db.count_placeholders().unwrap(), 0);
        assert!(db.get_all_placeholders().unwrap().is_empty());
    }

    #[test]
    fn upsert_inserts_and_counts() {
        let db = fresh_db();
        db.upsert_placeholder("/a.cloud", false, "placeholder").unwrap();
        db.upsert_placeholder("/b.cloudf", true, "placeholder").unwrap();
        assert_eq!(db.count_placeholders().unwrap(), 2);
    }

    #[test]
    fn upsert_is_idempotent_on_local_path() {
        // The scanner re-upserts every entry on each scan; if this didn't
        // dedup by local_path the count would grow without bound.
        let db = fresh_db();
        db.upsert_placeholder("/same.cloud", false, "placeholder").unwrap();
        db.upsert_placeholder("/same.cloud", false, "synced").unwrap();
        db.upsert_placeholder("/same.cloud", false, "synced").unwrap();
        assert_eq!(db.count_placeholders().unwrap(), 1);
    }

    #[test]
    fn upsert_updates_sync_status_only() {
        // Documented quirk: ON CONFLICT updates sync_status but not
        // is_folder. Pinning this so a refactor doesn't change it silently —
        // if it ever does need to change, this test should be updated
        // alongside.
        let db = fresh_db();
        db.upsert_placeholder("/x", true, "placeholder").unwrap();
        db.upsert_placeholder("/x", false, "synced").unwrap();
        let rows = db.get_all_placeholders().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_folder, "is_folder should not change on upsert");
        assert_eq!(rows[0].sync_status, "synced");
    }

    // ----- folder_sync_rules CRUD -----

    #[test]
    fn folder_rules_fresh_db_is_empty() {
        let db = fresh_db();
        assert!(db.list_folder_rules().unwrap().is_empty());
        assert!(db.get_folder_rule("/anywhere").unwrap().is_none());
    }

    #[test]
    fn folder_rules_upsert_inserts_and_lists() {
        let db = fresh_db();
        db.upsert_folder_rule("/home/k/odrive/Photos", 100, false).unwrap();
        db.upsert_folder_rule("/home/k/odrive/Docs", -1, true).unwrap();
        let rows = db.list_folder_rules().unwrap();
        assert_eq!(rows.len(), 2);
        // Ordered by local_path → Docs before Photos alphabetically.
        assert_eq!(rows[0].local_path, "/home/k/odrive/Docs");
        assert_eq!(rows[0].threshold_mb, -1);
        assert!(rows[0].expand_subfolders);
        assert_eq!(rows[1].local_path, "/home/k/odrive/Photos");
        assert_eq!(rows[1].threshold_mb, 100);
        assert!(!rows[1].expand_subfolders);
    }

    #[test]
    fn folder_rules_upsert_updates_threshold_and_subfolders() {
        let db = fresh_db();
        db.upsert_folder_rule("/p", 100, false).unwrap();
        let r1 = db.get_folder_rule("/p").unwrap().unwrap();
        db.upsert_folder_rule("/p", 500, true).unwrap();
        let r2 = db.get_folder_rule("/p").unwrap().unwrap();
        // Both threshold and expand_subfolders update.
        assert_eq!(r2.threshold_mb, 500);
        assert!(r2.expand_subfolders);
        // created_at is preserved across an update — we want a stable
        // "rule set on" timestamp visible in the GUI.
        assert_eq!(r1.created_at, r2.created_at);
        // Still exactly one row.
        assert_eq!(db.list_folder_rules().unwrap().len(), 1);
    }

    #[test]
    fn folder_rules_delete_is_idempotent() {
        let db = fresh_db();
        db.upsert_folder_rule("/p", 100, false).unwrap();
        db.delete_folder_rule("/p").unwrap();
        assert!(db.get_folder_rule("/p").unwrap().is_none());
        // Second delete on the missing row is a no-op, not an error.
        db.delete_folder_rule("/p").unwrap();
        // Deleting a never-existed path is also a no-op.
        db.delete_folder_rule("/never/existed").unwrap();
    }

    #[test]
    fn folder_rules_get_returns_full_row() {
        let db = fresh_db();
        db.upsert_folder_rule("/x", 250, true).unwrap();
        let r = db.get_folder_rule("/x").unwrap().expect("row exists");
        assert_eq!(r.local_path, "/x");
        assert_eq!(r.threshold_mb, 250);
        assert!(r.expand_subfolders);
        assert!(r.created_at > 0, "created_at should be a real timestamp");
    }

    // ----- folder_sync_rules pause / resume -----

    #[test]
    fn pause_then_resume_round_trip() {
        let db = fresh_db();
        db.upsert_folder_rule("/p", 100, true).unwrap();

        let prev = db.pause_folder_rule("/p").unwrap();
        assert_eq!(prev, Some(100));
        let r = db.get_folder_rule("/p").unwrap().unwrap();
        // Paused state: live threshold zeroed, prior stashed away.
        assert_eq!(r.threshold_mb, 0);
        assert_eq!(r.paused_threshold_mb, Some(100));
        assert!(r.is_paused());
        // expand_subfolders is preserved across pause.
        assert!(r.expand_subfolders);

        let restored = db.resume_folder_rule("/p").unwrap();
        assert_eq!(restored, Some(100));
        let r = db.get_folder_rule("/p").unwrap().unwrap();
        assert_eq!(r.threshold_mb, 100);
        assert_eq!(r.paused_threshold_mb, None);
        assert!(!r.is_paused());
    }

    #[test]
    fn pause_is_idempotent() {
        let db = fresh_db();
        db.upsert_folder_rule("/p", 250, false).unwrap();
        let first = db.pause_folder_rule("/p").unwrap();
        assert_eq!(first, Some(250));
        // Second pause is a no-op — would otherwise overwrite the
        // stashed value with 0 and break resume.
        let second = db.pause_folder_rule("/p").unwrap();
        assert_eq!(second, None);
        let r = db.get_folder_rule("/p").unwrap().unwrap();
        assert_eq!(r.paused_threshold_mb, Some(250));
        assert_eq!(r.threshold_mb, 0);
    }

    #[test]
    fn resume_is_no_op_on_active_rule() {
        let db = fresh_db();
        db.upsert_folder_rule("/p", 100, false).unwrap();
        let r = db.resume_folder_rule("/p").unwrap();
        assert_eq!(r, None);
        // Threshold unchanged.
        assert_eq!(db.get_folder_rule("/p").unwrap().unwrap().threshold_mb, 100);
    }

    #[test]
    fn pause_resume_on_missing_rule_is_ok() {
        let db = fresh_db();
        assert_eq!(db.pause_folder_rule("/never").unwrap(), None);
        assert_eq!(db.resume_folder_rule("/never").unwrap(), None);
    }

    #[test]
    fn pause_resume_preserves_inf_threshold() {
        // -1 is the "unlimited" sentinel. Round-trip through pause /
        // resume must restore exactly that, not a normalised positive.
        let db = fresh_db();
        db.upsert_folder_rule("/p", -1, false).unwrap();
        db.pause_folder_rule("/p").unwrap();
        assert_eq!(
            db.get_folder_rule("/p").unwrap().unwrap().paused_threshold_mb,
            Some(-1)
        );
        db.resume_folder_rule("/p").unwrap();
        assert_eq!(db.get_folder_rule("/p").unwrap().unwrap().threshold_mb, -1);
    }

    #[test]
    fn upsert_after_resume_clears_paused_field() {
        // If the user edits an active rule's threshold via the editor
        // (upsert), the paused column should remain NULL — there's no
        // hidden state to restore later. Smoke-test that upsert
        // doesn't accidentally repopulate paused_threshold_mb.
        let db = fresh_db();
        db.upsert_folder_rule("/p", 100, false).unwrap();
        db.upsert_folder_rule("/p", 500, true).unwrap();
        let r = db.get_folder_rule("/p").unwrap().unwrap();
        assert_eq!(r.paused_threshold_mb, None);
    }

    #[test]
    fn migration_is_idempotent_across_reopens() {
        // Simulate "open the same DB twice in a row" — the migration
        // path checks column_exists and skips ALTER if already there.
        // Use a tempfile so both opens hit the same on-disk state.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ot.db");
        {
            let db = OdriveDb::open(&path).unwrap();
            db.upsert_folder_rule("/p", 100, false).unwrap();
        }
        {
            let db = OdriveDb::open(&path).unwrap();
            // Row survives, and the new column is queryable (== None).
            let r = db.get_folder_rule("/p").unwrap().unwrap();
            assert_eq!(r.threshold_mb, 100);
            assert_eq!(r.paused_threshold_mb, None);
        }
    }

    // ----- sync_in_progress CRUD -----

    #[test]
    fn sync_in_progress_starts_empty() {
        let db = fresh_db();
        assert!(db.list_sync_in_progress().unwrap().is_empty());
    }

    #[test]
    fn sync_in_progress_mark_and_clear() {
        let db = fresh_db();
        db.mark_sync_in_progress("/p/a").unwrap();
        db.mark_sync_in_progress("/p/b").unwrap();
        let mut rows = db.list_sync_in_progress().unwrap();
        rows.sort();
        assert_eq!(rows, vec!["/p/a".to_string(), "/p/b".to_string()]);
        db.clear_sync_in_progress("/p/a").unwrap();
        assert_eq!(db.list_sync_in_progress().unwrap(), vec!["/p/b".to_string()]);
        // Clearing a never-marked path is a no-op, not an error.
        db.clear_sync_in_progress("/never/tracked").unwrap();
    }

    #[test]
    fn sync_in_progress_mark_is_idempotent() {
        // Two consecutive marks on the same path leave one row, not
        // a UNIQUE-constraint failure.
        let db = fresh_db();
        db.mark_sync_in_progress("/p").unwrap();
        db.mark_sync_in_progress("/p").unwrap();
        assert_eq!(db.list_sync_in_progress().unwrap().len(), 1);
    }

    #[test]
    fn get_all_returns_inserted_rows() {
        let db = fresh_db();
        db.upsert_placeholder("/a.cloud", false, "placeholder").unwrap();
        db.upsert_placeholder("/b.cloudf", true, "placeholder").unwrap();
        let mut rows = db.get_all_placeholders().unwrap();
        rows.sort_by(|a, b| a.local_path.cmp(&b.local_path));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].local_path, "/a.cloud");
        assert!(!rows[0].is_folder);
        assert_eq!(rows[0].sync_status, "placeholder");
        // upsert_placeholder never writes remote_path — keep that contract
        // visible until someone deliberately changes it.
        assert!(rows[0].remote_path.is_none());
        assert_eq!(rows[1].local_path, "/b.cloudf");
        assert!(rows[1].is_folder);
        assert_ne!(rows[0].id, rows[1].id);
    }
}
