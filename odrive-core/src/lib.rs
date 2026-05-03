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

        // Try systemd first
        let status = Command::new("systemctl")
            .arg("--user")
            .arg("start")
            .arg("odrive.service")
            .status();

        match status {
            Ok(s) if s.success() => {
                // Wait a bit for it to spin up
                std::thread::sleep(std::time::Duration::from_secs(2));
                if self.is_running() {
                    return Ok(());
                }
            }
            _ => {}
        }

        // Fallback to nohup if systemd fails or isn't set up
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
        // Try systemd
        let _ = Command::new("systemctl")
            .arg("--user")
            .arg("stop")
            .arg("odrive.service")
            .status();

        // Also try killing the binary directly just in case
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

    pub fn is_mounted(&self, path: &str) -> bool {
        let output = Command::new("mount").output();
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                stdout.contains(path)
            },
            Err(_) => false,
        }
    }
    
    pub fn setup_systemd_service(&self) -> Result<(), OdriveError> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/keith".to_string());
        let service_path = format!("{}/.config/systemd/user/odrive.service", home);
        let service_dir = Path::new(&service_path).parent().unwrap();
        
        if !service_dir.exists() {
            fs::create_dir_all(service_dir)?;
        }

        let content = format!(
            "[Unit]\n             Description=Run odrive-agent as a user service\n             Wants=network-online.target\n             After=network.target network-online.target\n\n             [Service]\n             Type=simple\n             ExecStart={}\n             Restart=on-failure\n             RestartSec=10\n\n             [Install]\n             WantedBy=default.target",
            self.agent_path
        );

        fs::write(&service_path, content)?;
        
        Command::new("systemctl")
            .arg("--user")
            .arg("daemon-reload")
            .status()?;
            
        Ok(())
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
