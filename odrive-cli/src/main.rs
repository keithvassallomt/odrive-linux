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
                        let db = OdriveDb::open(agent.get_db_path()).unwrap();
                        let count = db.count_placeholders().unwrap_or(0);
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
            let scan_path = path.unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/home/keith".to_string());
                format!("{}/odrive", home)
            });
            println!("Scanning {} for placeholders...", scan_path);
            match agent.scan_placeholders(&scan_path) {
                Ok(count) => println!("Found and tracked {} placeholders.", count),
                Err(e) => eprintln!("Scan failed: {}", e),
            }
        }
    }
}
