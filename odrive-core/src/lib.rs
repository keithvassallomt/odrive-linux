mod config;
mod db;
pub use config::{OdriveConfig, DEFAULT_TRAY_ICON_COLOR, TRAY_ICON_COLORS};
pub use db::{FolderRule, OdriveDb};

use std::process::{Command, Stdio};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::time::Duration;
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

/// One trashed item as reported by `odrive status --trash`. The CLI's
/// formatter joins `folderPath` and `name` so we only get the full
/// local path back; the agent's IPC carries more (`name`, `folderPath`,
/// `description`) but those are unreachable from outside the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashItem {
    pub local_path: String,
}

/// One backup job as the CLI's `BackupsStatus` formatter prints it. The
/// agent's IPC carries richer per-job fields (`processing`, `size`,
/// `percentComplete`); the CLI strips them. We can read those via
/// [`AgentIpc::status`] when we need progress UI, but the bulk of the
/// Backup tab uses this CLI-shell-out struct so it stays consistent
/// with the rest of the Manager's data path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupJob {
    pub job_id: String,
    pub local_path: String,
    pub remote_path: String,
    pub status: String,
}

/// Pre-formatted schedule strings the agent already emits (e.g.
/// `"09:42PM"`, `"Next backup in 11 hours and 14 minutes"`). We pass
/// them through to the UI verbatim — the agent's wording is the same
/// users see on macOS / Windows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackupSchedule {
    pub last_backup_time: Option<String>,
    pub time_till_next: Option<String>,
}

/// Parse `odrive status --backups` output. The agent emits one record
/// per job formatted as four labelled lines (with surrounding blank
/// lines):
/// ```text
/// ID: <jobId>
/// Local Path: <localPath>
/// Remote Path: <remotePath>
/// Status: <status>
/// ```
/// On empty trash the formatter prints `No backup jobs.\n`. We handle
/// both, plus tolerate stray blank lines and out-of-order field labels
/// (defensive: the upstream formatter is fixed, but order shifts have
/// happened in past agent versions).
pub fn parse_backups_status(stdout: &str) -> Vec<BackupJob> {
    let mut jobs = Vec::new();
    let mut cur = std::collections::HashMap::<&str, String>::new();
    let flush =
        |cur: &mut std::collections::HashMap<&str, String>, jobs: &mut Vec<BackupJob>| {
            if let Some(job_id) = cur.remove("ID") {
                jobs.push(BackupJob {
                    job_id,
                    local_path: cur.remove("Local Path").unwrap_or_default(),
                    remote_path: cur.remove("Remote Path").unwrap_or_default(),
                    status: cur.remove("Status").unwrap_or_default(),
                });
            }
            cur.clear();
        };
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line == "No backup jobs." {
            flush(&mut cur, &mut jobs);
            continue;
        }
        for label in &["ID", "Local Path", "Remote Path", "Status"] {
            let prefix = format!("{}: ", label);
            if let Some(value) = line.strip_prefix(&prefix) {
                if *label == "ID" && cur.contains_key("ID") {
                    // New record starting before a blank line — flush.
                    flush(&mut cur, &mut jobs);
                }
                cur.insert(label, value.to_string());
                break;
            }
        }
    }
    flush(&mut cur, &mut jobs);
    jobs
}

/// Parse `odrive status --trash` output.
///
/// Truth table:
/// - Empty output / "No trash." sentinel → empty Vec.
/// - One line per trashed item, the full local path.
/// - Surrounding blank lines tolerated.
pub fn parse_trash_status(stdout: &str) -> Vec<TrashItem> {
    stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && *l != "No trash.")
        .map(|l| TrashItem {
            local_path: l.to_string(),
        })
        .collect()
}

/// Compose the odrive web-app URL (`https://www.odrive.com/browse/<path>`)
/// for a local filesystem path, using the supplied mount list to resolve
/// which remote namespace the path belongs to. Pure: no I/O. Returns an
/// error if `path` is not under any mount's `local_path` prefix.
pub fn build_web_url(path: &str, mounts: &[OdriveMount]) -> Result<String, OdriveError> {
    let trimmed = path.trim_end_matches('/');
    let (mount, rel) = mounts
        .iter()
        .find_map(|m| {
            let mp = m.local_path.trim_end_matches('/');
            if trimmed == mp {
                Some((m, String::new()))
            } else if let Some(stripped) = trimmed.strip_prefix(&format!("{}/", mp)) {
                Some((m, stripped.to_string()))
            } else {
                None
            }
        })
        .ok_or_else(|| {
            OdriveError::CliError(format!("path is not inside an odrive mount: {}", path))
        })?;

    let rel = rel
        .strip_suffix(".cloudf")
        .or_else(|| rel.strip_suffix(".cloud"))
        .map(|s| s.to_string())
        .unwrap_or(rel);

    let remote_prefix = mount.remote_path.trim_start_matches('/').trim_end_matches('/');
    let combined = match (remote_prefix.is_empty(), rel.is_empty()) {
        (true, true) => String::new(),
        (true, false) => rel,
        (false, true) => remote_prefix.to_string(),
        (false, false) => format!("{}/{}", remote_prefix, rel),
    };

    Ok(format!(
        "https://www.odrive.com/browse/{}",
        percent_encode_path(&combined)
    ))
}

/// Percent-encode a path component for inclusion in an HTTP URL. Keeps
/// RFC 3986 unreserved chars (`A-Z a-z 0-9 - _ . ~`) and `/` unencoded;
/// every other byte (including spaces, `?`, `#`, `&`, non-ASCII UTF-8
/// bytes) becomes `%XX`. Avoids pulling in the `percent-encoding` crate
/// for what amounts to a half-screen of code.
fn percent_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char);
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{:02X}", b);
            }
        }
    }
    out
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

/// `odrive autotrashthreshold` accepts and reports identical tokens.
/// Cadence for automatically emptying the agent's trash; `Never` keeps
/// items in trash indefinitely until the user explicitly empties it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoTrashThreshold {
    Never,
    Immediately,
    Fifteen,
    Hour,
    Day,
}

impl AutoTrashThreshold {
    pub fn as_cli_arg(self) -> &'static str {
        match self {
            AutoTrashThreshold::Never => "never",
            AutoTrashThreshold::Immediately => "immediately",
            AutoTrashThreshold::Fifteen => "fifteen",
            AutoTrashThreshold::Hour => "hour",
            AutoTrashThreshold::Day => "day",
        }
    }

    fn from_status_token(token: &str) -> Option<Self> {
        match token {
            "never" => Some(AutoTrashThreshold::Never),
            "immediately" => Some(AutoTrashThreshold::Immediately),
            "fifteen" => Some(AutoTrashThreshold::Fifteen),
            "hour" => Some(AutoTrashThreshold::Hour),
            "day" => Some(AutoTrashThreshold::Day),
            _ => None,
        }
    }
}

/// Per-folder sync threshold. `odrive foldersyncrule <path> <threshold>`
/// accepts the literal `0` to disable auto-download, the literal `inf`
/// to download everything regardless of size, or any positive integer
/// MB. Modelling those three cases at the type level keeps the call
/// sites unambiguous and makes `0` (= never) a deliberate choice rather
/// than a sentinel collision with "default".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FolderSyncThreshold {
    /// No auto-download for this folder. Encoded as the CLI literal `0`.
    None,
    /// Auto-download files at or below this size in MB.
    Mb(u32),
    /// Auto-download all files. Encoded as the CLI literal `inf`.
    Inf,
}

impl FolderSyncThreshold {
    pub fn to_cli_arg(self) -> String {
        match self {
            FolderSyncThreshold::None => "0".to_string(),
            FolderSyncThreshold::Mb(n) => n.to_string(),
            FolderSyncThreshold::Inf => "inf".to_string(),
        }
    }

    /// Encode as a single i32 for the SQLite `threshold_mb` column.
    /// `0` → `None`, `-1` → `Inf`, anything else is the MB value
    /// directly. Negative MB is a programming error elsewhere; we
    /// don't validate the range here.
    pub fn to_db_value(self) -> i32 {
        match self {
            FolderSyncThreshold::None => 0,
            FolderSyncThreshold::Inf => -1,
            FolderSyncThreshold::Mb(n) => n as i32,
        }
    }

    /// Inverse of `to_db_value`. Out-of-range values fall back to `None`.
    pub fn from_db_value(v: i32) -> Self {
        match v {
            0 => FolderSyncThreshold::None,
            -1 => FolderSyncThreshold::Inf,
            n if n > 0 => FolderSyncThreshold::Mb(n as u32),
            _ => FolderSyncThreshold::None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSettings {
    pub placeholder: PlaceholderThreshold,
    pub xl: XlThreshold,
    pub auto_unsync: AutoUnsyncThreshold,
    pub auto_trash: AutoTrashThreshold,
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
            auto_trash: AutoTrashThreshold::Never,
            sync_enabled: true,
        }
    }
}

/// Snapshot of the agent's "what's currently in flight" counters as
/// printed at the bottom of `odrive status`. The block looks like:
///
/// ```text
/// Sync Requests: 0
/// Background Requests: 0
/// Uploads: 0
/// Downloads: 0
/// Trash: 0
/// Waiting: 0
/// Not Allowed: 0
/// ```
///
/// We track the five counters that mean "real work in progress"
/// (`is_active` returns true if any are > 0). `Trash` and `Not Allowed`
/// aren't progress signals — Trash is queued deletions awaiting flush,
/// Not Allowed is a permanent error bucket — so they're excluded from
/// the activity decision. Background Requests covers folder refreshes
/// triggered by the periodic remote scan, which is the most common
/// "agent is doing something" state on an idle account.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncActivity {
    pub sync_requests: u32,
    pub background_requests: u32,
    pub uploads: u32,
    pub downloads: u32,
    pub waiting: u32,
}

impl SyncActivity {
    /// True when the agent is doing — or about to do — real work. The
    /// tray indicator's animation is gated on this.
    pub fn is_active(&self) -> bool {
        self.sync_requests > 0
            || self.background_requests > 0
            || self.uploads > 0
            || self.downloads > 0
            || self.waiting > 0
    }
}

/// Parse the activity counters out of `odrive status` text. Each line
/// is `<Label>: <number>`; missing or non-numeric values fall back to
/// 0 (matching the "no work" default) so a future label rewording or a
/// truncated status response degrades to "idle" rather than panicking.
pub fn parse_sync_activity(status_text: &str) -> SyncActivity {
    let mut out = SyncActivity::default();
    for line in status_text.lines() {
        let line = line.trim();
        let Some((label, value)) = line.rsplit_once(':') else {
            continue;
        };
        let Ok(n) = value.trim().parse::<u32>() else {
            continue;
        };
        match label.trim() {
            "Sync Requests" => out.sync_requests = n,
            "Background Requests" => out.background_requests = n,
            "Uploads" => out.uploads = n,
            "Downloads" => out.downloads = n,
            "Waiting" => out.waiting = n,
            _ => {}
        }
    }
    out
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
                "autoTrashThreshold:" => {
                    if let Some(v) = tokens.next() {
                        if let Some(a) = AutoTrashThreshold::from_status_token(v) {
                            out.auto_trash = a;
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

#[derive(Clone)]
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

    /// Wrapper for `odrive sync <path> --recursive [--nodownload]`. The
    /// `no_download` flag is the lazy-expansion path used by the mount
    /// detail page on first open: it materialises every placeholder
    /// (creates real directories from `.cloudf`s) without pulling file
    /// contents. Without `--nodownload` it's the explicit "Sync now"
    /// for a one-time per-folder operation.
    pub fn sync_recursive(&self, path: &str, no_download: bool) -> Result<String, OdriveError> {
        let mut cmd = Command::new(&self.bin_path);
        cmd.arg("sync").arg(path).arg("--recursive");
        if no_download {
            cmd.arg("--nodownload");
        }
        let output = cmd.output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(OdriveError::CliError(String::from_utf8_lossy(&output.stderr).to_string()))
        }
    }

    /// Wrapper for `odrive foldersyncrule [--expandsubfolders] <path>
    /// <threshold>`. The agent has no LIST or REMOVE for these rules
    /// — to "delete" one we set the threshold to `0` (never
    /// auto-download for this folder) and drop our own SQLite tracking
    /// row. See `OdriveDb::delete_folder_rule` for that side.
    pub fn folder_sync_rule(
        &self,
        path: &str,
        threshold: FolderSyncThreshold,
        expand_subfolders: bool,
    ) -> Result<String, OdriveError> {
        let mut cmd = Command::new(&self.bin_path);
        cmd.arg("foldersyncrule");
        if expand_subfolders {
            cmd.arg("--expandsubfolders");
        }
        cmd.arg(path).arg(threshold.to_cli_arg());
        let output = cmd.output()?;

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

    /// Wrapper for `odrive sharelink <path>`. The upstream CLI prints a
    /// single share URL on stdout (e.g. `https://www.odrive.com/s/<id>`)
    /// and exits 0; we trim trailing whitespace so callers can drop the
    /// result straight into `xdg-open` or a clipboard write.
    pub fn share_link(&self, path: &str) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("sharelink")
            .arg(path)
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(OdriveError::CliError(String::from_utf8_lossy(&output.stderr).to_string()))
        }
    }

    /// Compose the odrive web-app URL for a local path. Looks up which
    /// mount the path lives under, joins the mount's `remote_path` with
    /// the relative segment, strips a trailing `.cloud`/`.cloudf`
    /// placeholder suffix, and percent-encodes the result. There is no
    /// upstream CLI command for this — we build the URL ourselves from
    /// `odrive status --mounts` data.
    pub fn web_url(&self, path: &str) -> Result<String, OdriveError> {
        let mounts = self.get_mounts()?;
        build_web_url(path, &mounts)
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
            // Best-effort: tell the file manager to render this folder
            // with the bundled main-folder icon. Failure is non-fatal —
            // a missing `gio` binary or a non-GVFS environment just
            // means Nautilus uses the default folder icon, same as
            // before this hook was added.
            let _ = set_folder_custom_icon(local, MOUNT_FOLDER_ICON_NAME);
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
            // Strip the custom-icon metadata we set on mount. The
            // folder may stay around populated with already-synced
            // files, but it's no longer an odrive mount, so the
            // distinctive icon would be misleading. Best-effort.
            let _ = unset_folder_custom_icon(local);
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

    /// `odrive autotrashthreshold <never|immediately|fifteen|hour|day>`.
    /// Sets the cadence for automatic trash emptying. Persisted by the
    /// agent in `odrive_user_premium_conf.txt` as
    /// `autoEmptyTrashIntervalMins` (with a token-to-minutes mapping
    /// the agent owns internally).
    pub fn auto_trash_threshold(&self, value: AutoTrashThreshold) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("autotrashthreshold")
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

    /// Snapshot of the agent's in-flight counters. Drives the tray
    /// indicator's "currently working" animation. Falls through to the
    /// same `get_status` shell-out that the dashboard already runs, so
    /// adding this poll on top of an existing one doesn't double the
    /// CLI invocation rate when callers reuse `get_status` directly.
    pub fn get_sync_activity(&self) -> Result<SyncActivity, OdriveError> {
        let status = self.get_status()?;
        Ok(parse_sync_activity(&status.sync_status))
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

    /// `odrive backup <local> <remote>` — register a new backup job.
    /// Upstream is one-way local→remote and runs on the agent's
    /// schedule (24 h default). Returns the agent's stdout (usually
    /// empty on success).
    pub fn add_backup_job(&self, local: &str, remote: &str) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("backup")
            .arg(local)
            .arg(remote)
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let msg = if !stderr.is_empty() { stderr } else { stdout };
            return Err(OdriveError::CliError(format!(
                "odrive backup failed: {}",
                msg
            )));
        }
        Ok(stdout)
    }

    /// `odrive removebackup <jobId>` — drop a job. The IPC also
    /// supports removal by `localPath` (the cleaner verb) but the CLI
    /// only exposes the jobId form, and we have the jobId from
    /// `get_backup_jobs()`, so it's a wash.
    pub fn remove_backup_job(&self, job_id: &str) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("removebackup")
            .arg(job_id)
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let msg = if !stderr.is_empty() { stderr } else { stdout };
            return Err(OdriveError::CliError(format!(
                "odrive removebackup failed: {}",
                msg
            )));
        }
        Ok(stdout)
    }

    /// `odrive backupnow` — kick the agent's scheduler immediately.
    /// Bulk only: there's no per-job force-run command in either the
    /// CLI or the IPC. All registered jobs run.
    pub fn backup_now(&self) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path).arg("backupnow").output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let msg = if !stderr.is_empty() { stderr } else { stdout };
            return Err(OdriveError::CliError(format!(
                "odrive backupnow failed: {}",
                msg
            )));
        }
        Ok(stdout)
    }

    /// `odrive status --backups` → list of registered jobs.
    pub fn get_backup_jobs(&self) -> Result<Vec<BackupJob>, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("status")
            .arg("--backups")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let msg = if !stderr.is_empty() { stderr } else { stdout };
            return Err(OdriveError::CliError(format!(
                "odrive status --backups failed: {}",
                msg
            )));
        }
        Ok(parse_backups_status(&String::from_utf8_lossy(&output.stdout)))
    }

    /// `odrive encpassphrase <passphrase> <id> [--initialize]` — set
    /// (or initialize) the local-only passphrase for an encryptor
    /// folder. The agent stores the passphrase in its SEE-encrypted DB
    /// after this call, then transparently decrypts content while it's
    /// running.
    ///
    /// `initialize=true` matches the upstream `--initialize` flag and
    /// is for a brand-new encryptor folder where no passphrase has yet
    /// been set; `initialize=false` is the routine "save the
    /// passphrase the user already chose elsewhere" case. The agent's
    /// IPC test-then-set logic only kicks in when initialize is false:
    /// a wrong passphrase fails the agent-side test and renders an
    /// "encryptor wrong password" dialog (no-op on Linux) without
    /// persisting.
    pub fn set_encryption_passphrase(
        &self,
        passphrase: &str,
        encryptor_id: &str,
        initialize: bool,
    ) -> Result<String, OdriveError> {
        let mut cmd = Command::new(&self.bin_path);
        cmd.arg("encpassphrase");
        if initialize {
            cmd.arg("--initialize");
        }
        cmd.arg(passphrase).arg(encryptor_id);
        let output = cmd.output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let msg = if !stderr.is_empty() { stderr } else { stdout };
            return Err(OdriveError::CliError(format!(
                "odrive encpassphrase failed: {}",
                msg
            )));
        }
        Ok(stdout)
    }

    /// Read `~/.odrive-agent/odrive_user_general_conf.txt` as a
    /// `serde_json::Value`, preserving every key. The agent watches
    /// this file for mtime changes and reloads its in-memory state on
    /// the next poll (~2s), so writing it back is the way to push
    /// settings without the user needing to restart the agent.
    pub fn read_general_conf(&self) -> Result<serde_json::Value, OdriveError> {
        read_conf_file(&self.general_conf_path())
    }

    /// Inverse of `read_general_conf`. Pretty-prints with 4-space
    /// indent, matching the upstream's own writeback format closely
    /// enough that our edits don't churn the diff.
    pub fn write_general_conf(&self, v: &serde_json::Value) -> Result<(), OdriveError> {
        write_conf_file(&self.general_conf_path(), v)
    }

    pub fn read_premium_conf(&self) -> Result<serde_json::Value, OdriveError> {
        read_conf_file(&self.premium_conf_path())
    }

    pub fn write_premium_conf(&self, v: &serde_json::Value) -> Result<(), OdriveError> {
        write_conf_file(&self.premium_conf_path(), v)
    }

    fn general_conf_path(&self) -> String {
        format!("{}/.odrive-agent/odrive_user_general_conf.txt", self.home)
    }

    fn premium_conf_path(&self) -> String {
        format!("{}/.odrive-agent/odrive_user_premium_conf.txt", self.home)
    }

    /// IPC-direct read of `lastBackupTime` and `timeTillNextBackup`.
    /// The CLI's status pretty-printer drops both fields, so the only
    /// way to surface them is to speak the agent's JSON IPC ourselves.
    /// Cheap: opens a TCP socket to 127.0.0.1, sends one line of JSON,
    /// reads one frame, closes. See [`AgentIpc::status`] for the
    /// generic primitive.
    pub fn get_backup_schedule(&self) -> Result<BackupSchedule, OdriveError> {
        let ipc = AgentIpc::new(&self.home);
        let msg = ipc.status()?;
        Ok(BackupSchedule {
            last_backup_time: msg
                .get("lastBackupTime")
                .and_then(|v| v.as_str().map(|s| s.to_string())),
            time_till_next: msg
                .get("timeTillNextBackup")
                .and_then(|v| v.as_str().map(|s| s.to_string())),
        })
    }

    /// `odrive status --trash` → list of trashed items.
    pub fn get_trash_items(&self) -> Result<Vec<TrashItem>, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("status")
            .arg("--trash")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let msg = if !stderr.is_empty() { stderr } else { stdout };
            return Err(OdriveError::CliError(format!(
                "odrive status --trash failed: {}",
                msg
            )));
        }
        Ok(parse_trash_status(&String::from_utf8_lossy(&output.stdout)))
    }

    /// `odrive restoretrash` — bulk restore. The IPC `restoretrash` command
    /// does not accept any item filter (verified end-to-end against a live
    /// agent): every parameter shape — `{}`, `{"path": <p>}`, bogus paths —
    /// dispatches the same `restore_all_deletes()` codepath. Per-item
    /// restore exists in `TrashController.restore_delete(o2Path)` but is
    /// only callable in-process; macOS/Windows desktop GUIs reach it
    /// directly because they're embedded Python, not over a socket.
    pub fn restore_trash(&self) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("restoretrash")
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let msg = if !stderr.is_empty() { stderr } else { stdout };
            return Err(OdriveError::CliError(format!(
                "odrive restoretrash failed: {}",
                msg
            )));
        }
        Ok(stdout)
    }

    /// `odrive emptytrash` — bulk permanent delete. Same shape limit as
    /// `restoretrash`: no per-item form exists.
    pub fn empty_trash(&self) -> Result<String, OdriveError> {
        let output = Command::new(&self.bin_path)
            .arg("emptytrash")
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let msg = if !stderr.is_empty() { stderr } else { stdout };
            return Err(OdriveError::CliError(format!(
                "odrive emptytrash failed: {}",
                msg
            )));
        }
        Ok(stdout)
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
                    if let Err(e) = pad_placeholder(&path) {
                        log::warn!("scan: failed to pad placeholder {}: {}", local_path, e);
                    }
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

/// Ensure a placeholder file is at least one byte. The upstream odrive
/// agent identifies placeholders by their `.cloud` / `.cloudf` extension,
/// not by zero size, so a single null byte is invisible to it. But GLib's
/// content-type resolver hardcodes empty files to `application/x-zerosize`
/// before consulting glob rules, which prevents Nautilus from finding our
/// MIME-typed handler. Padding to one byte lets the glob match win and
/// makes double-click activation work.
///
/// No-op if the file is already non-empty. Returns true if a byte was
/// written, false if already padded.
pub fn pad_placeholder(path: &Path) -> std::io::Result<bool> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > 0 {
        return Ok(false);
    }
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(&[0u8])?;
    Ok(true)
}

/// Icon-theme name registered by `odrive-cli install-handlers` for the
/// odrive-mount-folder rendering. The same constant feeds both ends:
/// the install-handlers code that copies the PNG into the icon theme
/// and the GVFS-metadata setter that points each mount at it.
pub const MOUNT_FOLDER_ICON_NAME: &str = "odrive-mount-folder";

/// Mark `local_path` (typically an odrive mount root) with a custom
/// icon. We hit two channels because GNOME and KDE look in different
/// places:
///
///  - **`.directory` file** in the folder itself, with `[Desktop Entry]
///    Icon=<name>` — FreeDesktop standard, what KDE Plasma / Dolphin
///    read. GNOME also honours it as a fallback.
///  - **GVFS metadata** (`gio set metadata::custom-icon-name`) — what
///    Nautilus reads first for local-path icon resolution. Dolphin
///    doesn't consult GVFS metadata for non-network paths, so for KDE
///    parity we have to write the .directory file too.
///
/// Writing into a watched mount means the agent would happily upload
/// `.directory` to the user's Drive root unless we blacklist it first
/// — which is what `ensure_directory_blacklisted` does. We call that
/// helper here so the per-mount path is safe even if `install-handlers`
/// hasn't run; it sleeps ~3s on the *first* call so the agent's
/// `~2s` mtime-poll has a chance to reload its conf before we drop
/// the file.
///
/// Best-effort. Failures (no `gio` binary, non-GVFS environment,
/// missing premium conf, etc.) are returned as `Err` but every current
/// call site ignores the result — degrading to the default folder
/// icon is preferable to bubbling a metadata error to the user.
pub fn set_folder_custom_icon(local_path: &str, icon_name: &str) -> std::io::Result<()> {
    if let Ok(home) = std::env::var("HOME") {
        // Add `.directory` to the agent's blackListNames so the file we're
        // about to drop doesn't end up uploaded. Best-effort: a missing
        // conf file (agent not installed) just means there's nothing to
        // upload anyway.
        let _ = ensure_directory_blacklisted(&home);
    }

    let dir_file = format!("{}/.directory", local_path);
    let body = format!("[Desktop Entry]\nIcon={}\n", icon_name);
    fs::write(&dir_file, body)?;

    // GVFS metadata for Nautilus. Best-effort — non-zero exit (no gio
    // binary, non-GVFS path, etc.) doesn't justify failing the whole
    // operation since the .directory file alone covers Plasma and is
    // a fallback in GNOME.
    let _ = Command::new("gio")
        .arg("set")
        .arg(local_path)
        .arg("metadata::custom-icon-name")
        .arg(icon_name)
        .status();

    Ok(())
}

/// Ensure `.directory` is in the agent's `blackListNames` so the
/// per-folder marker file we drop into mount roots (`set_folder_custom_icon`)
/// doesn't get uploaded. Idempotent: returns `Ok(true)` only when we
/// actually added the entry, in which case we sleep ~3s before
/// returning so the agent's mtime-based conf reload has time to pick
/// it up before any caller writes the file. On `Ok(false)` (already
/// present) we return immediately. Missing premium conf (agent not
/// installed yet) returns an `Err` the caller can ignore.
pub fn ensure_directory_blacklisted(home: &str) -> std::io::Result<bool> {
    let conf_path = format!("{}/.odrive-agent/odrive_user_premium_conf.txt", home);
    let mut conf = read_conf_file(&conf_path).map_err(|e| {
        std::io::Error::other(format!("read premium conf: {}", e))
    })?;

    let changed = {
        let names_existing = conf
            .get("blackListNames")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().any(|v| v.as_str() == Some(".directory")))
            .unwrap_or(false);
        if names_existing {
            false
        } else if let Some(obj) = conf.as_object_mut() {
            let entry = obj.entry("blackListNames".to_string())
                .or_insert_with(|| serde_json::Value::Array(Vec::new()));
            if let Some(arr) = entry.as_array_mut() {
                arr.push(serde_json::Value::String(".directory".to_string()));
            }
            true
        } else {
            false
        }
    };

    if changed {
        write_conf_file(&conf_path, &conf).map_err(|e| {
            std::io::Error::other(format!("write premium conf: {}", e))
        })?;
        // The agent polls conf mtime every ~2s; sleep slightly longer so
        // the reload has actually fired before we (or our caller) drop
        // the .directory file. Only paid on first-ever call.
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    Ok(changed)
}

/// Inverse of `set_folder_custom_icon`: strip the custom-icon metadata
/// from `local_path`. Called from `unmount` so a folder no longer
/// associated with odrive doesn't keep the distinctive icon.
/// Minimal client for the agent's local JSON-over-TCP IPC. The agent
/// listens on 127.0.0.1:<port> where `<port>` lives in
/// `~/.odrive-agent/.oreg` under `current.protocol`. Each command is a
/// single line of JSON (`{"command": <name>, "parameters": {...}}\n`)
/// and the agent replies with one or more lines of
/// `{"messageType": "Status"|"Error", "message": <value>}`.
///
/// Used in this codebase only for fields the public CLI strips —
/// today, just `lastBackupTime` / `timeTillNextBackup` for the Backup
/// tab's schedule strip. All mutations still go through the CLI so
/// upstream's audit log (`User chose to ...` lines in the agent log)
/// stays accurate.
pub struct AgentIpc {
    oreg_path: String,
}

impl AgentIpc {
    pub fn new(home: &str) -> Self {
        Self {
            oreg_path: format!("{}/.odrive-agent/.oreg", home),
        }
    }

    /// Read the protocol port from `.oreg`. Format:
    /// `{"current": {"protocol": <port>}, ...}`. Returns `None` if the
    /// file is missing, unreadable, or doesn't contain the expected
    /// key — same fault-tolerance posture as `OdriveConfig::load`.
    fn read_port(&self) -> Result<u16, OdriveError> {
        let bytes = fs::read_to_string(&self.oreg_path).map_err(|e| {
            OdriveError::CliError(format!("agent registry not readable: {}", e))
        })?;
        let v: serde_json::Value = serde_json::from_str(&bytes)
            .map_err(|e| OdriveError::Parse(format!("agent registry not JSON: {}", e)))?;
        v.get("current")
            .and_then(|c| c.get("protocol"))
            .and_then(|p| p.as_u64())
            .map(|p| p as u16)
            .ok_or_else(|| OdriveError::Parse("agent registry missing current.protocol".into()))
    }

    /// `{"command": "status"}` → first response frame's `message`
    /// object. Short timeouts (1s connect, 2s read) so the call doesn't
    /// hang the GTK main loop if the agent's wedged.
    pub fn status(&self) -> Result<serde_json::Value, OdriveError> {
        let port = self.read_port()?;
        let addr = format!("127.0.0.1:{}", port);
        let stream_addr: std::net::SocketAddr = addr.parse().map_err(|e: std::net::AddrParseError| {
            OdriveError::Parse(format!("invalid agent address {}: {}", addr, e))
        })?;
        let mut sock = TcpStream::connect_timeout(&stream_addr, Duration::from_secs(1))
            .map_err(|e| OdriveError::CliError(format!("agent IPC connect failed: {}", e)))?;
        sock.set_read_timeout(Some(Duration::from_secs(2)))?;
        sock.set_write_timeout(Some(Duration::from_secs(2)))?;
        let payload = b"{\"command\":\"status\",\"parameters\":{}}\n";
        sock.write_all(payload)?;

        // Read until we have at least one '\n'. The agent's status
        // response is a single frame in practice (one big JSON object)
        // so 64 KB chunks are plenty.
        let mut buf = Vec::with_capacity(64 * 1024);
        let mut chunk = [0u8; 64 * 1024];
        while !buf.contains(&b'\n') {
            let n = sock.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let _ = sock.shutdown(Shutdown::Both);

        let line_end = buf
            .iter()
            .position(|b| *b == b'\n')
            .unwrap_or(buf.len());
        let line = std::str::from_utf8(&buf[..line_end])
            .map_err(|e| OdriveError::Parse(format!("agent IPC non-utf8 response: {}", e)))?;
        let frame: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| OdriveError::Parse(format!("agent IPC bad JSON: {}", e)))?;
        Ok(frame
            .get("message")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }
}

/// Read a JSON conf file (the agent's `odrive_user_*_conf.txt`) into a
/// generic `serde_json::Value` so callers can edit any key without
/// having to model the entire schema. Missing file → empty
/// `Value::Object` (the agent does the same for first-run); malformed
/// JSON → `OdriveError::Parse` so callers can surface a "your conf
/// file is broken" error rather than silently dropping settings.
pub fn read_conf_file(path: &str) -> Result<serde_json::Value, OdriveError> {
    let bytes = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::Value::Object(serde_json::Map::new()));
        }
        Err(e) => return Err(OdriveError::Io(e)),
    };
    if bytes.trim().is_empty() {
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    }
    serde_json::from_str(&bytes)
        .map_err(|e| OdriveError::Parse(format!("conf file {} is not JSON: {}", path, e)))
}

/// Write a JSON value to a conf file. We pretty-print with 4-space
/// indent and append a trailing newline — close enough to the
/// upstream's own writeback (which uses Python's `json.dumps` with
/// `indent=4` and a trailing space after each colon) that the file
/// stays human-readable.
pub fn write_conf_file(path: &str, v: &serde_json::Value) -> Result<(), OdriveError> {
    let mut s = serde_json::to_string_pretty(v)
        .map_err(|e| OdriveError::Parse(format!("can't serialize conf: {}", e)))?;
    s.push('\n');
    fs::write(path, s)?;
    Ok(())
}

pub fn unset_folder_custom_icon(local_path: &str) -> std::io::Result<()> {
    // Drop the .directory file we wrote for KDE. Missing file is fine
    // (best-effort path, e.g. user already removed it manually).
    let dir_file = format!("{}/.directory", local_path);
    if let Err(e) = fs::remove_file(&dir_file) {
        if e.kind() != std::io::ErrorKind::NotFound {
            // Non-NotFound errors bubble — caller decides whether to
            // surface or swallow. We don't blow up on missing.
            return Err(e);
        }
    }
    // Strip GVFS metadata for Nautilus. Best-effort: non-GVFS environment
    // gives no exit signal worth surfacing, since the `.directory`
    // removal already covered the only file artefact.
    let _ = Command::new("gio")
        .args(["set", "-t", "unset", local_path, "metadata::custom-icon-name"])
        .status();
    Ok(())
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

    fn root_mount() -> OdriveMount {
        OdriveMount {
            local_path: "/home/keith/odrive".into(),
            remote_path: "/".into(),
            status: "Active".into(),
        }
    }

    #[test]
    fn percent_encode_path_keeps_path_safe_chars() {
        assert_eq!(percent_encode_path("foo"), "foo");
        assert_eq!(percent_encode_path("Google Drive"), "Google%20Drive");
        assert_eq!(percent_encode_path("a/b c"), "a/b%20c");
        assert_eq!(percent_encode_path("a?b#c&d"), "a%3Fb%23c%26d");
        // UTF-8 bytes for `é` are 0xC3 0xA9.
        assert_eq!(percent_encode_path("café"), "caf%C3%A9");
    }

    #[test]
    fn build_web_url_root_mount_subfolder() {
        let mounts = vec![root_mount()];
        let url = build_web_url("/home/keith/odrive/Google Drive", &mounts).unwrap();
        assert_eq!(url, "https://www.odrive.com/browse/Google%20Drive");
    }

    #[test]
    fn build_web_url_strips_cloud_suffix() {
        let mounts = vec![root_mount()];
        let url =
            build_web_url("/home/keith/odrive/Google Drive/foo.cloud", &mounts).unwrap();
        assert_eq!(url, "https://www.odrive.com/browse/Google%20Drive/foo");
    }

    #[test]
    fn build_web_url_strips_cloudf_suffix() {
        let mounts = vec![root_mount()];
        let url = build_web_url("/home/keith/odrive/Backups.cloudf", &mounts).unwrap();
        assert_eq!(url, "https://www.odrive.com/browse/Backups");
    }

    #[test]
    fn build_web_url_mount_root_itself() {
        let mounts = vec![root_mount()];
        let url = build_web_url("/home/keith/odrive", &mounts).unwrap();
        assert_eq!(url, "https://www.odrive.com/browse/");
    }

    #[test]
    fn build_web_url_non_root_mount() {
        // Mount where remote_path is `/Work`, not the odrive root.
        let mounts = vec![OdriveMount {
            local_path: "/home/keith/work-od".into(),
            remote_path: "/Work".into(),
            status: "Active".into(),
        }];
        let url = build_web_url("/home/keith/work-od/notes.txt", &mounts).unwrap();
        assert_eq!(url, "https://www.odrive.com/browse/Work/notes.txt");
    }

    #[test]
    fn build_web_url_non_root_mount_root_itself() {
        let mounts = vec![OdriveMount {
            local_path: "/home/keith/work-od".into(),
            remote_path: "/Work".into(),
            status: "Active".into(),
        }];
        let url = build_web_url("/home/keith/work-od", &mounts).unwrap();
        assert_eq!(url, "https://www.odrive.com/browse/Work");
    }

    #[test]
    fn build_web_url_path_outside_any_mount_errors() {
        let mounts = vec![root_mount()];
        let err = build_web_url("/tmp/foo.txt", &mounts).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("not inside an odrive mount"), "got: {}", msg);
    }

    #[test]
    fn build_web_url_trailing_slash_normalised() {
        let mounts = vec![root_mount()];
        let url =
            build_web_url("/home/keith/odrive/Google Drive/", &mounts).unwrap();
        assert_eq!(url, "https://www.odrive.com/browse/Google%20Drive");
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
    fn parse_sync_activity_real_status_text_idle() {
        // Real `odrive status` output for an idle agent — every counter
        // is zero, so `is_active()` is false.
        let s = "\
isActivated: True                                               hasSession: True
syncEnabled: True                                                            Mounts: 1

Sync Requests: 0
Background Requests: 0
Uploads: 0
Downloads: 0
Trash: 0
Waiting: 0
Not Allowed: 0
";
        let a = parse_sync_activity(s);
        assert_eq!(a, SyncActivity::default());
        assert!(!a.is_active());
    }

    #[test]
    fn parse_sync_activity_real_status_text_active() {
        // Mid-sync snapshot: a couple of counters non-zero. Trash and
        // Not Allowed are non-zero too but they don't influence is_active.
        let s = "\
Sync Requests: 3
Background Requests: 1
Uploads: 0
Downloads: 12
Trash: 5
Waiting: 2
Not Allowed: 1
";
        let a = parse_sync_activity(s);
        assert_eq!(a.sync_requests, 3);
        assert_eq!(a.background_requests, 1);
        assert_eq!(a.uploads, 0);
        assert_eq!(a.downloads, 12);
        assert_eq!(a.waiting, 2);
        assert!(a.is_active());
    }

    #[test]
    fn parse_sync_activity_only_background_requests_counts_as_active() {
        // Folder-refresh sweep with no user-initiated work — still
        // "active" so the tray animates rather than appearing idle while
        // the agent is clearly doing something.
        let s = "Background Requests: 1\n";
        let a = parse_sync_activity(s);
        assert!(a.is_active());
    }

    #[test]
    fn parse_sync_activity_missing_counters_fall_back_to_zero() {
        // No counter lines at all → every field 0, is_active false.
        let a = parse_sync_activity("isActivated: True\nemail: x@y\n");
        assert_eq!(a, SyncActivity::default());
        assert!(!a.is_active());
    }

    #[test]
    fn parse_sync_activity_non_numeric_value_ignored() {
        // Garbage value → that field stays at 0; the others still parse.
        let s = "Sync Requests: pending\nDownloads: 4\n";
        let a = parse_sync_activity(s);
        assert_eq!(a.sync_requests, 0);
        assert_eq!(a.downloads, 4);
        assert!(a.is_active());
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
    fn folder_sync_threshold_cli_round_trip() {
        // `0` and `inf` are explicit at the CLI; the literal MB value
        // is just the integer.
        assert_eq!(FolderSyncThreshold::None.to_cli_arg(), "0");
        assert_eq!(FolderSyncThreshold::Inf.to_cli_arg(), "inf");
        assert_eq!(FolderSyncThreshold::Mb(100).to_cli_arg(), "100");
        assert_eq!(FolderSyncThreshold::Mb(0).to_cli_arg(), "0");
    }

    #[test]
    fn folder_sync_threshold_db_round_trip() {
        // The DB column uses an i32 with `0`/`-1` sentinels.
        for v in [
            FolderSyncThreshold::None,
            FolderSyncThreshold::Inf,
            FolderSyncThreshold::Mb(1),
            FolderSyncThreshold::Mb(100),
            FolderSyncThreshold::Mb(500),
            FolderSyncThreshold::Mb(123_456),
        ] {
            let encoded = v.to_db_value();
            let decoded = FolderSyncThreshold::from_db_value(encoded);
            assert_eq!(decoded, v, "round-trip for {:?} via {}", v, encoded);
        }
        // Out-of-range negatives degrade to None rather than panic.
        assert_eq!(
            FolderSyncThreshold::from_db_value(-2),
            FolderSyncThreshold::None
        );
    }

    #[test]
    fn pad_placeholder_writes_one_byte_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("foo.cloud");
        std::fs::write(&p, b"").unwrap();
        let padded = pad_placeholder(&p).unwrap();
        assert!(padded);
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(bytes, vec![0u8]);
    }

    #[test]
    fn pad_placeholder_skips_already_padded() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("foo.cloud");
        std::fs::write(&p, b"\0").unwrap();
        let padded = pad_placeholder(&p).unwrap();
        assert!(!padded);
        // Content must be untouched — this is what protects post-sync
        // files (which may be non-empty real content) if a stray scan
        // hits them before the .cloud suffix is stripped.
        assert_eq!(std::fs::read(&p).unwrap(), vec![0u8]);
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

    #[test]
    fn parse_trash_status_empty_sentinel() {
        // The agent's TrashStatus controller emits "No trash.\n" when the
        // trash list is empty — not a path, so the parser must drop it.
        let items = parse_trash_status("No trash.\n");
        assert!(items.is_empty());
    }

    #[test]
    fn parse_trash_status_single_item() {
        let items = parse_trash_status("/home/keith/odrive/Google Drive/testfile.txt\n");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].local_path,
            "/home/keith/odrive/Google Drive/testfile.txt"
        );
    }

    #[test]
    fn parse_trash_status_multiple_items_with_blank_lines() {
        let s = "\n/home/keith/odrive/Google Drive/a.txt\n\
                 /home/keith/odrive/Google Drive/sub/b.txt\n\
                 \n\
                 /home/keith/odrive/OneDrive/c.txt\n";
        let items = parse_trash_status(s);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].local_path, "/home/keith/odrive/Google Drive/a.txt");
        assert_eq!(
            items[1].local_path,
            "/home/keith/odrive/Google Drive/sub/b.txt"
        );
        assert_eq!(items[2].local_path, "/home/keith/odrive/OneDrive/c.txt");
    }

    #[test]
    fn conf_file_round_trip_preserves_unknown_keys() {
        // Real shape from `odrive_user_general_conf.txt`. Round-tripping
        // through read/write must keep every key, even ones we don't
        // surface in the GUI — losing them would silently revert agent
        // config the user set elsewhere.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("conf.txt");
        let path_s = path.to_string_lossy().into_owned();
        let original = serde_json::json!({
            "allowFlagged": false,
            "localScanIntervalSecs": 1800,
            "processMemoryLimitMBytes": 3584,
            "someFutureKeyWeDontKnowYet": "bar",
            "nestedObject": {"a": 1, "b": [2, 3]},
        });
        write_conf_file(&path_s, &original).unwrap();
        let reread = read_conf_file(&path_s).unwrap();
        assert_eq!(reread, original);
    }

    #[test]
    fn conf_file_missing_returns_empty_object() {
        // First-run: the agent hasn't written its defaults yet. We
        // shouldn't error — the caller can layer their own defaults
        // and write back, matching the agent's own behaviour.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("not-here.txt");
        let v = read_conf_file(&path.to_string_lossy()).unwrap();
        assert!(v.is_object() && v.as_object().unwrap().is_empty());
    }

    #[test]
    fn conf_file_malformed_returns_parse_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.txt");
        fs::write(&path, "{this is not, json").unwrap();
        let err = read_conf_file(&path.to_string_lossy()).unwrap_err();
        assert!(matches!(err, OdriveError::Parse(_)));
    }

    #[test]
    fn parse_global_settings_includes_auto_trash() {
        let s = "\
placeholderThreshold: medium
xlThreshold: large
autoUnsyncThreshold: week
autoTrashThreshold: hour
syncEnabled: True
";
        let g = parse_global_settings(s);
        assert_eq!(g.placeholder, PlaceholderThreshold::Medium);
        assert_eq!(g.xl, XlThreshold::Large);
        assert_eq!(g.auto_unsync, AutoUnsyncThreshold::Week);
        assert_eq!(g.auto_trash, AutoTrashThreshold::Hour);
        assert!(g.sync_enabled);
    }

    #[test]
    fn parse_backups_status_empty_sentinel() {
        let jobs = parse_backups_status("No backup jobs.\n");
        assert!(jobs.is_empty());
    }

    #[test]
    fn parse_backups_status_single_job() {
        // The CLI emits a leading and trailing blank line plus the
        // four labelled fields per the BackupsStatus formatter.
        let s = "\nID: abc-123\nLocal Path: /home/keith/Documents\n\
                 Remote Path: /Google Drive/Backups/Documents\nStatus: ACTIVE\n\n";
        let jobs = parse_backups_status(s);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_id, "abc-123");
        assert_eq!(jobs[0].local_path, "/home/keith/Documents");
        assert_eq!(jobs[0].remote_path, "/Google Drive/Backups/Documents");
        assert_eq!(jobs[0].status, "ACTIVE");
    }

    #[test]
    fn parse_backups_status_multiple_jobs() {
        let s = "\nID: aaa\nLocal Path: /home/k/A\nRemote Path: /R/A\nStatus: ACTIVE\n\n\
                 ID: bbb\nLocal Path: /home/k/B\nRemote Path: /R/B\nStatus: PAUSED\n\n";
        let jobs = parse_backups_status(s);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].job_id, "aaa");
        assert_eq!(jobs[0].status, "ACTIVE");
        assert_eq!(jobs[1].job_id, "bbb");
        assert_eq!(jobs[1].status, "PAUSED");
    }

    #[test]
    fn parse_backups_status_back_to_back_records_without_blank_line() {
        // Defensive: if the formatter ever drops the blank separator,
        // a fresh `ID:` while we already hold one should flush the old
        // record before starting the new one. (Mirrors how parse_mounts
        // tolerates orphaned remote lines.)
        let s = "ID: aaa\nLocal Path: /A\nRemote Path: /RA\nStatus: ACTIVE\n\
                 ID: bbb\nLocal Path: /B\nRemote Path: /RB\nStatus: ACTIVE\n";
        let jobs = parse_backups_status(s);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].job_id, "aaa");
        assert_eq!(jobs[1].job_id, "bbb");
    }

    #[test]
    fn parse_backups_status_missing_fields_filled_blank() {
        // Status line missing → empty string in the struct, NOT an
        // error. The agent always emits the full set, but a partial
        // shouldn't panic the parser or skip the record.
        let s = "ID: solo\nLocal Path: /L\nRemote Path: /R\n\n";
        let jobs = parse_backups_status(s);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_id, "solo");
        assert_eq!(jobs[0].status, "");
    }

    #[test]
    fn parse_trash_status_no_trash_among_real_lines_still_filtered() {
        // Defensive: if the agent ever surrounded "No trash." with other
        // banner text we still filter that exact sentinel string. Real
        // paths next to it parse normally.
        let s = "No trash.\n/home/keith/odrive/x.txt\n";
        let items = parse_trash_status(s);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].local_path, "/home/keith/odrive/x.txt");
    }
}

