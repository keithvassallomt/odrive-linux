mod db;
pub use db::OdriveDb;

use std::process::{Command, Stdio};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use std::path::Path;
use std::fs;

/// Parse the stdout of `odrive status --mounts`. The agent prints two lines
/// per mount: a local-path line and a remote-path line, each suffixed with
/// `  status:<state>`. We split each line at the last `  status:` marker so
/// paths containing the substring `status:` survive intact, then pair lines
/// up. A trailing unpaired line is ignored.
fn parse_mounts(stdout: &str) -> Vec<OdriveMount> {
    fn split_path_status(line: &str) -> Option<(String, String)> {
        let marker = "  status:";
        let idx = line.rfind(marker)?;
        let path = line[..idx].trim().to_string();
        let status = line[idx + marker.len()..].trim().to_string();
        Some((path, status))
    }

    let lines: Vec<_> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();

    let mut mounts = Vec::new();
    for chunk in lines.chunks(2) {
        let Some((local_path, local_status)) = chunk.first().and_then(|l| split_path_status(l)) else {
            continue;
        };
        let (remote_path, _remote_status) = chunk
            .get(1)
            .and_then(|l| split_path_status(l))
            .unwrap_or_else(|| (String::new(), String::new()));
        let remote_path = if remote_path.is_empty() {
            "/".to_string()
        } else {
            remote_path
        };
        mounts.push(OdriveMount {
            local_path,
            remote_path,
            status: local_status,
        });
    }
    mounts
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
    home: String,
}

impl OdriveAgent {
    pub fn new() -> Self {
        // Every path this crate touches (agent dir, state DB, default
        // mount) is anchored to $HOME. If it isn't set the environment is
        // broken and we'd rather fail loudly than silently pick a wrong
        // directory.
        let home = std::env::var("HOME").expect("HOME environment variable must be set");
        Self {
            bin_path: format!("{}/.odrive-agent/bin/odrive", home),
            agent_path: format!("{}/.odrive-agent/bin/odriveagent", home),
            home,
        }
    }

    /// Conventional default mount path (`~/odrive`) — used by CLI/GUI as
    /// the scan target when no explicit path is given.
    pub fn default_mount_path(&self) -> String {
        format!("{}/odrive", self.home)
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
        // The upstream odrive CLI has no `mounts` subcommand — mount info is
        // exposed via `odrive status --mounts`, which prints two lines per
        // mount:
        //     <localPath>  status:<state>
        //     <remotePath>  status:<state>
        // The remote path may render blank when it's the odrive root (`/`).
        let output = Command::new(&self.bin_path)
            .arg("status")
            .arg("--mounts")
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let msg = if !stderr.is_empty() { stderr } else { stdout };
            return Err(OdriveError::CliError(format!(
                "odrive status --mounts failed: {}",
                msg
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_mounts(&stdout))
    }

    pub fn get_db_path(&self) -> String {
        format!("{}/.odrive-linux.db", self.home)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mounts_single_root_mount() {
        // Real output observed from `odrive status --mounts` with a single
        // mount of /home/keith/odrive against the odrive root `/`. The remote
        // path renders blank in that case.
        let stdout = "/home/keith/odrive  status:Active\n  status:None\n";
        let mounts = parse_mounts(stdout);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].local_path, "/home/keith/odrive");
        assert_eq!(mounts[0].remote_path, "/");
        assert_eq!(mounts[0].status, "Active");
    }

    #[test]
    fn parse_mounts_two_mounts_with_remote_paths() {
        let stdout = "\
/home/keith/gd  status:Active
/Google Drive  status:None
/home/keith/od  status:Active
/OneDrive  status:None
";
        let mounts = parse_mounts(stdout);
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].local_path, "/home/keith/gd");
        assert_eq!(mounts[0].remote_path, "/Google Drive");
        assert_eq!(mounts[1].local_path, "/home/keith/od");
        assert_eq!(mounts[1].remote_path, "/OneDrive");
    }

    #[test]
    fn parse_mounts_empty_input() {
        assert!(parse_mounts("").is_empty());
        assert!(parse_mounts("\n\n").is_empty());
    }
}

