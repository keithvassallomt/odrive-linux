mod config;
mod db;
pub use config::OdriveConfig;
pub use db::OdriveDb;

use std::process::{Command, Stdio};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use std::path::Path;
use std::fs;

/// True iff the human-readable `odrive status` text reports both an
/// activated account and an active session. Pulled out as a free function
/// so it's testable without spawning the agent.
fn is_authenticated_marker(status_text: &str) -> bool {
    status_text.contains("isActivated: True") && status_text.contains("hasSession: True")
}

/// Build the systemd user unit text targeted at a specific `odriveagent`
/// path. The body is the verbatim unit we already use, with only the
/// `ExecStart` line substituted so a wizard-discovered custom location
/// is honored.
fn render_systemd_unit(agent_path: &str) -> String {
    format!(
        "# Managed by odrive-linux. Edit at your own risk.
[Unit]
Description=Run odrive-agent as a user service
Wants=network-online.target
After=network.target network-online.target

[Service]
Type=simple
ExecStart={agent_path}
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
"
    )
}

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
        // Skip orphan trailing lines that look like a remote-side row but
        // have no matching local path. The agent emits `  status:None` as
        // the second line of every mount (remote half for an odrive-root
        // mount); when the user removes the last mount, that line still
        // shows up by itself and would otherwise produce a phantom row
        // with an empty local_path.
        if local_path.is_empty() {
            continue;
        }
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

/// `odrive placeholderthreshold` accepts these tokens. The tokens we send
/// on the CLI (`never`/`small`/`medium`/`large`/`always`) do NOT match
/// the way the upstream reports them back in `odrive status` — `never`
/// renders as `neverDownload` and `always` renders as `alwaysDownload`.
/// `as_cli_arg` and `from_status_token` straddle that asymmetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaceholderThreshold {
    Never,
    Small,
    Medium,
    Large,
    Always,
}

impl PlaceholderThreshold {
    pub fn as_cli_arg(self) -> &'static str {
        match self {
            PlaceholderThreshold::Never => "never",
            PlaceholderThreshold::Small => "small",
            PlaceholderThreshold::Medium => "medium",
            PlaceholderThreshold::Large => "large",
            PlaceholderThreshold::Always => "always",
        }
    }

    fn from_status_token(token: &str) -> Option<Self> {
        match token {
            "neverDownload" | "never" => Some(PlaceholderThreshold::Never),
            "small" => Some(PlaceholderThreshold::Small),
            "medium" => Some(PlaceholderThreshold::Medium),
            "large" => Some(PlaceholderThreshold::Large),
            "alwaysDownload" | "always" => Some(PlaceholderThreshold::Always),
            _ => None,
        }
    }
}

/// `odrive xlthreshold` (split-large-files threshold). CLI sends
/// `xlarge`; status reports it as `extraLarge`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum XlThreshold {
    Never,
    Small,
    Medium,
    Large,
    Xlarge,
}

impl XlThreshold {
    pub fn as_cli_arg(self) -> &'static str {
        match self {
            XlThreshold::Never => "never",
            XlThreshold::Small => "small",
            XlThreshold::Medium => "medium",
            XlThreshold::Large => "large",
            XlThreshold::Xlarge => "xlarge",
        }
    }

    fn from_status_token(token: &str) -> Option<Self> {
        match token {
            "never" => Some(XlThreshold::Never),
            "small" => Some(XlThreshold::Small),
            "medium" => Some(XlThreshold::Medium),
            "large" => Some(XlThreshold::Large),
            "extraLarge" | "xlarge" => Some(XlThreshold::Xlarge),
            _ => None,
        }
    }
}

/// `odrive autounsyncthreshold` accepts and reports identical tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoUnsyncThreshold {
    Never,
    Day,
    Week,
    Month,
}

impl AutoUnsyncThreshold {
    pub fn as_cli_arg(self) -> &'static str {
        match self {
            AutoUnsyncThreshold::Never => "never",
            AutoUnsyncThreshold::Day => "day",
            AutoUnsyncThreshold::Week => "week",
            AutoUnsyncThreshold::Month => "month",
        }
    }

    fn from_status_token(token: &str) -> Option<Self> {
        match token {
            "never" => Some(AutoUnsyncThreshold::Never),
            "day" => Some(AutoUnsyncThreshold::Day),
            "week" => Some(AutoUnsyncThreshold::Week),
            "month" => Some(AutoUnsyncThreshold::Month),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSettings {
    pub placeholder: PlaceholderThreshold,
    pub xl: XlThreshold,
    pub auto_unsync: AutoUnsyncThreshold,
    pub sync_enabled: bool,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        // Match upstream defaults: full download, no split, no auto-unsync,
        // sync enabled. These are the values a fresh agent reports before
        // any threshold tweaks have been applied.
        Self {
            placeholder: PlaceholderThreshold::Always,
            xl: XlThreshold::Never,
            auto_unsync: AutoUnsyncThreshold::Never,
            sync_enabled: true,
        }
    }
}

/// Pull the four global-settings markers out of `odrive status` text.
/// The upstream prints lines shaped like
/// `placeholderThreshold: neverDownload` (sometimes with extra whitespace
/// and other status fields packed onto the same line, separated by runs
/// of spaces). We scan token-by-token for a known marker key followed by
/// its value; missing or unrecognised markers fall back to defaults
/// rather than panicking, so a future upstream wording change degrades
/// gracefully.
pub fn parse_global_settings(status_text: &str) -> GlobalSettings {
    let mut out = GlobalSettings::default();
    for line in status_text.lines() {
        let mut tokens = line.split_whitespace().peekable();
        while let Some(tok) = tokens.next() {
            match tok {
                "placeholderThreshold:" => {
                    if let Some(v) = tokens.next() {
                        if let Some(p) = PlaceholderThreshold::from_status_token(v) {
                            out.placeholder = p;
                        }
                    }
                }
                "xlThreshold:" => {
                    if let Some(v) = tokens.next() {
                        if let Some(x) = XlThreshold::from_status_token(v) {
                            out.xl = x;
                        }
                    }
                }
                "autoUnsyncThreshold:" => {
                    if let Some(v) = tokens.next() {
                        if let Some(a) = AutoUnsyncThreshold::from_status_token(v) {
                            out.auto_unsync = a;
                        }
                    }
                }
                "syncEnabled:" => {
                    if let Some(v) = tokens.next() {
                        out.sync_enabled = matches!(v, "True" | "true");
                    }
                }
                _ => {}
            }
        }
    }
    out
}

pub struct OdriveAgent {
    bin_path: String,
    agent_path: String,
    agent_bin_dir: String,
    home: String,
}

impl OdriveAgent {
    pub fn new() -> Self {
        // Every path this crate touches (agent dir, state DB, default
        // mount) is anchored to $HOME. If it isn't set the environment is
        // broken and we'd rather fail loudly than silently pick a wrong
        // directory.
        let home = std::env::var("HOME").expect("HOME environment variable must be set");
        let cfg = OdriveConfig::load();
        Self::with_bin_dir(home, cfg.agent_bin_dir)
    }

    /// Construct an agent rooted at an explicit bin directory. Used by the
    /// onboarding wizard's "specify custom location" branch and by tests
    /// that want to bypass the on-disk config.
    pub fn with_bin_dir(home: String, agent_bin_dir: String) -> Self {
        Self {
            bin_path: format!("{}/odrive", agent_bin_dir),
            agent_path: format!("{}/odriveagent", agent_bin_dir),
            agent_bin_dir,
            home,
        }
    }

    /// Return a new `OdriveAgent` with the same `$HOME` but a different
    /// agent bin directory. The wizard uses this after the user picks a
    /// custom install location, before saving it to the config file.
    pub fn with_new_bin_dir(&self, new_bin_dir: String) -> Self {
        Self::with_bin_dir(self.home.clone(), new_bin_dir)
    }

    pub fn agent_bin_dir(&self) -> &str {
        &self.agent_bin_dir
    }

    pub fn home(&self) -> &str {
        &self.home
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

    /// True if the agent process is alive on the system. Uses `pgrep -f`
    /// against the canonical agent path — a stable upstream contract,
    /// since both the systemd unit and our nohup fallback launch the
    /// binary at exactly that path.
    fn agent_process_alive(&self) -> bool {
        Command::new("pgrep")
            .arg("-f")
            .arg(&self.agent_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    pub fn is_running(&self) -> bool {
        if !self.binary_exists() {
            return false;
        }
        // Two signals, both required:
        //   1. The agent process is alive (pgrep against the agent path).
        //   2. `odrive status` exits cleanly — catches the small window
        //      where the process is up but the daemon hasn't yet bound
        //      its IPC, or has wedged.
        // This replaces an earlier substring match against
        // "Unable to connect" in stdout/stderr, which was fragile to
        // upstream wording changes; the bare exit-code check on its own
        // also doesn't suffice because older `odrive` builds returned 0
        // even when the daemon was unreachable.
        if !self.agent_process_alive() {
            return false;
        }
        Command::new(&self.bin_path)
            .arg("status")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
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

    /// Run the official odrive install pipeline into `self.agent_bin_dir`.
    /// We shell out to bash to use the same curl+tar steps the upstream
    /// publishes; doing the equivalent natively in Rust would pull in
    /// reqwest+tar+flate2 just to replicate four lines of shell.
    /// On completion verifies both `odrive` and `odriveagent` exist.
    pub fn install_official(&self) -> Result<(), OdriveError> {
        let script = format!(
            r#"set -eo pipefail
od="{dir}"
mkdir -p "$od"
curl -fL "https://dl.odrive.com/odrive-py" --create-dirs -o "$od/odrive.py"
curl -fL "https://dl.odrive.com/odriveagent-lnx-64" | tar -xzf- -C "$od/"
curl -fL "https://dl.odrive.com/odrivecli-lnx-64" | tar -xzf- -C "$od/"
"#,
            dir = self.agent_bin_dir,
        );
        let status = Command::new("bash").arg("-c").arg(&script).status()?;
        if !status.success() {
            return Err(OdriveError::CliError(format!(
                "official install pipeline exited {}",
                status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".to_string())
            )));
        }
        if !Path::new(&self.bin_path).exists() {
            return Err(OdriveError::CliError(format!(
                "install completed but {} is missing",
                self.bin_path
            )));
        }
        if !Path::new(&self.agent_path).exists() {
            return Err(OdriveError::CliError(format!(
                "install completed but {} is missing",
                self.agent_path
            )));
        }
        Ok(())
    }

    /// Write a systemd user unit pointing at this agent's binary path.
    /// Replaces any existing unit at the same path. The wizard then calls
    /// `enable_systemd_unit()` to load + start it.
    pub fn write_systemd_unit(&self) -> Result<(), OdriveError> {
        let body = render_systemd_unit(&self.agent_path);
        let path = format!("{}/.config/systemd/user/odrive.service", self.home);
        if let Some(parent) = Path::new(&path).parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, body)?;
        Ok(())
    }

    /// `systemctl --user daemon-reload && systemctl --user enable --now
    /// odrive.service`. daemon-reload is necessary for fresh unit files;
    /// `enable --now` both enables auto-start at login and starts the
    /// service immediately.
    pub fn enable_systemd_unit(&self) -> Result<(), OdriveError> {
        let reload = Command::new("systemctl")
            .arg("--user")
            .arg("daemon-reload")
            .status()?;
        if !reload.success() {
            return Err(OdriveError::Systemd("daemon-reload failed".to_string()));
        }
        let enable = Command::new("systemctl")
            .arg("--user")
            .arg("enable")
            .arg("--now")
            .arg("odrive.service")
            .status()?;
        if !enable.success() {
            return Err(OdriveError::Systemd("enable --now odrive.service failed".to_string()));
        }
        Ok(())
    }

    /// `loginctl enable-linger <user>`. Lets the user-level service stay
    /// up after logout and start at boot. Requires polkit/sudo at the OS
    /// level the first time; if that prompt isn't available the call may
    /// fail — surface the error rather than silently swallowing it.
    pub fn enable_linger(&self) -> Result<(), OdriveError> {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .map_err(|_| OdriveError::CliError("USER/LOGNAME not set; cannot enable linger".to_string()))?;
        let status = Command::new("loginctl")
            .arg("enable-linger")
            .arg(&user)
            .status()?;
        if !status.success() {
            return Err(OdriveError::Systemd(format!(
                "loginctl enable-linger {} failed (exit {})",
                user,
                status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".to_string())
            )));
        }
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
        let process_alive = self.agent_process_alive();
        Ok(OdriveStatus {
            is_running: process_alive && output.status.success(),
            sync_status: String::from_utf8_lossy(&output.stdout).to_string(),
        })
    }

    /// True iff the agent reports both `isActivated: True` and
    /// `hasSession: True` in its status output. Reuses the same `odrive
    /// status` call as `get_status`/`is_running` — the upstream prints
    /// these markers in the human-readable status text. If the binary
    /// isn't there or the call fails, treat the user as unauthenticated.
    pub fn is_authenticated(&self) -> bool {
        is_authenticated_marker(&match self.get_status() {
            Ok(s) => s.sync_status,
            Err(_) => return false,
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

    /// Wrapper for `odrive authenticate <auth_key>`. Used by the wizard's
    /// Login page after the user pastes their key from
    /// https://www.odrive.com/account/authcodes.
    pub fn authenticate(&self, auth_key: &str) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("authenticate")
            .arg(auth_key)
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(OdriveError::CliError(String::from_utf8_lossy(&output.stderr).to_string()))
        }
    }

    /// Wrapper for `odrive mount <local> <remote>`. Used by the wizard's
    /// optional Mount page and any future post-wizard CTA. Local path is
    /// expected to be absolute; the upstream creates it if it doesn't exist.
    pub fn mount(&self, local: &str, remote: &str) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("mount")
            .arg(local)
            .arg(remote)
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(OdriveError::CliError(String::from_utf8_lossy(&output.stderr).to_string()))
        }
    }

    /// Wrapper for `odrive unmount <local>`. Mirror of `mount`; removes
    /// the mount entry from the agent but leaves any already-synced
    /// files on disk for the user to handle.
    pub fn unmount(&self, local: &str) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("unmount")
            .arg(local)
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(OdriveError::CliError(String::from_utf8_lossy(&output.stderr).to_string()))
        }
    }

    /// Wrapper for `odrive placeholderthreshold <value>`. Sets the
    /// global default for which files materialise on sync vs. stay as
    /// placeholders.
    pub fn placeholder_threshold(&self, value: PlaceholderThreshold) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("placeholderthreshold")
            .arg(value.as_cli_arg())
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(OdriveError::CliError(String::from_utf8_lossy(&output.stderr).to_string()))
        }
    }

    /// Wrapper for `odrive xlthreshold <value>`. Sets the size at which
    /// large files get split into chunks during upload.
    pub fn xl_threshold(&self, value: XlThreshold) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("xlthreshold")
            .arg(value.as_cli_arg())
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(OdriveError::CliError(String::from_utf8_lossy(&output.stderr).to_string()))
        }
    }

    /// Wrapper for `odrive autounsyncthreshold <value>`. Files
    /// untouched for the configured period get reverted to placeholders.
    /// Premium-tier feature upstream; non-premium accounts get a CLI
    /// error which we surface verbatim.
    pub fn auto_unsync_threshold(&self, value: AutoUnsyncThreshold) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("autounsyncthreshold")
            .arg(value.as_cli_arg())
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(OdriveError::CliError(String::from_utf8_lossy(&output.stderr).to_string()))
        }
    }

    /// Wrapper for `odrive shutdown`. Terminates the agent cleanly.
    /// Used by the panel indicator's "Quit" item.
    pub fn shutdown(&self) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("shutdown")
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(OdriveError::CliError(String::from_utf8_lossy(&output.stderr).to_string()))
        }
    }

    /// Snapshot the four global threshold-style settings by parsing the
    /// human-readable `odrive status` text. Falls back to defaults when
    /// the agent isn't reachable; the caller can distinguish that case
    /// via `get_status()` if they care.
    pub fn get_global_settings(&self) -> Result<GlobalSettings, OdriveError> {
        let status = self.get_status()?;
        Ok(parse_global_settings(&status.sync_status))
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

    #[test]
    fn parse_mounts_orphan_remote_line_yields_no_mount() {
        // After unmounting the last mount, the agent still emits the
        // remote-side line `  status:None` by itself. That has no local
        // path and shouldn't materialise as a phantom mount in the GUI.
        assert!(parse_mounts("  status:None\n").is_empty());
    }

    #[test]
    fn is_authenticated_marker_requires_both_true_markers() {
        // Real-shape excerpt from `odrive status` on this box.
        let activated = "isActivated: True                                               hasSession: True\nemail: keithv@me.com";
        assert!(is_authenticated_marker(activated));

        // Either marker absent or false → not authenticated.
        assert!(!is_authenticated_marker("isActivated: True\nhasSession: False"));
        assert!(!is_authenticated_marker("isActivated: False\nhasSession: True"));
        assert!(!is_authenticated_marker("isActivated: True"));
        assert!(!is_authenticated_marker("hasSession: True"));
        assert!(!is_authenticated_marker(""));
    }

    #[test]
    fn parse_global_settings_real_status_text() {
        // Real shape observed from `odrive status` on this box, with the
        // four markers we care about wedged into multi-field lines
        // (upstream packs several settings onto one line separated by
        // runs of whitespace).
        let s = "\
isActivated: True                                               hasSession: True
email: keith@example.com
syncEnabled: True                                               mounts: 1
placeholderThreshold: neverDownload                             autoUnsyncThreshold: never
xlThreshold: extraLarge
";
        let g = parse_global_settings(s);
        assert_eq!(g.placeholder, PlaceholderThreshold::Never);
        assert_eq!(g.xl, XlThreshold::Xlarge);
        assert_eq!(g.auto_unsync, AutoUnsyncThreshold::Never);
        assert!(g.sync_enabled);
    }

    #[test]
    fn parse_global_settings_alwaysdownload_and_disabled() {
        let s = "\
placeholderThreshold: alwaysDownload
xlThreshold: small
autoUnsyncThreshold: month
syncEnabled: False
";
        let g = parse_global_settings(s);
        assert_eq!(g.placeholder, PlaceholderThreshold::Always);
        assert_eq!(g.xl, XlThreshold::Small);
        assert_eq!(g.auto_unsync, AutoUnsyncThreshold::Month);
        assert!(!g.sync_enabled);
    }

    #[test]
    fn parse_global_settings_missing_markers_fall_back_to_defaults() {
        // No marker present at all → entire struct equals Default.
        let g = parse_global_settings("isActivated: True\nemail: x@y\n");
        let d = GlobalSettings::default();
        assert_eq!(g.placeholder, d.placeholder);
        assert_eq!(g.xl, d.xl);
        assert_eq!(g.auto_unsync, d.auto_unsync);
        assert_eq!(g.sync_enabled, d.sync_enabled);
    }

    #[test]
    fn parse_global_settings_unknown_value_keeps_default() {
        // Marker present but value unparseable → that one field stays
        // at default; the others still parse normally.
        let s = "\
placeholderThreshold: gibberish
xlThreshold: medium
";
        let g = parse_global_settings(s);
        assert_eq!(g.placeholder, GlobalSettings::default().placeholder);
        assert_eq!(g.xl, XlThreshold::Medium);
    }

    #[test]
    fn threshold_cli_args_round_trip() {
        // The CLI tokens we send and the status tokens we accept should
        // both map back to the same enum variant.
        for v in [
            PlaceholderThreshold::Never,
            PlaceholderThreshold::Small,
            PlaceholderThreshold::Medium,
            PlaceholderThreshold::Large,
            PlaceholderThreshold::Always,
        ] {
            assert_eq!(
                PlaceholderThreshold::from_status_token(v.as_cli_arg()),
                Some(v)
            );
        }
        // Status-only renderings:
        assert_eq!(
            PlaceholderThreshold::from_status_token("neverDownload"),
            Some(PlaceholderThreshold::Never)
        );
        assert_eq!(
            PlaceholderThreshold::from_status_token("alwaysDownload"),
            Some(PlaceholderThreshold::Always)
        );
        assert_eq!(
            XlThreshold::from_status_token("extraLarge"),
            Some(XlThreshold::Xlarge)
        );
    }

    #[test]
    fn render_systemd_unit_substitutes_exec_start() {
        let unit = render_systemd_unit("/opt/odrive/bin/odriveagent");
        assert!(unit.contains("ExecStart=/opt/odrive/bin/odriveagent\n"));
        // Skeleton bits we rely on for systemd to accept the unit:
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("Type=simple"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("WantedBy=default.target"));
        // No leftover `%h` placeholder from the upstream-template version.
        assert!(!unit.contains("%h"));
    }
}

