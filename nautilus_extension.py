import os
import shutil
import sqlite3
import subprocess
import sys
import time
from gi.repository import Nautilus, GObject


# Cross-process sync state. The GUI inserts a row before kicking off a
# folder-level `odrive sync` and deletes it on completion (see
# `OdriveDb::mark_sync_in_progress` / `clear_sync_in_progress`). Reading
# this on every `update_file_info` would hammer SQLite, so we cache the
# set with a short TTL — the GUI also touches the folder's mtime on
# mark/clear which forces Nautilus to re-call us, so the emblem
# transition feels immediate without us polling aggressively.
_SYNC_DB_PATH = os.path.expanduser('~/.odrive-linux.db')
_DB_CACHE_TTL = 0.5  # seconds
_db_cache = {'sync_in_progress': frozenset(), 'folder_rules': frozenset(), 'expires': 0.0}


def _refresh_db_cache():
    """Reload both small read sets from SQLite. Called when the cache
    has expired; the GUI also touches affected folder mtimes on
    mark/clear/save/delete so Nautilus re-calls update_file_info,
    making the cache TTL the worst-case lag rather than the typical
    one.
    """
    now = time.monotonic()
    if now < _db_cache['expires']:
        return
    sip = frozenset()
    rules = frozenset()
    try:
        # Open read-only via URI form so we never accidentally lock the
        # DB or create a stale file when ~/.odrive-linux.db doesn't yet
        # exist.
        uri = f'file:{_SYNC_DB_PATH}?mode=ro'
        with sqlite3.connect(uri, uri=True, timeout=0.2) as conn:
            cur = conn.execute('SELECT local_path FROM sync_in_progress')
            sip = frozenset(row[0] for row in cur.fetchall())
            cur = conn.execute('SELECT local_path FROM folder_sync_rules')
            rules = frozenset(row[0] for row in cur.fetchall())
    except sqlite3.Error:
        pass  # DB missing / locked / tables not yet migrated → empty.
    _db_cache['sync_in_progress'] = sip
    _db_cache['folder_rules'] = rules
    _db_cache['expires'] = now + _DB_CACHE_TTL


def _sync_in_progress_set():
    _refresh_db_cache()
    return _db_cache['sync_in_progress']


def _folder_rule_set():
    _refresh_db_cache()
    return _db_cache['folder_rules']


# Per-user emblem toggles, persisted by the GUI's Preferences →
# Appearance page into ~/.config/odrive-linux/config.toml. Both default
# True (= prior always-on behaviour) so an extension running ahead of
# a Manager that's never been opened keeps painting emblems. Cached
# briefly to avoid hitting the file on every update_file_info, but
# short enough that a toggle takes effect within a directory tour.
_CONFIG_PATH = os.path.expanduser('~/.config/odrive-linux/config.toml')
_CONFIG_CACHE_TTL = 1.0  # seconds
_config_cache = {'synced_emblem': True, 'syncing_emblem': True, 'expires': 0.0}


def _parse_toml_bool(line):
    """Parse `key = true` / `key = false`. Default True on anything we
    don't recognise — a garbled value should keep the prior on-by-
    default behaviour rather than silently turn the emblem off.
    """
    if '=' not in line:
        return True
    value = line.split('=', 1)[1].strip().lower()
    if value.startswith('false'):
        return False
    return True


def _refresh_config_cache():
    """Reload the two emblem toggles. We don't pull in `tomllib` (3.11+
    only) because the keys we care about are simple `key = bool` lines
    a 5-line parser handles fine — and being permissive on TOML quirks
    is exactly the wrong move here. Missing file → both True (defaults).
    """
    now = time.monotonic()
    if now < _config_cache['expires']:
        return
    synced = True
    syncing = True
    try:
        with open(_CONFIG_PATH, 'r', encoding='utf-8') as f:
            for line in f:
                stripped = line.strip()
                if stripped.startswith('nautilus_synced_emblem'):
                    synced = _parse_toml_bool(stripped)
                elif stripped.startswith('nautilus_syncing_emblem'):
                    syncing = _parse_toml_bool(stripped)
    except (OSError, IOError):
        pass  # Missing / unreadable → defaults stand.
    _config_cache['synced_emblem'] = synced
    _config_cache['syncing_emblem'] = syncing
    _config_cache['expires'] = now + _CONFIG_CACHE_TTL


def _emblems_enabled():
    _refresh_config_cache()
    return _config_cache['synced_emblem'], _config_cache['syncing_emblem']


def _strip_placeholder_suffix(path):
    """Mirror `odrive-cli`'s strip — the GUI marks the conceptual
    folder path (no suffix) but during expand the on-disk entry is
    still `<path>.cloudf`. Strip it before checking the in-progress
    set so the syncing emblem appears on the placeholder too.
    """
    if path.endswith('.cloudf'):
        return path[: -len('.cloudf')]
    if path.endswith('.cloud'):
        return path[: -len('.cloud')]
    return path


def _find_cli():
    """Locate odrive-cli. Priority: $ODRIVE_CLI, $PATH, release build, debug build.

    Returns the absolute path, or None if no usable binary is found — in which
    case the extension stays loaded but renders no menu items rather than
    silently invoking a missing executable on every right-click.
    """
    override = os.environ.get('ODRIVE_CLI')
    if override and os.path.isfile(override) and os.access(override, os.X_OK):
        return override

    on_path = shutil.which('odrive-cli')
    if on_path:
        return on_path

    here = os.path.dirname(os.path.realpath(__file__))
    for relative in ('target/release/odrive-cli', 'target/debug/odrive-cli'):
        candidate = os.path.join(here, relative)
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate

    return None


class OdriveExtension(GObject.GObject, Nautilus.MenuProvider, Nautilus.InfoProvider):
    def __init__(self):
        self.cli_path = _find_cli()
        if self.cli_path is None:
            print(
                'odrive-linux Nautilus extension: odrive-cli not found. '
                'Set $ODRIVE_CLI, install odrive-cli on PATH, or build the '
                'workspace (cargo build [--release]) to enable right-click '
                'sync/unsync.',
                file=sys.stderr,
            )
            self.mounts = []
        else:
            self.mounts = self._discover_mounts()

    def _discover_mounts(self):
        """Query odrive-cli for the local mount paths. Cached at extension
        init — restart Nautilus (`nautilus -q`) to pick up newly-added mounts.
        On any failure, fall back to ['~/odrive'] so users with the default
        layout aren't worse off than before.
        """
        try:
            result = subprocess.run(
                [self.cli_path, 'mounts', '--paths'],
                capture_output=True, text=True, timeout=5,
            )
        except (subprocess.SubprocessError, OSError) as e:
            print(
                f'odrive-linux Nautilus extension: failed to enumerate mounts '
                f'({e}); falling back to ~/odrive.',
                file=sys.stderr,
            )
            return [os.path.expanduser('~/odrive')]
        if result.returncode != 0:
            print(
                f'odrive-linux Nautilus extension: odrive-cli mounts exited '
                f'{result.returncode}; falling back to ~/odrive. stderr: '
                f'{result.stderr.strip()}',
                file=sys.stderr,
            )
            return [os.path.expanduser('~/odrive')]
        paths = [line.strip() for line in result.stdout.splitlines() if line.strip()]
        return paths or [os.path.expanduser('~/odrive')]

    def get_mounts(self):
        return self.mounts

    def get_file_items(self, *args):
        files = args[-1]
        if not files or self.cli_path is None:
            return []

        mounts = self.get_mounts()
        placeholders = []
        regular_files = []
        in_mount_any = []

        for file in files:
            path = file.get_location().get_path()
            name = file.get_name()

            is_placeholder = name.endswith('.cloud') or name.endswith('.cloudf')
            in_mount = any(path.startswith(m) for m in mounts)

            if is_placeholder:
                placeholders.append(path)
                in_mount_any.append(path)
            elif in_mount:
                regular_files.append(path)
                in_mount_any.append(path)

        # Nothing in this selection lives under an odrive mount — no menu.
        if not in_mount_any:
            return []

        submenu = Nautilus.Menu()

        # Sync — only meaningful for placeholders (downloads them).
        if placeholders:
            sync_item = Nautilus.MenuItem(
                name='OdriveMenu::Sync',
                label='Sync',
                tip='Download from cloud'
            )
            sync_item.connect('activate', self.on_sync_clicked, placeholders)
            submenu.append_item(sync_item)

        # Unsync — only meaningful for materialised files (reverts to placeholder).
        if regular_files:
            unsync_item = Nautilus.MenuItem(
                name='OdriveMenu::Unsync',
                label='Unsync',
                tip='Revert to placeholder'
            )
            unsync_item.connect('activate', self.on_unsync_clicked, regular_files)
            submenu.append_item(unsync_item)

        # Refresh — works on anything inside a mount.
        refresh_item = Nautilus.MenuItem(
            name='OdriveMenu::Refresh',
            label='Refresh',
            tip='Re-check remote for changes'
        )
        refresh_item.connect('activate', self.on_refresh_clicked, in_mount_any)
        submenu.append_item(refresh_item)

        # Share Link — generate a public URL and open it in the browser.
        share_item = Nautilus.MenuItem(
            name='OdriveMenu::ShareLink',
            label='Share Link',
            tip='Generate a share link and open it in the browser'
        )
        share_item.connect('activate', self.on_share_link_clicked, in_mount_any)
        submenu.append_item(share_item)

        # Copy Share Link — same URL, written to the clipboard instead.
        copy_item = Nautilus.MenuItem(
            name='OdriveMenu::CopyShareLink',
            label='Copy Share Link',
            tip='Generate a share link and copy it to the clipboard'
        )
        copy_item.connect('activate', self.on_copy_share_link_clicked, in_mount_any)
        submenu.append_item(copy_item)

        # Parent label with the bundled odrive-menu icon (installed under
        # hicolor/<size>/apps/ by `odrive-cli install-handlers`).
        parent = Nautilus.MenuItem(
            name='OdriveMenu',
            label='Odrive',
            tip='odrive actions',
            icon='odrive-menu',
        )
        parent.set_submenu(submenu)
        return [parent]

    def on_sync_clicked(self, menu, paths):
        for path in paths:
            subprocess.run([self.cli_path, 'sync', path], check=False)

    def on_unsync_clicked(self, menu, paths):
        for path in paths:
            subprocess.run([self.cli_path, 'unsync', path], check=False)

    def on_refresh_clicked(self, menu, paths):
        for path in paths:
            subprocess.run([self.cli_path, 'refresh', path], check=False)

    def _generate_share_links(self, paths):
        urls = []
        for path in paths:
            try:
                result = subprocess.run(
                    [self.cli_path, 'sharelink', path],
                    capture_output=True, text=True, check=True,
                )
                url = result.stdout.strip()
                if url:
                    urls.append(url)
            except (subprocess.CalledProcessError, FileNotFoundError):
                pass
        return urls

    def on_share_link_clicked(self, menu, paths):
        for url in self._generate_share_links(paths):
            subprocess.Popen(['xdg-open', url],
                             stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL,
                             start_new_session=True)

    def on_copy_share_link_clicked(self, menu, paths):
        urls = self._generate_share_links(paths)
        if not urls:
            return
        text = '\n'.join(urls)
        # Wayland first (Ubuntu 25.10's default), X11 second. If neither
        # tool is on PATH the link silently isn't copied — there's no
        # meaningful recovery from a Nautilus extension hook.
        if os.environ.get('WAYLAND_DISPLAY'):
            cmd = ['wl-copy']
        else:
            cmd = ['xclip', '-selection', 'clipboard']
        try:
            subprocess.run(cmd, input=text, text=True, check=False)
        except FileNotFoundError:
            pass

    # InfoProvider — applies emblems and pads placeholders on the fly.
    #
    # Decoration model (matches macOS/Windows odrive):
    # - `.cloud`/`.cloudf` placeholders: NO emblem. The cloud-file-type
    #   icon registered for known extensions (gdoc/gsheet/...) already
    #   conveys "remote", and unknown placeholders fall back to the
    #   generic file icon. Adding an emblem here would be redundant.
    # - Materialized items inside any known mount: `odrive-synced`
    #   emblem (a vendor-prefixed icon installed by
    #   `odrive-cli install-handlers` into the hicolor theme).
    # - Mount root itself: no emblem (would clutter ~/odrive's row).
    # - A future syncing emblem on parent folders during in-flight
    #   `odrive sync` is tracked separately; the design reads
    #   in-progress state from a sync_in_progress table in
    #   ~/.odrive-linux.db so both the GUI and Nautilus see the same
    #   set without D-Bus glue.
    #
    # We also opportunistically pad zero-byte placeholders to one byte
    # so GLib's `application/x-zerosize` hardcoding stops blocking
    # MIME-based double-click activation. The upstream odrive agent
    # identifies placeholders by the `.cloud`/`.cloudf` extension, not
    # by zero size, so the null byte is invisible to it. See
    # `odrive-core::pad_placeholder` for the matching behaviour during
    # `odrive-cli scan`.
    def update_file_info(self, file):
        name = file.get_name()
        path = file.get_location().get_path()
        if path is None:
            return Nautilus.OperationResult.COMPLETE

        is_placeholder = name.endswith('.cloud') or name.endswith('.cloudf')

        if is_placeholder:
            self._maybe_pad_placeholder(file)

        synced_on, syncing_on = _emblems_enabled()

        # Syncing emblem wins over the static synced/none state — when
        # a folder is mid-`odrive sync` we want users to see *that*,
        # not the prior synced badge. The GUI marks the conceptual
        # folder path (no .cloudf suffix), so strip before checking.
        # Even with the toggle off we still return early on a match so
        # a synced emblem doesn't sneak through during a sync.
        in_progress = _sync_in_progress_set()
        if in_progress and _strip_placeholder_suffix(path) in in_progress:
            if syncing_on:
                file.add_emblem('odrive-syncing')
            return Nautilus.OperationResult.COMPLETE

        if synced_on:
            if file.is_directory():
                # Directories don't get the synced emblem just because the
                # `.cloudf` was expanded — their contents may still be
                # placeholders, and badging the wrapper as "synced" would
                # mislead. The exception is folders with an explicit
                # sync rule set via the Manager: that rule promises the
                # folder will be kept in sync, so the emblem is honest.
                if path in _folder_rule_set():
                    file.add_emblem('odrive-synced')
            elif not is_placeholder:
                in_mount = any(path.startswith(m) for m in self.mounts)
                is_mount_root = path in self.mounts
                if in_mount and not is_mount_root:
                    file.add_emblem('odrive-synced')
        return Nautilus.OperationResult.COMPLETE

    def _maybe_pad_placeholder(self, file):
        try:
            path = file.get_location().get_path()
            if path is None:
                return
            if os.path.getsize(path) == 0:
                with open(path, 'ab') as f:
                    f.write(b'\0')
                # Tell Nautilus to re-resolve content-type so the new
                # default-app association takes effect on the next click.
                file.invalidate_extension_info()
        except (OSError, IOError):
            pass  # Read-only filesystem, gone, no permission — silently skip.
