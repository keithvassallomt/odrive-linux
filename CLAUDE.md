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
cargo run -p odrive-gui              # launch the GTK app (wizard or dashboard depending on state)
cargo test --workspace               # 18 unit tests across odrive-core (config, db, parsers, threshold round-trip)
```

The GUI requires GTK4 and Libadwaita 1.5+ system libraries (`libgtk-4-dev`, `libadwaita-1-dev` on Debian/Ubuntu).

## Runtime prerequisites the code assumes

These are not installed by this repo — the user must have them set up before any of this code is useful:

1. **odrive agent binaries** — `odrive` and `odriveagent` in a directory `OdriveAgent::new()` reads from `~/.config/odrive-linux/config.toml` (key: `agent_bin_dir`, default `~/.odrive-agent/bin`). The GUI's onboarding wizard installs them on first launch via the official `dl.odrive.com` pipeline if missing, or accepts a custom path.
2. **Authentication** — `odrive authenticate <key>`, surfaced through the wizard's Login page or invokable directly.
3. **At least one mount** — `odrive mount <local> <remote>`. The wizard offers an optional Mount page (`~/odrive` ↔ `/`) at the end; users can skip and mount later.
4. **State DB** at `~/.odrive-linux.db` — created automatically on first `scan` or GUI launch.
5. **`python3-nautilus`** (only for the right-click integration) — Debian/Ubuntu `python3-nautilus`, Fedora `nautilus-python`, Arch `python-nautilus`. Without it the extension file is never loaded by Nautilus, even when symlinked into `~/.local/share/nautilus-python/extensions/`. Not auto-installed by anything in this repo.

The first three are walked through automatically by the GUI's onboarding wizard on first launch. See "Onboarding wizard" below.

## Architecture

```
odrive-cli  ──┐                 ┌─► <agent_bin_dir>/odrive   (configured via)
              ├──► odrive-core ─┼─► ~/.odrive-linux.db        (SQLite — placeholder rows)
odrive-gui  ──┘                 └─► ~/.config/odrive-linux/config.toml  (XDG — agent_bin_dir)

nautilus_extension.py  ──► odrive-cli (via _find_cli) ──► (same path through odrive-core)
```

**`odrive-core`** is the only crate that knows how to talk to the agent. Three types matter:

- `OdriveAgent` (lib.rs) — wraps every CLI subcommand the manager uses (`status`, `status --mounts`, `sync`, `unsync`, `refresh`, `mount`, `authenticate`) plus daemon lifecycle (`start`/`stop` try `systemctl --user start odrive.service` first, fall back to `nohup odriveagent`). Onboarding helpers: `is_authenticated()`, `install_official()` (runs the dl.odrive.com curl+tar pipeline), `write_systemd_unit()` (templates the unit's `ExecStart` with the current `agent_path`), `enable_systemd_unit()`, `enable_linger()`. Path-resolution helpers: `default_mount_path()` (`~/odrive`), `with_new_bin_dir()` (rebuild after the wizard's "specify custom location" branch). Also contains `scan_placeholders`, which walks a mount tree and upserts every `.cloud`/`.cloudf` it finds into the DB.
- `OdriveDb` (db.rs) — thin rusqlite wrapper around a single `placeholders` table (id, local_path, remote_path, is_folder, sync_status). The DB is the bridge between "what the CLI tells us" and "what the GUI renders" — the GUI never invokes the agent for placeholder counts, it reads from SQLite.
- `OdriveConfig` (config.rs) — `~/.config/odrive-linux/config.toml`. Currently a single key, `agent_bin_dir`, set by the wizard's "specify custom location" branch. Load is fault-tolerant: missing file or unparseable TOML both fall back to defaults rather than erroring (a fresh-system run is the common case, not a bug).

**`odrive-cli`** is a clap-derive front-end that 1:1 maps subcommands onto `OdriveAgent` methods. `Status` and the no-subcommand default both print agent status plus the DB-tracked placeholder count.

**`odrive-gui`** is a Libadwaita app split into two surfaces, each its own `ApplicationWindow`:

- **Onboarding wizard** (`wizard.rs`) — `Adw.NavigationView` with up to four pages (Install / Service / Login / optional Mount). At `connect_activate`, `main.rs::needs_wizard()` checks all four preconditions; if any fails the wizard window is presented, otherwise the dashboard goes straight up. Pages advance dynamically: each successful action re-runs `push_next` which checks every precondition fresh and pushes the next failing one (or closes the wizard). The wizard's agent is held in `Rc<RefCell<OdriveAgent>>` because the Install page can swap the active bin directory mid-flow. Long-running ops (install download, mount) run synchronously on the GTK thread for now — same trade-off as the dashboard.
- **Dashboard** (`main.rs::present_dashboard`) — wrapped in an `Adw.NavigationView` so subpages (currently the Settings page) can be pushed onto the same window. Built imperatively; state updates flow through an `update_ui` closure cloned into every button handler and into a 5s `glib::timeout_add_seconds_local` background poll. Each mount row gets a trailing "Unmount" button that pops an `Adw.MessageDialog` confirmation before calling `agent.unmount(local)`. The header has a gear button that pushes the Settings page. The poll runs the same synchronous shell-outs as a click, so a slow `odrive` response will briefly stutter the UI; if that ever becomes visible the next step is to move IO to a worker thread and post results back via `glib::idle_add_local`.
- **Settings page** (`settings_page.rs`) — three `Adw.ComboRow` widgets bound to the `PlaceholderThreshold` / `XlThreshold` / `AutoUnsyncThreshold` enums in `odrive-core`. Selection changes apply immediately (no Save button — same idiom as GNOME Settings) by calling the matching `OdriveAgent` setter. On CLI failure (e.g. `autounsyncthreshold` rejected on a non-premium account) the row is reverted to the value the agent reports, gated by a shared re-entrancy `RefCell<bool>` to keep the revert from re-firing the handler. We don't gate any UI on subscription tier — show all options to everyone and let upstream errors surface as toasts.

**`nautilus_extension.py`** plugs into Nautilus's `MenuProvider`. On right-click it inspects selected files: `.cloud`/`.cloudf` get a "Sync with odrive" item, regular files inside a known mount get an "Unsync" item. Both shell out to `odrive-cli`, located via `_find_cli`: `$ODRIVE_CLI` override → `$PATH` lookup → `target/release/odrive-cli` → `target/debug/odrive-cli` (relative to the extension file). If none resolve, the extension loads but stays inert (no menu items) and prints a one-shot stderr hint at init. The mount list is discovered at extension init via `odrive-cli mounts --paths` and cached for the lifetime of the Nautilus process — restart Nautilus (`nautilus -q`) to pick up newly-added mounts. On any discovery failure the extension falls back to `[~/odrive]` so users with the conventional layout aren't broken.

## Non-obvious things to know before editing

- **Mount enumeration goes through `odrive status --mounts`, not `odrive mounts`.** The upstream CLI has no `mounts` subcommand — that name is *only* the `odrive-cli` wrapper subcommand we expose. The agent prints two lines per mount (`<local>  status:<state>` then `<remote>  status:<state>`, with remote rendering blank for the odrive root `/`); `parse_mounts` handles that pairing.
- **`is_running` requires both a live agent process *and* a clean `odrive status` exit.** Process aliveness comes from `pgrep -f <agent_path>` (stable contract regardless of upstream wording); the status exit catches the brief window where the process is up but the IPC isn't yet bound or has wedged. `get_status` shares the same process check so the two never disagree.
- **`scan_placeholders` is fault-tolerant per-entry.** Unreadable directory entries, recursion errors, and DB upsert failures all `log::warn!` and continue rather than aborting the scan. The returned count only includes successfully-recorded placeholders.
- **The GUI's `update_ui` closure must stay `Clone` and `'static`-friendly.** It's cloned into each button handler. Adding non-`Clone` captures will break the build in non-obvious ways.
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
odrive xlthreshold <never|small|medium|large|xlarge>            # split files larger than this into chunks on upload
odrive autounsyncthreshold <never|day|week|month>               # auto-cleanup files not accessed within window (premium)
odrive shutdown                                                 # terminate the agent cleanly
```

**Threshold-token asymmetry to know about.** The CLI accepts `never`/`always` for `placeholderthreshold` and `xlarge` for `xlthreshold`; the *same* values render in `odrive status` text as `neverDownload` / `alwaysDownload` / `extraLarge`. `odrive-core::parse_global_settings` accepts both renderings; the CLI-arg side uses the short form via `<Enum>::as_cli_arg()`.
