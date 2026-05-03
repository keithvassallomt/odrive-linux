import os
import subprocess
from gi.repository import Nautilus, GObject

class OdriveExtension(GObject.GObject, Nautilus.MenuProvider):
    def __init__(self):
        self.cli_path = os.path.expanduser('~/LocalCode/keithvassallomt/odrive-linux/target/debug/odrive-cli')

    def get_mounts(self):
        # In the future, parse from 'odrive-cli mounts'
        return [os.path.expanduser('~/odrive')]

    def get_file_items(self, *args):
        files = args[-1]
        if not files:
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
