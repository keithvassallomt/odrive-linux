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

fn xdg_data_home() -> String {
    std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").expect("$HOME must be set");
        format!("{}/.local/share", home)
    })
}

fn install_handlers() -> Result<(), Box<dyn std::error::Error>> {
    let xdg_data = xdg_data_home();

    let exe = std::env::current_exe()?;
    let exe_str = exe.to_string_lossy().to_string();

    let mime_pkg_dir = format!("{}/mime/packages", xdg_data);
    let app_dir = format!("{}/applications", xdg_data);
    std::fs::create_dir_all(&mime_pkg_dir)?;
    std::fs::create_dir_all(&app_dir)?;

    let mime_path = format!("{}/{}", mime_pkg_dir, MIME_XML_NAME);
    let desktop_path = format!("{}/{}", app_dir, DESKTOP_NAME);

    let mime_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="application/vnd.odrive.placeholder-file">
    <comment>odrive remote-only file</comment>
    <glob pattern="*.cloud"/>
  </mime-type>
  <mime-type type="application/vnd.odrive.placeholder-folder">
    <comment>odrive remote-only folder</comment>
    <glob pattern="*.cloudf"/>
  </mime-type>
</mime-info>
"#;
    std::fs::write(&mime_path, mime_xml)?;

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
    println!("Default app for {} and {} set to {}.", MIME_FILE, MIME_FOLDER, DESKTOP_NAME);
    println!("Restart Nautilus (`nautilus -q`) to pick up the new MIME types.");
    Ok(())
}

fn uninstall_handlers() -> Result<(), Box<dyn std::error::Error>> {
    let xdg_data = xdg_data_home();
    let mime_path = format!("{}/mime/packages/{}", xdg_data, MIME_XML_NAME);
    let desktop_path = format!("{}/applications/{}", xdg_data, DESKTOP_NAME);

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
