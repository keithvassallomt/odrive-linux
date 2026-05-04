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
        Ok(())
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
            "SELECT id, local_path, threshold_mb, expand_subfolders, created_at
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
                })
            })
            .ok();
        Ok(row)
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

    /// Every rule the GUI knows about, ordered by local_path so callers
    /// don't have to re-sort for stable display.
    pub fn list_folder_rules(&self) -> Result<Vec<FolderRule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, local_path, threshold_mb, expand_subfolders, created_at
             FROM folder_sync_rules ORDER BY local_path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(FolderRule {
                id: row.get(0)?,
                local_path: row.get(1)?,
                threshold_mb: row.get(2)?,
                expand_subfolders: row.get(3)?,
                created_at: row.get(4)?,
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
