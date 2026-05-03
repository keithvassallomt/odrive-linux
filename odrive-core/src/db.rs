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
