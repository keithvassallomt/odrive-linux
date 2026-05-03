# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

odrive-linux is a native Linux frontend for [odrive](https://www.odrive.com)'s on-demand cloud sync. odrive ships an official headless agent (`odriveagent`) and a CLI (`odrive`) that operate on **placeholders** — zero-byte stand-ins for remote files/folders that materialize on demand:

- `*.cloud` — placeholder for a single remote file
- `*.cloudf` — placeholder for a remote folder

This repository builds a higher-level manager around that agent: a Rust CLI, a GTK4/Libadwaita GUI, and a Python Nautilus extension. **None of this code talks to odrive's cloud directly** — every sync/unsync/status call shells out to the user's installed `odrive` binary at `~/.odrive-agent/bin/odrive`.

**Design intent** — the manager wraps the agent so a GNOME user gets:
1. **Daemon orchestration** — make sure `odriveagent` is up (systemd-user preferred, `nohup` fallback) and surface authenticate/mount as part of onboarding.
2. **Local state for the UI** — scan the sync tree for `.cloud`/`.cloudf` and persist them in SQLite so the GUI doesn't have to re-shell-out for every paint.
3. **Transparent on-demand sync** — Nautilus right-click triggers `odrive sync` against a `.cloud` placeholder so the file materializes before the OS hands control back.
4. **Space management** — right-click "Unsync" calls `odrive unsync`, with the agent's `autounsyncthreshold` available as a longer-term auto-cleanup knob.

## Build / run

The repo is a Cargo workspace with three crates plus one standalone Python file.

```bash
cargo build                          # build all three crates
cargo build --release                # release build
cargo check                          # fast type-check across the workspace
cargo run -p odrive-cli -- <subcmd>  # run the CLI (status, mounts, sync, unsync, refresh, scan, start, stop)
cargo run -p odrive-gui              # launch the GTK dashboard
cargo test -p <crate>                # no tests exist yet — adding them is open work
```

The GUI requires GTK4 and Libadwaita 1.5+ system libraries (`libgtk-4-dev`, `libadwaita-1-dev` on Debian/Ubuntu).

## Runtime prerequisites the code assumes

These are not installed by this repo — the user must have them set up before any of this code is useful:

1. **odrive agent binaries** at `~/.odrive-agent/bin/{odrive,odriveagent}` (paths hard-coded in `odrive-core/src/lib.rs::OdriveAgent::new`).
2. **Authentication** done out-of-band via `odrive authenticate <key>`.
3. **At least one mount** created via `odrive mount <local> <remote>`. The conventional local mount is `~/odrive`.
4. **State DB** at `~/.odrive-linux.db` — created automatically on first `scan` or GUI launch.

## Architecture

```
odrive-cli  ──┐
              ├──► odrive-core ──► (Command::new) ──► ~/.odrive-agent/bin/odrive
odrive-gui  ──┘                └─► ~/.odrive-linux.db (SQLite)

nautilus_extension.py  ──► target/debug/odrive-cli  ──► (same path through odrive-core)
```

**`odrive-core`** is the only crate that knows how to talk to the agent. Two types matter:

- `OdriveAgent` (lib.rs) — wraps every CLI subcommand (`status`, `status --mounts`, `sync`, `unsync`, `refresh`) plus daemon lifecycle (`start`/`stop` try `systemctl --user start odrive.service` first, fall back to `nohup odriveagent`). Also contains `scan_placeholders`, which walks a mount tree and upserts every `.cloud`/`.cloudf` it finds into the DB.
- `OdriveDb` (db.rs) — thin rusqlite wrapper around a single `placeholders` table (id, local_path, remote_path, is_folder, sync_status). The DB is the bridge between "what the CLI tells us" and "what the GUI renders" — the GUI never invokes the agent for placeholder counts, it reads from SQLite.

**`odrive-cli`** is a clap-derive front-end that 1:1 maps subcommands onto `OdriveAgent` methods. `Status` and the no-subcommand default both print agent status plus the DB-tracked placeholder count.

**`odrive-gui`** is a single-window Libadwaita app. The whole UI is built imperatively in `main.rs` — there is no separate view layer. State updates happen via a closure (`update_ui`) that's cloned into every button handler and into a 5s `glib::timeout_add_seconds_local` background poll. The poll runs the same synchronous shell-outs as a click, so a slow `odrive` response will briefly stutter the UI; if that ever becomes visible the next step is to move IO to a worker thread and post results back via `glib::idle_add_local`.

**`nautilus_extension.py`** plugs into Nautilus's `MenuProvider`. On right-click it inspects selected files: `.cloud`/`.cloudf` get a "Sync with odrive" item, regular files inside a known mount get an "Unsync" item. Both shell out to the `odrive-cli` debug binary. The extension is **not** wired into a release build path — `self.cli_path` points at `target/debug/odrive-cli`.

## Non-obvious things to know before editing

- **Mount enumeration goes through `odrive status --mounts`, not `odrive mounts`.** The upstream CLI has no `mounts` subcommand — that name is *only* the `odrive-cli` wrapper subcommand we expose. The agent prints two lines per mount (`<local>  status:<state>` then `<remote>  status:<state>`, with remote rendering blank for the odrive root `/`); `parse_mounts` handles that pairing.
- **`is_running` substring-matches `"Unable to connect"`** against the agent's combined stdout/stderr to catch the legacy case where older `odriveagent` builds returned exit 0 even when the daemon was unreachable.
- **`scan_placeholders` is fault-tolerant per-entry.** Unreadable directory entries, recursion errors, and DB upsert failures all `log::warn!` and continue rather than aborting the scan. The returned count only includes successfully-recorded placeholders.
- **The GUI's `update_ui` closure must stay `Clone` and `'static`-friendly.** It's cloned into each button handler. Adding non-`Clone` captures will break the build in non-obvious ways.
- **The Nautilus extension's binary path is a known wart** — it points at `target/debug/odrive-cli` and will silently no-op once the user moves to a release/install layout.
- **`odrive-core` re-exports `OdriveDb` from `lib.rs`.** Use `odrive_core::OdriveDb`, not `odrive_core::db::OdriveDb`.

## Reference: the upstream `odrive` CLI surface this code wraps

```
odrive status                # overview (also prints a Mounts: N count)
odrive status --mounts       # list mounts (two lines each: local then remote)
odrive mount <local> <remote>
odrive unmount <local>
odrive sync <path>           # download a .cloud, or expand a .cloudf
odrive sync <path> --recursive --nodownload   # placeholder-only expansion (typical first run after mount)
odrive unsync <path>         # revert local file to .cloud placeholder
odrive unsync <path> --force # also discard un-uploaded local changes
odrive refresh <path>        # re-check remote for changes
odrive authenticate <key>
odrive placeholderthreshold <never|small|medium|large|always>   # auto-download files under threshold size on expand
odrive autounsyncthreshold <never|day|week|month>               # auto-cleanup files not accessed within window
```
