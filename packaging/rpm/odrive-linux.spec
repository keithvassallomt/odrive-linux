Name:           odrive-linux
Version:        0.1.0
Release:        1%{?dist}
Summary:        Linux frontend for odrive on-demand cloud sync

License:        AGPL-3.0-or-later
URL:            https://github.com/keithvassallomt/odrive-linux
Source0:        %{name}-%{version}.tar.gz

# Rust toolchain expected on PATH; Fedora's `cargo`/`rust` packages may
# lag the MSRV of our zbus deps, so the release workflow uses rustup.
# Build from source manually with: dnf install rustup; rustup default stable.
BuildRequires:  pkgconfig(gtk4) >= 4.12
BuildRequires:  pkgconfig(libadwaita-1) >= 1.5
BuildRequires:  sqlite-devel
BuildRequires:  cmake >= 3.16
BuildRequires:  extra-cmake-modules
BuildRequires:  qt6-qtbase-devel
BuildRequires:  kf6-kio-devel
BuildRequires:  kf6-kcoreaddons-devel
BuildRequires:  kf6-ki18n-devel
BuildRequires:  desktop-file-utils
BuildRequires:  /usr/bin/update-mime-database
BuildRequires:  /usr/bin/gtk-update-icon-cache

Requires:       gtk4 >= 4.12
Requires:       libadwaita >= 1.5
Requires:       wl-clipboard
Requires:       xdg-utils
Requires:       curl
Requires:       tar
Recommends:     gnome-shell-extension-appindicator
Suggests:       odrive-linux-nautilus
Suggests:       odrive-linux-dolphin

%description
odrive-linux is a native Linux manager around odrive's headless agent
(odriveagent) and CLI (odrive). It ships a GTK4/Libadwaita Manager
with onboarding wizard, mount management, folder sync rules, backup
jobs, trash, encryptor folders, advanced agent settings, log viewer,
and tray indicator. The wizard downloads the official agent on first
launch and writes the systemd-user unit that runs it.

Install odrive-linux-nautilus or odrive-linux-dolphin alongside this
package for first-class right-click integration in your file manager.

GNOME 46+ or KDE Plasma 6.0+ recommended.

%package nautilus
Summary:        Nautilus right-click integration for %{name}
BuildArch:      noarch
Requires:       %{name} = %{version}-%{release}
Recommends:     nautilus-python

%description nautilus
Python extension that adds an "Odrive" submenu to Nautilus's
right-click menu (Sync, Unsync, Refresh, Share Storage, Copy Share
Link, Open Web Preview) and emblem decorations on placeholders and
synced files.

Requires nautilus-python to actually load — install that package or
your Nautilus session won't see the extension.

%package dolphin
Summary:        Dolphin right-click integration for %{name}
Requires:       %{name} = %{version}-%{release}
Recommends:     dolphin

%description dolphin
Qt6/KF6 KFileItemAction and KOverlayIcon plugins giving Dolphin
first-class odrive integration: an "Odrive" right-click submenu that
filters to in-mount selections, plus per-file emblem overlays for
synced and syncing items.

Plasma 6.0 or newer required.

%prep
%autosetup -n %{name}-%{version}

%build
cargo build --release --workspace --locked
cmake -S dolphin-plugin -B dolphin-plugin/build \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX=/usr
cmake --build dolphin-plugin/build --parallel

%install
mkdir -p %{buildroot}%{_bindir}
install -m755 target/release/odrive-cli %{buildroot}%{_bindir}/odrive-cli
install -m755 target/release/odrive-gui %{buildroot}%{_bindir}/odrive-gui

# Static payload: icons, MIME XML, .desktop files, Nautilus extension.
# Same code path install-handlers uses, parameterised by destination root.
./target/release/odrive-cli prepare-payload %{buildroot}

# Dolphin plugins via cmake's install rule.
DESTDIR=%{buildroot} cmake --install dolphin-plugin/build

%check
cargo test --release --workspace --locked

%post
/usr/bin/update-mime-database %{_datadir}/mime &>/dev/null || :
/usr/bin/update-desktop-database &>/dev/null || :
/usr/bin/gtk-update-icon-cache %{_datadir}/icons/hicolor &>/dev/null || :

%postun
/usr/bin/update-mime-database %{_datadir}/mime &>/dev/null || :
/usr/bin/update-desktop-database &>/dev/null || :
/usr/bin/gtk-update-icon-cache %{_datadir}/icons/hicolor &>/dev/null || :

%post dolphin
kbuildsycoca6 --noincremental &>/dev/null || :

%postun dolphin
kbuildsycoca6 --noincremental &>/dev/null || :

%files
%license LICENSE
%doc README.md
%{_bindir}/odrive-cli
%{_bindir}/odrive-gui
%{_datadir}/applications/io.github.keithvassallomt.odrive-linux.desktop
%{_datadir}/applications/odrive-linux-open.desktop
%{_datadir}/mime/packages/odrive-linux.xml
%{_datadir}/icons/hicolor/*/apps/odrive-menu.png
%{_datadir}/icons/hicolor/*/apps/io.github.keithvassallomt.odrive-linux.png
%{_datadir}/icons/hicolor/*/emblems/odrive-*.png
%{_datadir}/icons/hicolor/*/mimetypes/odrive-*.png
%{_datadir}/icons/hicolor/*/places/odrive-*.png
%{_datadir}/icons/hicolor/*/status/odrive-*.png

%files nautilus
%{_datadir}/nautilus-python/extensions/odrive-linux.py

%files dolphin
%{_libdir}/qt6/plugins/kf6/kfileitemaction/odriveaction.so
%{_libdir}/qt6/plugins/kf6/overlayicon/odriveoverlay.so

%changelog
* Wed May 06 2026 Keith Vassallo <keith@icemalta.com> - 0.1.0-1
- Initial release.
- GTK4/Libadwaita Manager with onboarding wizard, mount management,
  folder sync rules, backup jobs, trash, encryptor folders, advanced
  agent settings, log viewer, and tray indicator.
- Rust CLI for status, sync/unsync, mount, share-link, and packaging
  payload generation.
- Optional Nautilus right-click integration (subpackage).
- Optional Dolphin Qt6/KF6 integration (subpackage).
