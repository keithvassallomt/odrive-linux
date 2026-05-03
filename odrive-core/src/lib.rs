mod db;
pub use db::OdriveDb;

use std::process::{Command, Stdio};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use std::path::Path;
use std::fs;

/// Split a line on runs of 2+ whitespace characters. This preserves single
/// spaces inside fields (e.g. mount paths with spaces) which `split_whitespace`
/// would shred.
fn split_columns(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut ws_run = 0usize;

    for ch in line.chars() {
        if ch.is_whitespace() {
            ws_run += 1;
        } else {
            if ws_run >= 2 && !current.is_empty() {
                out.push(std::mem::take(&mut current));
            } else if ws_run >= 1 && !current.is_empty() {
                current.push(' ');
            }
            current.push(ch);
            ws_run = 0;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[derive(Error, Debug)]
pub enum OdriveError {
    #[error("CLI execution failed: {0}")]
    CliError(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Systemd error: {0}")]
    Systemd(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OdriveStatus {
    pub is_running: bool,
    pub sync_status: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OdriveMount {
    pub local_path: String,
    pub remote_path: String,
    pub status: String,
}

pub struct OdriveAgent {
    bin_path: String,
    agent_path: String,
}

impl OdriveAgent {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/keith".to_string());
        Self {
            bin_path: format!("{}/.odrive-agent/bin/odrive", home),
            agent_path: format!("{}/.odrive-agent/bin/odriveagent", home),
        }
    }

    /// True if the odrive CLI binary itself is on disk. A `false` here means
    /// the user hasn't installed the agent at all, which is a different
    /// failure mode from the daemon being stopped.
    pub fn binary_exists(&self) -> bool {
        Path::new(&self.bin_path).exists()
    }

    pub fn is_running(&self) -> bool {
        if !self.binary_exists() {
            return false;
        }
        match Command::new(&self.bin_path).arg("status").output() {
            Ok(out) => {
                // Primary signal: a successful exit. Secondary: the legacy
                // "Unable to connect" marker, since older odrive builds
                // returned 0 even when the daemon was unreachable.
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr),
                );
                out.status.success() && !combined.contains("Unable to connect")
            }
            Err(_) => false,
        }
    }

    pub fn start(&self) -> Result<(), OdriveError> {
        if self.is_running() {
            return Ok(());
        }

        let status = Command::new("systemctl")
            .arg("--user")
            .arg("start")
            .arg("odrive.service")
            .status();

        match status {
            Ok(s) if s.success() => {
                std::thread::sleep(std::time::Duration::from_secs(2));
                if self.is_running() {
                    return Ok(());
                }
            }
            _ => {}
        }

        Command::new("nohup")
            .arg(&self.agent_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        std::thread::sleep(std::time::Duration::from_secs(2));
        if self.is_running() {
            Ok(())
        } else {
            Err(OdriveError::CliError("Failed to start odriveagent via fallback".to_string()))
        }
    }

    pub fn stop(&self) -> Result<(), OdriveError> {
        let _ = Command::new("systemctl")
            .arg("--user")
            .arg("stop")
            .arg("odrive.service")
            .status();

        Command::new("pkill")
            .arg("odriveagent")
            .status()?;

        Ok(())
    }

    pub fn get_status(&self) -> Result<OdriveStatus, OdriveError> {
        if !self.binary_exists() {
            return Ok(OdriveStatus {
                is_running: false,
                sync_status: format!(
                    "odrive binary not found at {}. Install it from https://www.odrive.com/downloads",
                    self.bin_path,
                ),
            });
        }

        let output = Command::new(&self.bin_path).arg("status").output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let is_running =
            output.status.success() && !stdout.contains("Unable to connect") && !stderr.contains("Unable to connect");

        Ok(OdriveStatus {
            is_running,
            sync_status: stdout.to_string(),
        })
    }

    pub fn sync(&self, path: &str) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("sync")
            .arg(path)
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(OdriveError::CliError(String::from_utf8_lossy(&output.stderr).to_string()))
        }
    }

    pub fn unsync(&self, path: &str) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("unsync")
            .arg(path)
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(OdriveError::CliError(String::from_utf8_lossy(&output.stderr).to_string()))
        }
    }

    pub fn refresh(&self, path: &str) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("refresh")
            .arg(path)
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(OdriveError::CliError(String::from_utf8_lossy(&output.stderr).to_string()))
        }
    }

    pub fn get_mounts(&self) -> Result<Vec<OdriveMount>, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("mounts")
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut mounts = Vec::new();

        // `odrive mounts` separates columns with 2+ spaces, so paths containing
        // single spaces survive intact. Lines with fewer than 3 columns (blanks,
        // headers, footer text) are skipped.
        for line in stdout.lines() {
            let parts = split_columns(line);
            if parts.len() >= 3 {
                mounts.push(OdriveMount {
                    local_path: parts[0].clone(),
                    remote_path: parts[1].clone(),
                    status: parts[2].clone(),
                });
            }
        }

        Ok(mounts)
    }

    pub fn get_db_path(&self) -> String {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/keith".to_string());
        format!("{}/.odrive-linux.db", home)
    }

    pub fn scan_placeholders(&self, mount_path: &str) -> Result<usize, OdriveError> {
        let db = OdriveDb::open(self.get_db_path()).map_err(|e| OdriveError::Parse(e.to_string()))?;
        let mut count = 0;

        fn visit_dirs(dir: &Path, db: &OdriveDb, count: &mut usize) -> std::io::Result<()> {
            if !dir.is_dir() {
                return Ok(());
            }
            for entry in fs::read_dir(dir)? {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        log::warn!("scan: skipping unreadable entry in {}: {}", dir.display(), e);
                        continue;
                    }
                };
                let path = entry.path();
                let Some(file_name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                    continue;
                };

                if file_name.ends_with(".cloud") || file_name.ends_with(".cloudf") {
                    let is_folder = file_name.ends_with(".cloudf");
                    let local_path = path.to_string_lossy();
                    if let Err(e) = db.upsert_placeholder(&local_path, is_folder, "placeholder") {
                        log::warn!("scan: failed to record placeholder {}: {}", local_path, e);
                        continue;
                    }
                    *count += 1;
                } else if path.is_dir() {
                    if let Err(e) = visit_dirs(&path, db, count) {
                        log::warn!("scan: failed to recurse into {}: {}", path.display(), e);
                    }
                }
            }
            Ok(())
        }

        visit_dirs(Path::new(mount_path), &db, &mut count)?;
        Ok(count)
    }
}
