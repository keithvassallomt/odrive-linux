use rusqlite::{params, Connection, Result};
use std::path::Path;

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
