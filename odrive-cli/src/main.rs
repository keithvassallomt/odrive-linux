use odrive_core::{OdriveAgent, OdriveDb};
use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Show current odrive status
    Status,
    /// List active mounts
    Mounts {
        /// Print only local mount paths, one per line (machine-readable).
        #[arg(long)]
        paths: bool,
    },
    /// Start the odrive agent
    Start,
    /// Stop the odrive agent
    Stop,
    /// Sync a file or folder
    Sync {
        /// Path to the placeholder or folder
        path: String,
    },
    /// Unsync a file or folder
    Unsync {
        /// Path to the file or folder
        path: String,
    },
    /// Refresh a folder
    Refresh {
        /// Path to the folder
        path: String,
    },
    /// Generate a share link for a file or folder. Prints the URL on stdout.
    Sharelink {
        /// Path to the item (placeholder or materialised) to share
        path: String,
    },
    /// Generate share links for one or more paths and copy them to the system
    /// clipboard (newline-joined). Used by the Dolphin "Copy Share Link"
    /// service-menu action; equivalent to the GTK in-process clipboard write
    /// the Nautilus extension does. Falls back to wl-copy / xclip depending
    /// on the session.
    CopyShareLink {
        /// One or more paths under an odrive mount
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Compose the odrive web-app URL for a local path. Prints the URL on stdout.
    Weburl {
        /// Path to the item under an odrive mount
        path: String,
    },
    /// Compose the odrive web-app URL for a path and xdg-open it in the
    /// browser. Used by the Dolphin "Open Web Preview" service-menu action;
    /// equivalent to the Nautilus extension's `weburl + xdg-open` pipeline.
    OpenWebPreview {
        /// Path to the item under an odrive mount
        path: String,
    },
    /// Scan for placeholders and update the database
    Scan {
        /// Mount path to scan (defaults to ~/odrive)
        path: Option<String>,
    },
    /// Sync a placeholder, then open the materialized result with xdg-open.
    /// Used as the handler for the .cloud / .cloudf MIME types registered by
    /// `install-handlers`; safe to invoke directly too.
    Open {
        /// Path to the .cloud or .cloudf placeholder
        path: String,
    },
    /// Register MIME types for *.cloud / *.cloudf and set this binary as the
    /// default handler so double-click in Nautilus materializes and opens
    /// placeholders. Writes to ~/.local/share/{mime,applications}; reversible
    /// via `uninstall-handlers`.
    InstallHandlers,
    /// Remove the MIME / desktop registrations written by `install-handlers`.
    UninstallHandlers,
    /// Materialise the static package payload (icons in hicolor layout, MIME
    /// XML, .desktop files with system paths, Nautilus extension) under
    /// `<DST><prefix>/share/...`. Used by the .deb / .rpm builds; not for
    /// end users. Idempotent.
    PreparePayload {
        /// Destination root. The tree lands at `<DST><prefix>/share/...`.
        dst: String,
        /// Install prefix used in .desktop Exec lines. Default `/usr`
        /// (Debian/Fedora convention).
        #[arg(long, default_value = "/usr")]
        prefix: String,
    },
    /// Per-user setup that .deb / .rpm post-install hooks can't do: pad
    /// zero-byte placeholders so MIME resolution stops returning
    /// `x-zerosize`, apply the mount-folder icon to existing mounts, and
    /// set the packaged opener as the xdg-mime default for placeholder
    /// MIMEs. Idempotent — re-run after upgrades.
    Setup,
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();
    let agent = OdriveAgent::new();

    match cli.command {
        Some(Commands::Status) | None => {
            match agent.get_status() {
                Ok(status) => {
                    if status.is_running {
                        println!("Agent is running!");
                        let count = match OdriveDb::open(agent.get_db_path()) {
                            Ok(db) => db.count_placeholders().unwrap_or(0),
                            Err(e) => {
                                eprintln!(
                                    "Warning: could not open state DB at {} ({}); placeholder count unavailable.",
                                    agent.get_db_path(),
                                    e,
                                );
                                0
                            }
                        };
                        println!("Tracked Placeholders: {}", count);
                        println!("\nCLI Status Output:\n{}", status.sync_status);
                    } else {
                        println!("Agent is NOT running or unable to connect.");
                    }
                }
                Err(e) => eprintln!("Error getting status: {}", e),
            }
        }
        Some(Commands::Mounts { paths }) => {
            match agent.get_mounts() {
                Ok(mounts) => {
                    if paths {
                        for mount in mounts {
                            println!("{}", mount.local_path);
                        }
                    } else if mounts.is_empty() {
                        println!("No active mounts found.");
                    } else {
                        println!("{:<40} {:<20} {:<10}", "Local Path", "Remote Path", "Status");
                        println!("{}", "-".repeat(70));
                        for mount in mounts {
                            println!("{:<40} {:<20} {:<10}", mount.local_path, mount.remote_path, mount.status);
                        }
                    }
                }
                Err(e) => eprintln!("Error getting mounts: {}", e),
            }
        }
        Some(Commands::Start) => {
            println!("Starting agent...");
            match agent.start() {
                Ok(_) => println!("Agent started."),
                Err(e) => eprintln!("Failed to start: {}", e),
            }
        }
        Some(Commands::Stop) => {
            println!("Stopping agent...");
            match agent.stop() {
                Ok(_) => println!("Agent stopped."),
                Err(e) => eprintln!("Failed to stop: {}", e),
            }
        }
        Some(Commands::Sync { path }) => {
            println!("Syncing {}...", path);
            match agent.sync(&path) {
                Ok(out) => println!("Done: {}", out),
                Err(e) => eprintln!("Sync failed: {}", e),
            }
        }
        Some(Commands::Unsync { path }) => {
            println!("Unsyncing {}...", path);
            match agent.unsync(&path) {
                Ok(out) => println!("Done: {}", out),
                Err(e) => eprintln!("Unsync failed: {}", e),
            }
        }
        Some(Commands::Sharelink { path }) => {
            match agent.share_link(&path) {
                Ok(url) => println!("{}", url),
                Err(e) => {
                    eprintln!("sharelink failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Weburl { path }) => {
            match agent.web_url(&path) {
                Ok(url) => println!("{}", url),
                Err(e) => {
                    eprintln!("weburl failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::CopyShareLink { paths }) => {
            if let Err(e) = copy_share_link(&agent, &paths) {
                eprintln!("copy-share-link failed: {}", e);
                std::process::exit(1);
            }
        }
        Some(Commands::OpenWebPreview { path }) => {
            if let Err(e) = open_web_preview(&agent, &path) {
                eprintln!("open-web-preview failed: {}", e);
                std::process::exit(1);
            }
        }
        Some(Commands::Refresh { path }) => {
            println!("Refreshing {}...", path);
            match agent.refresh(&path) {
                Ok(out) => println!("Done: {}", out),
                Err(e) => eprintln!("Refresh failed: {}", e),
            }
        }
        Some(Commands::Scan { path }) => {
            let scan_path = path.unwrap_or_else(|| agent.default_mount_path());
            println!("Scanning {} for placeholders...", scan_path);
            match agent.scan_placeholders(&scan_path) {
                Ok(count) => println!("Found and tracked {} placeholders.", count),
                Err(e) => eprintln!("Scan failed: {}", e),
            }
        }
        Some(Commands::Open { path }) => {
            // Sync the placeholder, then xdg-open the materialized result.
            // `odrive sync foo.cloud` produces `foo`; `odrive sync foo.cloudf`
            // expands to a real `foo` directory containing more placeholders.
            // For non-placeholder paths we still pass through to xdg-open so
            // accidental invocation doesn't fail loudly.
            if path.ends_with(".cloud") || path.ends_with(".cloudf") {
                println!("Syncing {}...", path);
                if let Err(e) = agent.sync(&path) {
                    eprintln!("Sync failed: {}", e);
                    std::process::exit(1);
                }
            }
            let target = strip_placeholder_suffix(&path);
            if let Err(e) = std::process::Command::new("xdg-open").arg(&target).spawn() {
                eprintln!("xdg-open failed: {}", e);
                std::process::exit(1);
            }
        }
        Some(Commands::InstallHandlers) => {
            if let Err(e) = install_handlers() {
                eprintln!("install-handlers failed: {}", e);
                std::process::exit(1);
            }
            // Pad existing zero-byte placeholders so MIME resolution stops
            // returning x-zerosize. New placeholders get padded as the
            // Nautilus extension's update_file_info encounters them.
            match agent.get_mounts() {
                Ok(mounts) => {
                    for m in mounts {
                        match agent.scan_placeholders(&m.local_path) {
                            Ok(n) => println!("Padded/tracked {} placeholders under {}.", n, m.local_path),
                            Err(e) => eprintln!("Scan failed for {}: {}", m.local_path, e),
                        }
                    }
                }
                Err(e) => eprintln!("Could not enumerate mounts to pad: {}", e),
            }
        }
        Some(Commands::UninstallHandlers) => {
            if let Err(e) = uninstall_handlers() {
                eprintln!("uninstall-handlers failed: {}", e);
                std::process::exit(1);
            }
        }
        Some(Commands::PreparePayload { dst, prefix }) => {
            if let Err(e) = prepare_payload(&dst, &prefix) {
                eprintln!("prepare-payload failed: {}", e);
                std::process::exit(1);
            }
        }
        Some(Commands::Setup) => {
            if let Err(e) = run_setup(&agent) {
                eprintln!("setup failed: {}", e);
                std::process::exit(1);
            }
        }
    }
}

/// Per-user finalisation step for packaged installs (and equivalent to the
/// per-user portion of `install-handlers`). Walks every mount the agent
/// reports and:
///   - pads zero-byte placeholders via `scan_placeholders`, so GLib's
///     `g_content_type_guess` resolves `*.cloud` / `*.cloudf` against our
///     globs instead of `application/x-zerosize`;
///   - tags the mount root with `MOUNT_FOLDER_ICON_NAME` via
///     `set_folder_custom_icon` (which writes both `.directory` for
///     Plasma and the GVFS `metadata::custom-icon-name` attribute for
///     Nautilus);
///   - sets `DESKTOP_NAME` as the xdg-mime default for the two
///     placeholder MIMEs in the user's `~/.config/mimeapps.list`.
///
/// Idempotent: re-running after a package upgrade (or after adding a new
/// mount) re-applies what's needed and silently no-ops on what's already
/// in place. None of the side-effects are destructive.
fn run_setup(agent: &OdriveAgent) -> Result<(), Box<dyn std::error::Error>> {
    let mut padded = 0usize;
    let mut tagged = 0usize;
    match agent.get_mounts() {
        Ok(mounts) => {
            for m in mounts {
                if odrive_core::set_folder_custom_icon(
                    &m.local_path,
                    odrive_core::MOUNT_FOLDER_ICON_NAME,
                )
                .is_ok()
                {
                    tagged += 1;
                }
                match agent.scan_placeholders(&m.local_path) {
                    Ok(n) => padded += n,
                    Err(e) => eprintln!("Scan failed for {}: {}", m.local_path, e),
                }
            }
        }
        Err(e) => eprintln!("Could not enumerate mounts: {}", e),
    }

    for mime in &[MIME_FILE, MIME_FOLDER] {
        let _ = std::process::Command::new("xdg-mime")
            .args(["default", DESKTOP_NAME, mime])
            .status();
    }

    println!(
        "Setup complete: padded/tracked {} placeholder(s), tagged {} mount(s).",
        padded, tagged
    );
    println!("Restart your file manager to pick up icon changes (`nautilus -q` / `dolphin --quit; dolphin &`).");
    Ok(())
}

fn strip_placeholder_suffix(path: &str) -> String {
    if let Some(s) = path.strip_suffix(".cloud") {
        s.to_string()
    } else if let Some(s) = path.strip_suffix(".cloudf") {
        s.to_string()
    } else {
        path.to_string()
    }
}

/// Best-effort desktop notification via `notify-send` (libnotify). KDE,
/// GNOME, and most other DEs ship this; if it's missing we silently skip
/// — the action's primary effect (clipboard write or browser launch)
/// already happened, so the missing notification is cosmetic.
fn notify(summary: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "odrive", "-i", APP_LAUNCHER_ICON])
        .arg(summary)
        .arg(body)
        .status();
}

/// Generate share links for each path and place the newline-joined list on
/// the system clipboard. Wayland sessions get `wl-copy`; X11 sessions get
/// `xclip`. We don't have GTK in-process here (this is the headless CLI
/// path used by Dolphin's service menu), so we shell out — Nautilus's
/// in-process Gdk.Clipboard.set_content path is the equivalent on the
/// Python extension side. A success notification confirms the copy since
/// service-menu actions don't surface stdout.
fn copy_share_link(agent: &OdriveAgent, paths: &[String]) -> Result<(), String> {
    let mut urls: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for p in paths {
        match agent.share_link(p) {
            Ok(url) => {
                let trimmed = url.trim();
                if !trimmed.is_empty() {
                    urls.push(trimmed.to_string());
                }
            }
            Err(e) => errors.push(format!("{}: {}", p, e)),
        }
    }

    if urls.is_empty() {
        let detail = if errors.is_empty() {
            "No share links were generated.".to_string()
        } else {
            errors.join("\n")
        };
        notify("odrive — share link failed", &detail);
        return Err(detail);
    }

    let text = urls.join("\n");
    write_clipboard(&text)?;

    let summary = if urls.len() == 1 {
        "Share link copied".to_string()
    } else {
        format!("{} share links copied", urls.len())
    };
    let body = if errors.is_empty() {
        text.clone()
    } else {
        format!("{}\n\nFailed:\n{}", text, errors.join("\n"))
    };
    notify(&summary, &body);
    Ok(())
}

/// Pipe `text` to wl-copy (Wayland) or xclip (X11). Returns a string
/// error if the chosen tool is missing or exits non-zero — the caller
/// surfaces that as a notification + non-zero exit so `Exec=` invocations
/// don't silently lose data.
fn write_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    let on_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let (tool, args): (&str, &[&str]) = if on_wayland {
        ("wl-copy", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    };

    let mut child = std::process::Command::new(tool)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{} not available ({}). Install wl-clipboard (Wayland) or xclip (X11).", tool, e))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("write to {}: {}", tool, e))?;
    }
    let status = child
        .wait()
        .map_err(|e| format!("wait for {}: {}", tool, e))?;
    if !status.success() {
        return Err(format!("{} exited with status {}", tool, status));
    }
    Ok(())
}

/// Compose the odrive web URL for `path` and xdg-open it. Surfaces any
/// failure as a notification so users right-clicking from Dolphin don't
/// just see nothing happen.
fn open_web_preview(agent: &OdriveAgent, path: &str) -> Result<(), String> {
    let url = agent
        .web_url(path)
        .map_err(|e| {
            let msg = format!("{}: {}", path, e);
            notify("odrive — Open Web Preview failed", &msg);
            msg
        })?;
    std::process::Command::new("xdg-open")
        .arg(&url)
        .spawn()
        .map_err(|e| {
            let msg = format!("xdg-open: {}", e);
            notify("odrive — Open Web Preview failed", &msg);
            msg
        })?;
    Ok(())
}

const MIME_FILE: &str = "application/vnd.odrive.placeholder-file";
const MIME_FOLDER: &str = "application/vnd.odrive.placeholder-folder";
const MIME_XML_NAME: &str = "odrive-linux.xml";
const DESKTOP_NAME: &str = "odrive-linux-open.desktop";

/// Legacy KDE service-menu .desktop name. We previously wrote a static
/// service menu here; the integration is now handled by the C++
/// `KFileItemActionPlugin` shipped from `dolphin-plugin/`. We keep the
/// constant only so `uninstall-handlers` can sweep the legacy file from
/// systems that ran an earlier `install-handlers`.
const LEGACY_SERVICEMENU_NAME: &str = "odrive-linux.desktop";

/// Icon names bound to the two generic placeholder MIME types. Concrete
/// `.gdocx.cloud`-style sub-MIMEs override these via their own `<icon>`
/// elements; a plain `*.cloud` / `*.cloudf` resolves to the parent MIME
/// and these icons.
const PLACEHOLDER_FILE_ICON: &str = "odrive-cloud-file";
const PLACEHOLDER_FOLDER_ICON: &str = "odrive-cloud-folder";

/// Icon name installed under `apps/` for use as the parent label of the
/// Nautilus right-click "Odrive ▸" submenu. Bundled at 16/32/256/512/1024
/// from `odrive-icons/app-icon/`.
const APP_MENU_ICON: &str = "odrive-menu";

/// Icon name + .desktop file used by the GTK launcher (taskbar, app grid,
/// window decorations). The icon name matches the GUI's `application_id`
/// so `gtk::Window::set_default_icon_name(APP_LAUNCHER_ICON)` resolves
/// against the same hicolor entries this installer writes. Same source
/// PNGs as `APP_MENU_ICON` — both map to the odrive infinity logo — but
/// landing under separate target names lets uninstall sweep them
/// independently.
const APP_LAUNCHER_ICON: &str = "io.github.keithvassallomt.odrive-linux";
const APP_DESKTOP_NAME: &str = "io.github.keithvassallomt.odrive-linux.desktop";

/// Icon name for the mascot illustration shown in the GUI's About
/// dialog. Single bundled source PNG (`odrive-icons/odrive-linux-mascot.png`)
/// is non-square (1536×1024) so we don't try to feed it to the standard
/// install_icon_set / hicolor sized-bucket pipeline; `install_mascot_icon`
/// drops it once at `1024x1024/apps/<name>.png` and lets GTK's icon
/// loader scale it at render time.
const MASCOT_ICON_NAME: &str = "odrive-linux-mascot";

/// Cloud-file-type sub-MIMEs. Each entry: (icons subdir, mime/icon stem,
/// glob patterns). The MIME stems become `application/vnd.odrive.<stem>-cloud`
/// (e.g. `gdoc-cloud`); icons under `~/.local/share/icons/hicolor/<size>/mimetypes/`
/// land as `odrive-<stem>-cloud.png` and are referenced via `<icon name>` so
/// the FreeDesktop slash-to-dash naming convention doesn't constrain us.
/// All sub-types are sub-class-of the placeholder-file MIME so the .desktop
/// handler installed below still applies on double-click.
const CLOUD_TYPES: &[(&str, &str, &[&str])] = &[
    ("gdoc",    "gdoc",    &["*.gdoc.cloud", "*.gdocx.cloud"]),
    ("gsheet",  "gsheet",  &["*.gsheet.cloud", "*.gsheetx.cloud"]),
    ("gslides", "gslides", &["*.gslides.cloud", "*.gslidesx.cloud"]),
    ("gdraw",   "gdraw",   &["*.gdraw.cloud"]),
    ("gform",   "gform",   &["*.gform.cloud"]),
    ("gmap",    "gmap",    &["*.gmap.cloud", "*.gmaps.cloud"]),
    ("onenote", "onenote", &["*.one.cloud", "*.onepkg.cloud", "*.onetoc.cloud", "*.onetoc2.cloud"]),
];

const EMBLEMS: &[(&str, &str)] = &[
    ("synced",  "odrive-synced"),
    ("syncing", "odrive-syncing"),
    ("locked",  "odrive-locked"),
];

/// Tray-icon colour variants. Each entry: (colour name, target icon stem
/// installed under hicolor/<size>/status/). The colour name is what
/// `OdriveConfig::tray_icon_color` stores; the target stem is what the
/// indicator passes to `IconTheme::lookup_icon`. The asset bundle is
/// asymmetric: pink and darkgrey ship a per-colour subdirectory with
/// 256/512/1024 sized PNGs, while black/grey/white only ship a single
/// large master at the top of `tray-icons/static/`. `install_tray_icon`
/// handles both layouts.
const TRAY_COLORS: &[&str] = &["pink", "white", "black", "darkgrey", "grey"];

/// Number of animation frames per colour bundled in `odrive-icons/tray-icons/animated/<color>/`.
/// Colours without an `animated/<color>/` directory are simply not animated;
/// `install_tray_animation` returns 0 for them. The GUI's animation timer
/// detects "no frames installed" by checking whether `odrive-tray-<color>-active-1`
/// resolves to a file path on disk.
const TRAY_ANIMATION_FRAMES: u32 = 16;

fn xdg_data_home() -> String {
    std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").expect("$HOME must be set");
        format!("{}/.local/share", home)
    })
}

/// Walk up from the running binary looking for a sibling `odrive-icons/`
/// directory (workspace layout). Returns None if not found — install-handlers
/// continues without icons in that case.
fn find_icons_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut cur = exe.as_path();
    while let Some(parent) = cur.parent() {
        let candidate = parent.join("odrive-icons");
        if candidate.is_dir() {
            return Some(candidate);
        }
        cur = parent;
    }
    None
}

/// Parse a size from a filename like `synced_256x256x32.png` → 256.
/// Falls back to None if the pattern doesn't match.
fn parse_icon_size(file_stem: &str) -> Option<u32> {
    // Take the last `_NxNx32` chunk; the trailing `x32` is colour depth.
    let last = file_stem.rsplit('_').next()?;
    let n = last.split('x').next()?;
    n.parse().ok()
}

/// Copy every PNG under `src_dir` into hicolor's `<size>x<size>/<category>/<target>.png`,
/// using the size embedded in the filename. Skips files whose names don't match
/// our pattern. Returns the count copied.
fn install_icon_set(
    src_dir: &std::path::Path,
    hicolor: &str,
    category: &str,
    target_name: &str,
) -> std::io::Result<usize> {
    if !src_dir.is_dir() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in std::fs::read_dir(src_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("png") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let Some(size) = parse_icon_size(stem) else { continue };
        let dst_dir = format!("{}/{}x{}/{}", hicolor, size, size, category);
        std::fs::create_dir_all(&dst_dir)?;
        let dst = format!("{}/{}.png", dst_dir, target_name);
        std::fs::copy(&path, &dst)?;
        count += 1;
    }
    Ok(count)
}

/// Copy the smallest source PNG in `src_dir` (parsed via the
/// `parse_icon_size` underscore-separated filename convention) into a
/// hardcoded list of small-size hicolor buckets under `category`.
/// Used for emblems, where Plasma's overlay renderer demands a small
/// bucket but the asset bundle only ships large masters; same trick
/// the tray-icon install uses for SNI panel hosts. Returns the count
/// of files copied. A missing / empty src dir yields `Ok(0)` — same
/// defensive shape as the other icon installers.
fn install_small_size_shims(
    src_dir: &std::path::Path,
    hicolor: &str,
    category: &str,
    target_name: &str,
) -> std::io::Result<usize> {
    if !src_dir.is_dir() {
        return Ok(0);
    }
    let mut smallest: Option<(u32, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(src_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("png") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let Some(size) = parse_icon_size(stem) else { continue };
        match &smallest {
            None => smallest = Some((size, path)),
            Some((cur, _)) if size < *cur => smallest = Some((size, path)),
            _ => {}
        }
    }
    let Some((_, src)) = smallest else {
        return Ok(0);
    };
    let mut count = 0usize;
    for size in ["16x16", "22x22", "48x48"] {
        let dst_dir = format!("{}/{}/{}", hicolor, size, category);
        std::fs::create_dir_all(&dst_dir)?;
        std::fs::copy(&src, format!("{}/{}.png", dst_dir, target_name))?;
        count += 1;
    }
    Ok(count)
}

/// Install the tray-icon variants for one colour. Tries the per-colour
/// subdirectory first (`tray-icons/static/infinity-<color>/` with sized
/// PNGs); if that's empty/missing, falls back to the single root master
/// (`tray-icons/static/infinity-<color>.png`).
///
/// Returns the count of files copied.
///
/// **Why we also write to `48x48/status/`:** SNI hosts on GNOME (notably
/// the `status-tray` extension and the older `appindicator` one) walk a
/// hardcoded list of panel-relevant size buckets — `48x48`, `32x32`,
/// `24x24`, `22x22`, `16x16` — when looking up an `IconName` against the
/// icon theme. They never check `256x256+/`. If we only deposit the
/// hi-res masters (which is what the source bundle ships) the panel
/// finds nothing and the icon goes blank, even though the GTK icon-theme
/// cache indexes our name correctly. Copying the smallest-available
/// source into `48x48/status/<name>.png` puts at least one file inside
/// the host's search list. The dimensions don't have to match — both
/// `gtk-update-icon-cache` and St.Icon load by file path and scale at
/// render time — so a 256-px PNG sitting under `48x48/` is a valid
/// "best available" shim.
fn install_tray_icon(
    icons_dir: &std::path::Path,
    hicolor: &str,
    color: &str,
) -> std::io::Result<usize> {
    let target = format!("odrive-tray-{}", color);
    let mut count = 0usize;

    let subdir = icons_dir
        .join("tray-icons")
        .join("static")
        .join(format!("infinity-{}", color));
    if subdir.is_dir() {
        count += install_icon_set(&subdir, hicolor, "status", &target)?;
    }

    let master = icons_dir
        .join("tray-icons")
        .join("static")
        .join(format!("infinity-{}.png", color));
    if count == 0 && master.is_file() {
        // No per-size subdir — drop the master at the largest sensible
        // bucket so high-DPI panels and "show on the bus" tooling pick
        // it up.
        let dst_dir = format!("{}/256x256/status", hicolor);
        std::fs::create_dir_all(&dst_dir)?;
        let dst = format!("{}/{}.png", dst_dir, target);
        std::fs::copy(&master, &dst)?;
        count += 1;
    }

    // Always also write a copy under 48x48/status/. Pick the smallest
    // available source file to minimise download size during decode.
    if let Some(panel_src) = panel_source_for(&subdir, &master) {
        let dst_dir = format!("{}/48x48/status", hicolor);
        std::fs::create_dir_all(&dst_dir)?;
        let dst = format!("{}/{}.png", dst_dir, target);
        std::fs::copy(&panel_src, &dst)?;
        count += 1;
    }

    Ok(count)
}

/// Install the bundled main-folder icon under hicolor's `places`
/// category as the name `odrive-mount-folder` (= `MOUNT_FOLDER_ICON_NAME`
/// in odrive-core, which `OdriveAgent::mount` references when setting
/// per-folder GVFS metadata). The asset is a single 512×512 PNG, so
/// we drop copies into 512/256/48 size dirs — Nautilus picks the best
/// fit at render time and the 48 dir doubles as the small list-view
/// rendering size. `places/` is the canonical hicolor category for
/// folder-style icons (e.g. `folder-pictures`, `folder-documents`).
///
/// Returns the count of files copied. A missing source PNG yields
/// `Ok(0)` — same defensive shape as the other icon installers.
fn install_mount_folder_icon(
    icons_dir: &std::path::Path,
    hicolor: &str,
) -> std::io::Result<usize> {
    let src = icons_dir
        .join("mime_icons")
        .join("main-folder")
        .join("main-folder-512x512.png");
    if !src.is_file() {
        return Ok(0);
    }
    let mut count = 0usize;
    for size in ["512x512", "256x256", "48x48"] {
        let dst_dir = format!("{}/{}/places", hicolor, size);
        std::fs::create_dir_all(&dst_dir)?;
        std::fs::copy(
            &src,
            format!("{}/{}.png", dst_dir, odrive_core::MOUNT_FOLDER_ICON_NAME),
        )?;
        count += 1;
    }
    Ok(count)
}

/// Install the bundled cloud-file or cloud-folder placeholder icon under
/// hicolor's `mimetypes/` category as `target_name`. Source filenames
/// follow the dash-separated `<stem>-NxN.png` shape (with byte-identical
/// `@2x` duplicates the asset bundle ships) — we skip the `@2x` files
/// and deposit each remaining sized PNG into its own size bucket plus a
/// 48x48 list-view shim. A missing source directory yields `Ok(0)`,
/// matching the other icon installers' defensive shape.
fn install_placeholder_icon(
    icons_dir: &std::path::Path,
    hicolor: &str,
    src_subdir: &str,
    target_name: &str,
) -> std::io::Result<usize> {
    let src_dir = icons_dir.join("mime_icons").join(src_subdir);
    if !src_dir.is_dir() {
        return Ok(0);
    }
    let mut sized: Vec<(u32, std::path::PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&src_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("png") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        if stem.contains("@2x") {
            continue;
        }
        let Some(size) = parse_dashed_icon_size(stem) else { continue };
        sized.push((size, path));
    }
    if sized.is_empty() {
        return Ok(0);
    }
    sized.sort_by_key(|(s, _)| *s);
    let mut count = 0usize;
    for (size, path) in &sized {
        let dst_dir = format!("{}/{}x{}/mimetypes", hicolor, size, size);
        std::fs::create_dir_all(&dst_dir)?;
        std::fs::copy(path, format!("{}/{}.png", dst_dir, target_name))?;
        count += 1;
    }
    if let Some((_, smallest)) = sized.first() {
        let dst_dir = format!("{}/48x48/mimetypes", hicolor);
        std::fs::create_dir_all(&dst_dir)?;
        std::fs::copy(smallest, format!("{}/{}.png", dst_dir, target_name))?;
        count += 1;
    }
    Ok(count)
}

/// Install the mascot illustration shown in the GUI's About dialog.
/// Single bundled source PNG, non-square (1536×1024). Drops copies
/// into a handful of standard hicolor app-icon buckets so GTK's
/// `IconTheme::lookup_icon` (which walks 16/22/24/32/48/64/96/128/
/// 192/256/512 — *not* 1024) actually finds the file. GTK loads PNGs
/// by file path and scales at render time so the bucket-name vs.
/// actual-pixel-size mismatch is cosmetic — and covered by the
/// existing icon-size lintian override.
///
/// We do *not* drop into 16/22/24 because the mascot is a detailed
/// illustration that wouldn't read at panel-thumbnail sizes; about-
/// dialog rendering picks 128 or 256 depending on display scale and
/// scales up to 512 for HiDPI, so those three buckets are sufficient.
///
/// Returns the count of files copied. Missing source PNG yields `Ok(0)`
/// — defensive shape matching the other icon installers.
fn install_mascot_icon(
    icons_dir: &std::path::Path,
    hicolor: &str,
) -> std::io::Result<usize> {
    let src = icons_dir.join("odrive-linux-mascot.png");
    if !src.is_file() {
        return Ok(0);
    }
    let mut count = 0usize;
    for size in ["128x128", "256x256", "512x512", "1024x1024"] {
        let dst_dir = format!("{}/{}/apps", hicolor, size);
        std::fs::create_dir_all(&dst_dir)?;
        std::fs::copy(&src, format!("{}/{}.png", dst_dir, MASCOT_ICON_NAME))?;
        count += 1;
    }
    Ok(count)
}

/// Parse a size from a dash-separated filename stem like `cloud-file-512x512` → 512.
/// The asset bundle uses this shape for `mime_icons/cloud-file/` and
/// `mime_icons/cloud-folder/` (alongside byte-identical `@2x` duplicates the
/// caller filters out). Returns `None` if the trailing chunk doesn't parse
/// as `<N>x<N>`.
fn parse_dashed_icon_size(file_stem: &str) -> Option<u32> {
    let last = file_stem.rsplit('-').next()?;
    let n = last.split('x').next()?;
    n.parse().ok()
}

/// Install the animated tray-icon frame set for one colour. The bundle
/// is asymmetric: only `pink`, `white`, and `black` ship animation
/// frames under `odrive-icons/tray-icons/animated/<color>/`, and the
/// filename pattern differs per colour (pink: `infinity-N.png`;
/// black/white: `infinity-<color>-backup-N.png`). We don't care about
/// the prefix — the trailing `-N.png` is the only part we extract — so
/// any well-numbered frame set works.
///
/// Each frame is installed as `odrive-tray-<color>-active-<N>` under
/// hicolor's `status` category, in two places:
/// - `256x256/status/` so high-DPI hosts get the hi-res master.
/// - `48x48/status/` so SNI hosts on GNOME (which only walk
///   panel-size buckets when resolving icon names — see
///   `install_tray_icon`'s docstring) actually find the icon.
///
/// Source frames are 1024×1024; the dimensions don't have to match the
/// bucket name (`gtk-update-icon-cache` and St.Icon load by file path
/// and scale at render time) so a single-size source set is fine.
///
/// Returns the count of files copied (2 per frame on success). A
/// missing `animated/<color>/` directory yields `Ok(0)` — the colour
/// simply won't animate at runtime.
fn install_tray_animation(
    icons_dir: &std::path::Path,
    hicolor: &str,
    color: &str,
) -> std::io::Result<usize> {
    let frames_dir = icons_dir
        .join("tray-icons")
        .join("animated")
        .join(color);
    if !frames_dir.is_dir() {
        return Ok(0);
    }

    // Collect (frame_n, path) pairs by extracting the trailing integer
    // from each PNG's stem. Sort by frame_n so install order is
    // deterministic (helps when debugging via `ls`); the runtime
    // animation indexes by name, not by install order.
    let mut frames: Vec<(u32, std::path::PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&frames_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("png") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let Some(n_str) = stem.rsplit('-').next() else { continue };
        let Ok(n) = n_str.parse::<u32>() else { continue };
        frames.push((n, path));
    }
    frames.sort_by_key(|(n, _)| *n);

    let mut count = 0usize;
    for (n, path) in &frames {
        let target_name = format!("odrive-tray-{}-active-{}", color, n);
        for size in ["256x256", "48x48"] {
            let dst_dir = format!("{}/{}/status", hicolor, size);
            std::fs::create_dir_all(&dst_dir)?;
            std::fs::copy(path, format!("{}/{}.png", dst_dir, target_name))?;
            count += 1;
        }
    }
    Ok(count)
}

/// Return the smallest source PNG suitable as a panel-size shim. Prefer
/// the 256-px file inside the per-colour subdirectory; otherwise the
/// root master. Returns `None` when neither exists (caller's
/// `install_tray_icon` already returned 0 in that case).
fn panel_source_for(subdir: &std::path::Path, master: &std::path::Path) -> Option<std::path::PathBuf> {
    if subdir.is_dir() {
        let mut smallest: Option<(u32, std::path::PathBuf)> = None;
        if let Ok(entries) = std::fs::read_dir(subdir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) != Some("png") {
                    continue;
                }
                let stem = match p.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s,
                    None => continue,
                };
                let Some(size) = parse_icon_size(stem) else { continue };
                match &smallest {
                    None => smallest = Some((size, p)),
                    Some((s, _)) if size < *s => smallest = Some((size, p)),
                    _ => {}
                }
            }
        }
        if let Some((_, p)) = smallest {
            return Some(p);
        }
    }
    if master.is_file() {
        return Some(master.to_path_buf());
    }
    None
}

fn build_mime_xml() -> String {
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <mime-info xmlns=\"http://www.freedesktop.org/standards/shared-mime-info\">\n  \
         <mime-type type=\"application/vnd.odrive.placeholder-file\">\n    \
         <comment>odrive remote-only file</comment>\n    \
         <icon name=\"{file_icon}\"/>\n    \
         <glob pattern=\"*.cloud\"/>\n  \
         </mime-type>\n  \
         <mime-type type=\"application/vnd.odrive.placeholder-folder\">\n    \
         <comment>odrive remote-only folder</comment>\n    \
         <icon name=\"{folder_icon}\"/>\n    \
         <glob pattern=\"*.cloudf\"/>\n  \
         </mime-type>\n",
        file_icon = PLACEHOLDER_FILE_ICON,
        folder_icon = PLACEHOLDER_FOLDER_ICON,
    );
    for (_subdir, stem, globs) in CLOUD_TYPES {
        out.push_str(&format!(
            "  <mime-type type=\"application/vnd.odrive.{stem}-cloud\">\n    \
             <comment>odrive remote-only {stem} placeholder</comment>\n    \
             <sub-class-of type=\"application/vnd.odrive.placeholder-file\"/>\n    \
             <icon name=\"odrive-{stem}-cloud\"/>\n",
            stem = stem,
        ));
        for g in *globs {
            out.push_str(&format!("    <glob pattern=\"{}\"/>\n", g));
        }
        out.push_str("  </mime-type>\n");
    }
    out.push_str("</mime-info>\n");
    out
}

fn install_handlers() -> Result<(), Box<dyn std::error::Error>> {
    let xdg_data = xdg_data_home();

    let exe = std::env::current_exe()?;
    let exe_str = exe.to_string_lossy().to_string();

    let mime_pkg_dir = format!("{}/mime/packages", xdg_data);
    let app_dir = format!("{}/applications", xdg_data);
    let hicolor = format!("{}/icons/hicolor", xdg_data);
    std::fs::create_dir_all(&mime_pkg_dir)?;
    std::fs::create_dir_all(&app_dir)?;

    let mime_path = format!("{}/{}", mime_pkg_dir, MIME_XML_NAME);
    let desktop_path = format!("{}/{}", app_dir, DESKTOP_NAME);
    let launcher_desktop_path = format!("{}/{}", app_dir, APP_DESKTOP_NAME);

    std::fs::write(&mime_path, build_mime_xml())?;

    let desktop = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=odrive Cloud Sync\n\
         Comment=Materialize and open odrive placeholders\n\
         Exec={} open %f\n\
         NoDisplay=true\n\
         MimeType={};{};\n\
         Icon=folder-remote\n",
        exe_str, MIME_FILE, MIME_FOLDER,
    );
    std::fs::write(&desktop_path, desktop)?;

    // Launcher .desktop for the GUI (taskbar, app grid, alt-tab). The
    // Exec line is best-effort: we look for `odrive-gui` next to the
    // running CLI, fall back to bare `odrive-gui` (assumes $PATH).
    // StartupWMClass matches the GUI's application_id so window
    // managers associate the running window with this entry.
    let gui_exec = exe
        .parent()
        .map(|d| d.join("odrive-gui"))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "odrive-gui".to_string());
    let launcher_desktop = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=odrive Manager\n\
         Comment=Manage odrive cloud sync, mounts, and folder rules\n\
         Exec={}\n\
         Icon={}\n\
         StartupWMClass={}\n\
         Terminal=false\n\
         Categories=Network;Utility;FileTransfer;\n",
        gui_exec, APP_LAUNCHER_ICON, APP_LAUNCHER_ICON,
    );
    std::fs::write(&launcher_desktop_path, launcher_desktop)?;

    // Sweep any leftover legacy Plasma service-menu .desktop from a
    // prior install — superseded by the C++ KFileItemActionPlugin in
    // dolphin-plugin/. Best-effort: missing file is fine.
    let legacy_servicemenu = format!(
        "{}/kio/servicemenus/{}",
        xdg_data, LEGACY_SERVICEMENU_NAME
    );
    if std::path::Path::new(&legacy_servicemenu).exists() {
        if let Err(e) = std::fs::remove_file(&legacy_servicemenu) {
            eprintln!("Warning: could not remove legacy service menu {}: {}",
                legacy_servicemenu, e);
        } else {
            println!("Removed legacy service menu {}", legacy_servicemenu);
        }
    }

    // Icons are optional: if the workspace odrive-icons/ dir isn't sitting
    // next to the binary (e.g. installed via `cargo install` without
    // copying assets), we still register MIME types and the .desktop
    // handler — Nautilus just falls back to generic file icons.
    let mut icon_files = 0usize;
    if let Some(icons_dir) = find_icons_dir() {
        for (subdir, name) in EMBLEMS {
            icon_files += install_icon_set(
                &icons_dir.join("emblems").join(subdir),
                &hicolor,
                "emblems",
                name,
            )?;
            // Plasma's KOverlayIconPlugin renders emblems at small
            // sizes (16/22/48 px depending on view zoom) and looks them
            // up by walking *upward* through hicolor size buckets — but
            // it doesn't always fall back from 256x256 down. The
            // emblem source bundle only ships 256/512/1024, so without
            // a small-size shim Dolphin sometimes silently fails to
            // resolve the icon and the overlay never paints. Mirror
            // the same fix `install_tray_icon` uses for SNI panels:
            // copy the smallest available source into the standard
            // small buckets. Nautilus is more forgiving and didn't
            // need this; Dolphin is stricter.
            icon_files += install_small_size_shims(
                &icons_dir.join("emblems").join(subdir),
                &hicolor,
                "emblems",
                name,
            )?;
        }
        for (subdir, stem, _globs) in CLOUD_TYPES {
            let target = format!("odrive-{}-cloud", stem);
            icon_files += install_icon_set(
                &icons_dir.join("cloud-file-types").join(subdir),
                &hicolor,
                "mimetypes",
                &target,
            )?;
        }
        for color in TRAY_COLORS {
            icon_files += install_tray_icon(&icons_dir, &hicolor, color)?;
            icon_files += install_tray_animation(&icons_dir, &hicolor, color)?;
        }
        icon_files += install_mount_folder_icon(&icons_dir, &hicolor)?;
        icon_files += install_placeholder_icon(
            &icons_dir,
            &hicolor,
            "cloud-file",
            PLACEHOLDER_FILE_ICON,
        )?;
        icon_files += install_placeholder_icon(
            &icons_dir,
            &hicolor,
            "cloud-folder",
            PLACEHOLDER_FOLDER_ICON,
        )?;
        icon_files += install_icon_set(
            &icons_dir.join("app-icon"),
            &hicolor,
            "apps",
            APP_MENU_ICON,
        )?;
        icon_files += install_icon_set(
            &icons_dir.join("app-icon"),
            &hicolor,
            "apps",
            APP_LAUNCHER_ICON,
        )?;
        icon_files += install_mascot_icon(&icons_dir, &hicolor)?;
        let _ = std::process::Command::new("gtk-update-icon-cache")
            .args(["-f", "-t"])
            .arg(&hicolor)
            .status();
    } else {
        eprintln!("Note: odrive-icons/ not found alongside the binary — emblems and cloud-file-type icons skipped.");
    }

    // Apply the main-folder icon to every existing mount. New mounts
    // get this automatically via OdriveAgent::mount; this back-fills
    // any mounts the user already had before install-handlers ran.
    // Best-effort: a non-GVFS environment or a missing `gio` binary
    // just leaves the default folder icon in place.
    let agent = OdriveAgent::new();
    let mut tagged = 0usize;
    if let Ok(mounts) = agent.get_mounts() {
        for m in mounts {
            if odrive_core::set_folder_custom_icon(
                &m.local_path,
                odrive_core::MOUNT_FOLDER_ICON_NAME,
            )
            .is_ok()
            {
                tagged += 1;
            }
        }
    }

    let _ = std::process::Command::new("update-mime-database")
        .arg(format!("{}/mime", xdg_data))
        .status();
    let _ = std::process::Command::new("update-desktop-database")
        .arg(&app_dir)
        .status();

    for mime in &[MIME_FILE, MIME_FOLDER] {
        let _ = std::process::Command::new("xdg-mime")
            .args(["default", DESKTOP_NAME, mime])
            .status();
    }

    println!("Handlers installed:");
    println!("  {}", mime_path);
    println!("  {}", desktop_path);
    println!("  {}", launcher_desktop_path);
    if icon_files > 0 {
        println!("  {} icon files under {}", icon_files, hicolor);
    }
    if tagged > 0 {
        println!("Tagged {} existing mount(s) with the main-folder icon.", tagged);
    }
    println!("Default app for placeholder MIMEs set to {}.", DESKTOP_NAME);
    println!("Restart Nautilus (`nautilus -q`) to pick up the new MIME types and icons.");
    Ok(())
}

fn uninstall_handlers() -> Result<(), Box<dyn std::error::Error>> {
    let xdg_data = xdg_data_home();
    let mime_path = format!("{}/mime/packages/{}", xdg_data, MIME_XML_NAME);
    let desktop_path = format!("{}/applications/{}", xdg_data, DESKTOP_NAME);
    let launcher_desktop_path = format!("{}/applications/{}", xdg_data, APP_DESKTOP_NAME);
    let legacy_servicemenu = format!("{}/kio/servicemenus/{}", xdg_data, LEGACY_SERVICEMENU_NAME);
    let hicolor = format!("{}/icons/hicolor", xdg_data);

    let mut removed_any = false;
    for path in [&mime_path, &desktop_path, &launcher_desktop_path, &legacy_servicemenu] {
        match std::fs::remove_file(path) {
            Ok(()) => {
                println!("Removed {}", path);
                removed_any = true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("Failed to remove {}: {}", path, e),
        }
    }

    // Sweep our installed icons. We only delete files we wrote; other
    // emblems/mimetypes/status icons in hicolor are untouched.
    let emblem_targets: Vec<String> = EMBLEMS.iter().map(|(_, n)| (*n).to_string()).collect();
    let mut mime_targets: Vec<String> = CLOUD_TYPES
        .iter()
        .map(|(_, stem, _)| format!("odrive-{}-cloud", stem))
        .collect();
    mime_targets.push(PLACEHOLDER_FILE_ICON.to_string());
    mime_targets.push(PLACEHOLDER_FOLDER_ICON.to_string());
    let mut status_targets: Vec<String> = TRAY_COLORS
        .iter()
        .map(|c| format!("odrive-tray-{}", c))
        .collect();
    // Sweep animation frames too. Colours without animation just won't
    // have files at these names; `remove_file` returns NotFound and the
    // sweep loop silently skips. We don't need a separate "animated
    // colours" list at uninstall time.
    for c in TRAY_COLORS {
        for n in 1..=TRAY_ANIMATION_FRAMES {
            status_targets.push(format!("odrive-tray-{}-active-{}", c, n));
        }
    }
    let places_targets: Vec<String> = vec![odrive_core::MOUNT_FOLDER_ICON_NAME.to_string()];
    let apps_targets: Vec<String> = vec![
        APP_MENU_ICON.to_string(),
        APP_LAUNCHER_ICON.to_string(),
        MASCOT_ICON_NAME.to_string(),
    ];
    let mut removed_icons = 0usize;
    if let Ok(entries) = std::fs::read_dir(&hicolor) {
        for size_dir in entries.flatten() {
            for (category, names) in [
                ("emblems", &emblem_targets),
                ("mimetypes", &mime_targets),
                ("status", &status_targets),
                ("places", &places_targets),
                ("apps", &apps_targets),
            ] {
                let cat_dir = size_dir.path().join(category);
                if !cat_dir.is_dir() {
                    continue;
                }
                for name in names {
                    let p = cat_dir.join(format!("{}.png", name));
                    if p.exists() {
                        let _ = std::fs::remove_file(&p);
                        removed_icons += 1;
                    }
                }
            }
        }
    }
    if removed_icons > 0 {
        println!("Removed {} icon files from {}", removed_icons, hicolor);
        let _ = std::process::Command::new("gtk-update-icon-cache")
            .args(["-f", "-t"])
            .arg(&hicolor)
            .status();
        removed_any = true;
    }

    // Strip the GVFS custom-icon metadata from any mounts we tagged
    // during install. Best-effort: a non-GVFS environment or a
    // missing `gio` simply leaves the metadata in place — harmless,
    // since the icon name no longer resolves and Nautilus falls back
    // to the default folder icon.
    let agent = OdriveAgent::new();
    if let Ok(mounts) = agent.get_mounts() {
        for m in mounts {
            let _ = odrive_core::unset_folder_custom_icon(&m.local_path);
        }
    }

    if removed_any {
        let _ = std::process::Command::new("update-mime-database")
            .arg(format!("{}/mime", xdg_data))
            .status();
        let _ = std::process::Command::new("update-desktop-database")
            .arg(format!("{}/applications", xdg_data))
            .status();
        println!("Caches refreshed. Default-app entries in mimeapps.list may need manual cleanup.");
    } else {
        println!("Nothing to remove (handlers were not installed).");
    }
    Ok(())
}

/// Walk up from the running binary looking for the workspace's
/// `nautilus_extension.py`. Same shape as `find_icons_dir`. Returns
/// `None` if the file isn't found, in which case `prepare_payload`
/// skips the Nautilus extension — packagers running the command from
/// a non-workspace checkout can copy the file in by other means.
fn find_nautilus_extension() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut cur = exe.as_path();
    while let Some(parent) = cur.parent() {
        let candidate = parent.join("nautilus_extension.py");
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = parent;
    }
    None
}

/// Walk up from the running binary looking for `docs/man/`. Same
/// workspace-relative shape as the icon and Nautilus-extension
/// finders. Returns `None` if the directory isn't found —
/// `prepare_payload` then skips manpage staging and the .deb / .rpm
/// builds get a `no-manual-page` lintian / rpmlint warning instead
/// of failing.
fn find_man_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut cur = exe.as_path();
    while let Some(parent) = cur.parent() {
        let candidate = parent.join("docs").join("man");
        if candidate.is_dir() {
            return Some(candidate);
        }
        cur = parent;
    }
    None
}

/// Materialise the static package payload (icons under hicolor/, MIME
/// XML, .desktop files with system-stable Exec paths, the Nautilus
/// Python extension) under `<dst><prefix>/share/...`. Reuses the same
/// icon-installer helpers `install_handlers` calls; only the destination
/// root and the .desktop Exec lines differ.
///
/// What this *doesn't* do (intentionally — these are per-user / runtime
/// concerns the package's `postinst` / `%post` hook must trigger from
/// the install host, or the user must do once via `odrive-cli setup`):
///   - cache refresh (`gtk-update-icon-cache`, `update-mime-database`,
///     `update-desktop-database`) — the package's post-install hook
///     runs these against system dirs
///   - `xdg-mime` defaults — per-user, lives in `~/.config/mimeapps.list`
///   - `set_folder_custom_icon` on existing mounts — touches user files
///     under `~/odrive`
///   - placeholder padding — same reason
///   - systemd unit — `OdriveAgent::write_systemd_unit` plants this
///     per-user in `~/.config/systemd/user/` after the wizard's Install
///     page lands the agent binary at `~/.odrive-agent/bin/odriveagent`;
///     the path is per-user variable so a system-wide unit doesn't fit.
fn prepare_payload(dst: &str, prefix: &str) -> Result<(), Box<dyn std::error::Error>> {
    let icons_dir = find_icons_dir()
        .ok_or("odrive-icons/ not found alongside the binary; run prepare-payload from a workspace checkout")?;

    let share_root = format!("{}{}/share", dst, prefix);
    let hicolor = format!("{}/icons/hicolor", share_root);
    let mime_dir = format!("{}/mime/packages", share_root);
    let app_dir = format!("{}/applications", share_root);
    let nautilus_dir = format!("{}/nautilus-python/extensions", share_root);
    let man_dir = format!("{}/man/man1", share_root);

    std::fs::create_dir_all(&mime_dir)?;
    std::fs::create_dir_all(&app_dir)?;
    std::fs::create_dir_all(&nautilus_dir)?;
    std::fs::create_dir_all(&man_dir)?;

    let mut icon_files = 0usize;
    for (subdir, name) in EMBLEMS {
        icon_files += install_icon_set(
            &icons_dir.join("emblems").join(subdir),
            &hicolor,
            "emblems",
            name,
        )?;
        icon_files += install_small_size_shims(
            &icons_dir.join("emblems").join(subdir),
            &hicolor,
            "emblems",
            name,
        )?;
    }
    for (subdir, stem, _globs) in CLOUD_TYPES {
        let target = format!("odrive-{}-cloud", stem);
        icon_files += install_icon_set(
            &icons_dir.join("cloud-file-types").join(subdir),
            &hicolor,
            "mimetypes",
            &target,
        )?;
    }
    for color in TRAY_COLORS {
        icon_files += install_tray_icon(&icons_dir, &hicolor, color)?;
        icon_files += install_tray_animation(&icons_dir, &hicolor, color)?;
    }
    icon_files += install_mount_folder_icon(&icons_dir, &hicolor)?;
    icon_files += install_placeholder_icon(&icons_dir, &hicolor, "cloud-file", PLACEHOLDER_FILE_ICON)?;
    icon_files += install_placeholder_icon(&icons_dir, &hicolor, "cloud-folder", PLACEHOLDER_FOLDER_ICON)?;
    icon_files += install_icon_set(&icons_dir.join("app-icon"), &hicolor, "apps", APP_MENU_ICON)?;
    icon_files += install_icon_set(&icons_dir.join("app-icon"), &hicolor, "apps", APP_LAUNCHER_ICON)?;
    icon_files += install_mascot_icon(&icons_dir, &hicolor)?;

    let mime_path = format!("{}/{}", mime_dir, MIME_XML_NAME);
    std::fs::write(&mime_path, build_mime_xml())?;

    let cli_path = format!("{}/bin/odrive-cli", prefix);
    let desktop = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=odrive Cloud Sync\n\
         Comment=Materialize and open odrive placeholders\n\
         Exec={} open %f\n\
         NoDisplay=true\n\
         MimeType={};{};\n\
         Icon=folder-remote\n",
        cli_path, MIME_FILE, MIME_FOLDER,
    );
    let desktop_path = format!("{}/{}", app_dir, DESKTOP_NAME);
    std::fs::write(&desktop_path, desktop)?;

    let gui_path = format!("{}/bin/odrive-gui", prefix);
    let launcher = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=odrive Manager\n\
         Comment=Manage odrive cloud sync, mounts, and folder rules\n\
         Exec={}\n\
         Icon={}\n\
         StartupWMClass={}\n\
         Terminal=false\n\
         Categories=Network;Utility;FileTransfer;\n",
        gui_path, APP_LAUNCHER_ICON, APP_LAUNCHER_ICON,
    );
    let launcher_path = format!("{}/{}", app_dir, APP_DESKTOP_NAME);
    std::fs::write(&launcher_path, launcher)?;

    let nautilus_path = if let Some(src) = find_nautilus_extension() {
        let target = format!("{}/odrive-linux.py", nautilus_dir);
        std::fs::copy(&src, &target)?;
        Some(target)
    } else {
        None
    };

    // Manpages: section-1 sources live under docs/man/. debhelper +
    // rpmbuild auto-gzip on package time, so we drop them in plain.
    let mut man_count = 0usize;
    if let Some(src_dir) = find_man_dir() {
        for name in ["odrive-cli.1", "odrive-gui.1"] {
            let src = src_dir.join(name);
            if src.is_file() {
                std::fs::copy(&src, format!("{}/{}", man_dir, name))?;
                man_count += 1;
            }
        }
    }

    println!("Payload prepared under {}", dst);
    println!("  icons:    {} files under {}", icon_files, hicolor);
    println!("  mime:     {}", mime_path);
    println!("  handler:  {}", desktop_path);
    println!("  launcher: {}", launcher_path);
    if let Some(p) = nautilus_path {
        println!("  nautilus: {}", p);
    } else {
        eprintln!("Note: nautilus_extension.py not found — Nautilus integration omitted from payload.");
    }
    if man_count > 0 {
        println!("  man:      {} pages under {}", man_count, man_dir);
    } else {
        eprintln!("Note: docs/man/ not found — manpages omitted from payload.");
    }
    Ok(())
}
