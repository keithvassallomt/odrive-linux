mod db;
pub use db::OdriveDb;

use std::process::{Command, Stdio};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use std::path::Path;
use std::fs;

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

    pub fn is_running(&self) -> bool {
        let output = Command::new(&self.bin_path)
            .arg("status")
            .output();
        
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                !stdout.contains("Unable to connect")
            },
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
        let output = Command::new(&self.bin_path)
            .arg("status")
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let is_running = !stdout.contains("Unable to connect");
        
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

        // odrive mounts output format:
        // /home/keith/odrive  /  active
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                mounts.push(OdriveMount {
                    local_path: parts[0].to_string(),
                    remote_path: parts[1].to_string(),
                    status: parts[2].to_string(),
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
            if dir.is_dir() {
                for entry in fs::read_dir(dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    let file_name = path.file_name().unwrap().to_string_lossy();
                    
                    if file_name.ends_with(".cloud") || file_name.ends_with(".cloudf") {
                        let is_folder = file_name.ends_with(".cloudf");
                        let local_path = path.to_string_lossy();
                        db.upsert_placeholder(&local_path, is_folder, "placeholder").unwrap();
                        *count += 1;
                    } else if path.is_dir() {
                        visit_dirs(&path, db, count)?;
                    }
                }
            }
            Ok(())
        }

        visit_dirs(Path::new(mount_path), &db, &mut count)?;
        Ok(count)
    }
}
