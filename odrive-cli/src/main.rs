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
    }
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

const MIME_FILE: &str = "application/vnd.odrive.placeholder-file";
const MIME_FOLDER: &str = "application/vnd.odrive.placeholder-folder";
const MIME_XML_NAME: &str = "odrive-linux.xml";
const DESKTOP_NAME: &str = "odrive-linux-open.desktop";

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
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <mime-info xmlns=\"http://www.freedesktop.org/standards/shared-mime-info\">\n  \
         <mime-type type=\"application/vnd.odrive.placeholder-file\">\n    \
         <comment>odrive remote-only file</comment>\n    \
         <glob pattern=\"*.cloud\"/>\n  \
         </mime-type>\n  \
         <mime-type type=\"application/vnd.odrive.placeholder-folder\">\n    \
         <comment>odrive remote-only folder</comment>\n    \
         <glob pattern=\"*.cloudf\"/>\n  \
         </mime-type>\n",
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
        }
        let _ = std::process::Command::new("gtk-update-icon-cache")
            .args(["-f", "-t"])
            .arg(&hicolor)
            .status();
    } else {
        eprintln!("Note: odrive-icons/ not found alongside the binary — emblems and cloud-file-type icons skipped.");
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
    if icon_files > 0 {
        println!("  {} icon files under {}", icon_files, hicolor);
    }
    println!("Default app for placeholder MIMEs set to {}.", DESKTOP_NAME);
    println!("Restart Nautilus (`nautilus -q`) to pick up the new MIME types and icons.");
    Ok(())
}

fn uninstall_handlers() -> Result<(), Box<dyn std::error::Error>> {
    let xdg_data = xdg_data_home();
    let mime_path = format!("{}/mime/packages/{}", xdg_data, MIME_XML_NAME);
    let desktop_path = format!("{}/applications/{}", xdg_data, DESKTOP_NAME);
    let hicolor = format!("{}/icons/hicolor", xdg_data);

    let mut removed_any = false;
    for path in [&mime_path, &desktop_path] {
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
    let mime_targets: Vec<String> = CLOUD_TYPES
        .iter()
        .map(|(_, stem, _)| format!("odrive-{}-cloud", stem))
        .collect();
    let status_targets: Vec<String> = TRAY_COLORS
        .iter()
        .map(|c| format!("odrive-tray-{}", c))
        .collect();
    let mut removed_icons = 0usize;
    if let Ok(entries) = std::fs::read_dir(&hicolor) {
        for size_dir in entries.flatten() {
            for (category, names) in [
                ("emblems", &emblem_targets),
                ("mimetypes", &mime_targets),
                ("status", &status_targets),
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
