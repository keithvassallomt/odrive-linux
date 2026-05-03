import os
import shutil
import subprocess
import sys
from gi.repository import Nautilus, GObject


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


class OdriveExtension(GObject.GObject, Nautilus.MenuProvider):
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
        items = []

        placeholders = []
        regular_files = []

        for file in files:
            path = file.get_location().get_path()
            name = file.get_name()
            
            is_placeholder = name.endswith('.cloud') or name.endswith('.cloudf')
            in_mount = any(path.startswith(m) for m in mounts)

            if is_placeholder:
                placeholders.append(path)
            elif in_mount:
                regular_files.append(path)

        if placeholders:
            sync_item = Nautilus.MenuItem(
                name='OdriveSyncItem',
                label='Sync with odrive',
                tip='Download from cloud'
            )
            sync_item.connect('activate', self.on_sync_clicked, placeholders)
            items.append(sync_item)

        if regular_files:
            unsync_item = Nautilus.MenuItem(
                name='OdriveUnsyncItem',
                label='Unsync (odrive)',
                tip='Revert to placeholder'
            )
            unsync_item.connect('activate', self.on_unsync_clicked, regular_files)
            items.append(unsync_item)

        return items

    def on_sync_clicked(self, menu, paths):
        for path in paths:
            subprocess.run([self.cli_path, 'sync', path], check=False)

    def on_unsync_clicked(self, menu, paths):
        for path in paths:
            subprocess.run([self.cli_path, 'unsync', path], check=False)
