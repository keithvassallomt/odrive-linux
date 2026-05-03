# odrive-linux Research & Setup

## Current Setup Environment
- **OS:** Ubuntu (VM)
- **Host:** 192.168.96.13
- **Base Directory:** `/home/keith/LocalCode/keithvassallomt/odrive-linux`

## odrive CLI Agent Configuration
The project uses the official odrive binary agent as the backend engine.

### 1. Authentication
```bash
$HOME/.odrive-agent/bin/odrive authenticate <KEY>
```
*Note: Key is retrieved from the odrive.com user dashboard.*

### 2. Mounting the Drive
The local folder is mapped to the root of the remote odrive storage:
```bash
$HOME/.odrive-agent/bin/odrive mount /home/keith/odrive /
```

### 3. Initial Placeholder Creation (Google Drive Example)
To create a local representation of the remote file system without downloading full file content:
```bash
$HOME/.odrive-agent/bin/odrive sync "/home/keith/odrive/Google Drive.cloudf" --recursive --nodownload
```
- `.cloudf`: Extension used for placeholder folders.
- `.cloud`: Extension used for placeholder files.
- `--recursive`: Traverses all subdirectories.
- `--nodownload`: Ensures only metadata/placeholders are created locally.

## Project Vision: The "Manager" App
- **Language:** Rust (Libadwaita / GTK4)
- **Goal:** Provide a native GNOME experience for managing these placeholders.
- **Key Feature:** Nautilus integration to trigger transparent `odrive sync` on double-click of a `.cloud` file.

## Sync Operations (from official docs)

### 1. Synchronizing a Folder
Expands a `.cloudf` placeholder into its immediate contents.
```bash
odrive sync "/path/to/folder.cloudf"
```
*Options:*
- `--recursive`: Sync the entire tree.
- `--nodownload`: Expand folders but keep files as `.cloud` placeholders.

### 2. Synchronizing a Single File
Downloads the content of a `.cloud` placeholder and replaces it with the actual file.
```bash
odrive sync "/path/to/file.txt.cloud"
```

### 3. Unsyncing (Reverting to Placeholder)
Replaces a local file or folder with its placeholder equivalent to save space.
```bash
odrive sync unsync "/path/to/local/item"
```
*Note: Use `--force` to discard local changes that haven't been uploaded.*

### 4. Refreshing
Forces a check for remote changes in a specific folder.
```bash
odrive refresh "/path/to/folder"
```

### 5. Automation Rules
- **Placeholder Threshold:** `odrive placeholderthreshold [never|small|medium|large|always]` (Auto-download files under a certain size).
- **Auto-Unsync:** `odrive autounsyncthreshold [never|day|week|month]` (Auto-cleanup old files).

## CLI Wrapper Responsibilities

The Rust application acts as a high-level manager/orchestrator for the `odriveagent`.

### 1. Daemon Management
- Ensure `odriveagent` is active (running as a user systemd service or background process).
- Wrap the `authenticate` and `mount` commands for a streamlined onboarding UI.

### 2. Filesystem Monitoring & State
- Scan the sync directory for `.cloud` and `.cloudf` placeholders.
- Maintain a local state (likely SQLite) to bridge the gap between the CLI's raw output and the UI requirements.

### 3. On-Demand Sync (The "Magic" Part)
- Intercept file open events (via Nautilus extension).
- Trigger `odrive sync "/path/to/file.cloud"`.
- Wait for the CLI to replace the placeholder with the real file before passing control back to the OS.

### 4. Space Management
- Implement "Right-click -> Unsync" by calling `odrive unsync "/path/to/file"`.
- Monitor disk usage and potentially provide an "Auto-Unsync" toggle based on the CLI's `autounsyncthreshold`.

## Night Shift Progress (2026-05-03)
- **Daemon Management:** Added robust logic to start/stop `odriveagent` via systemd or fallback.
- **SQLite State:** Implemented placeholder tracking in `~/.odrive-linux.db`.
- **Scanner:** Added a recursive scanner that detects `.cloud` and `.cloudf` files.
- **CLI:** Expanded with `mounts`, `refresh`, and `scan` commands.
- **GUI:** Enhanced dashboard with live status updates, placeholder counts, and a 'Scan Now' button.
- **Verification:** All components pass `cargo check` on the VM.
