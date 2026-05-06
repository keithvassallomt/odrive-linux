#!/usr/bin/env bash
#
# Local convenience driver: build the .deb + .rpm packages inside
# debian:13 and fedora:41 containers via podman (or docker), and drop
# every artifact into ./dist/ on the host. Same images and steps the
# release.yml workflow uses, so a successful local run means the GH
# Actions run will succeed too.
#
# Usage:
#   packaging/build-local.sh             # build both .deb and .rpm
#   packaging/build-local.sh deb         # only .deb
#   packaging/build-local.sh rpm         # only .rpm
#

set -euo pipefail

cd "$(dirname "$0")/.."

# ---------- runtime ----------
if command -v podman >/dev/null 2>&1; then
    RUNTIME=podman
elif command -v docker >/dev/null 2>&1; then
    RUNTIME=docker
else
    echo "podman or docker required (apt install podman / dnf install podman)" >&2
    exit 1
fi

# ---------- args ----------
WANT_DEB=1; WANT_RPM=1
case "${1:-}" in
    deb) WANT_RPM=0 ;;
    rpm) WANT_DEB=0 ;;
    "") ;;
    *) echo "usage: $0 [deb|rpm]" >&2; exit 2 ;;
esac

VER="$(grep -m1 '^version' odrive-cli/Cargo.toml | cut -d'"' -f2)"
[ -n "$VER" ] || { echo "could not read version from odrive-cli/Cargo.toml" >&2; exit 1; }

mkdir -p dist
# Sweep prior artifacts so a re-run doesn't leave stale files behind.
rm -f dist/*.deb dist/*.rpm dist/*.changes dist/*.buildinfo dist/*.tar.gz

echo ">>> odrive-linux ${VER} → dist/ (runtime: ${RUNTIME})"

# ---------- source tarball (.rpm needs this; .deb builds from working tree) ----------
if [ "$WANT_RPM" -eq 1 ]; then
    echo ">>> staging source tarball for rpmbuild"
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT
    mkdir -p "$tmpdir/odrive-linux-${VER}"
    # Working-tree snapshot — captures uncommitted changes so iteration
    # before tagging is friction-free. Excludes build outputs and VCS dirs.
    tar -cf - \
        --exclude=./target \
        --exclude=./.git \
        --exclude=./dist \
        --exclude=./dolphin-plugin/build \
        --exclude=./__pycache__ \
        --exclude=./debian/.debhelper \
        --exclude=./debian/cargo-home \
        --exclude=./debian/odrive-linux \
        --exclude=./debian/odrive-linux-nautilus \
        --exclude=./debian/odrive-linux-dolphin \
        --exclude=./debian/tmp \
        --exclude=./debian/files \
        --exclude=./debian/debhelper-build-stamp \
        --exclude='./debian/*.substvars' \
        . | tar -xf - -C "$tmpdir/odrive-linux-${VER}"
    tar -czf "dist/odrive-linux-${VER}.tar.gz" -C "$tmpdir" "odrive-linux-${VER}"
fi

# ---------- .deb ----------
if [ "$WANT_DEB" -eq 1 ]; then
    echo ">>> building .deb (debian:13)"
    $RUNTIME run --rm \
        -v "$PWD:/work:Z" \
        -v "$PWD/dist:/out:Z" \
        -w /work \
        debian:13 bash -ec '
            export DEBIAN_FRONTEND=noninteractive
            apt-get update -qq
            apt-get install -y --no-install-recommends \
                build-essential debhelper devscripts dpkg-dev fakeroot pkg-config \
                libgtk-4-dev libadwaita-1-dev libsqlite3-dev \
                cmake extra-cmake-modules \
                qt6-base-dev libkf6kio-dev libkf6coreaddons-dev libkf6i18n-dev \
                ca-certificates rustup lintian
            rustup default stable
            export PATH="$HOME/.cargo/bin:$PATH"
            dpkg-buildpackage -us -uc -b
            mv ../odrive-linux*.deb /out/
            mv ../*.buildinfo ../*.changes /out/ 2>/dev/null || true
            echo "=== lintian ==="
            lintian /out/odrive-linux_*.deb /out/odrive-linux-nautilus_*.deb /out/odrive-linux-dolphin_*.deb \
                | grep -E "^(E|W):" | sort -u || echo "(clean)"
        '
fi

# ---------- .rpm ----------
if [ "$WANT_RPM" -eq 1 ]; then
    echo ">>> building .rpm (fedora:41)"
    $RUNTIME run --rm \
        -v "$PWD/packaging:/work-packaging:Z" \
        -v "$PWD/dist:/out:Z" \
        fedora:41 bash -ec "
            dnf install -y rpm-build rpmdevtools rustup pkgconf-pkg-config \
                gtk4-devel libadwaita-devel sqlite-devel \
                cmake extra-cmake-modules \
                qt6-qtbase-devel kf6-kio-devel kf6-kcoreaddons-devel kf6-ki18n-devel \
                desktop-file-utils shared-mime-info gtk-update-icon-cache \
                rpmlint git tar gzip
            rustup-init -y --no-modify-path --default-toolchain stable
            export PATH=\$HOME/.cargo/bin:\$PATH
            mkdir -p /root/rpmbuild/{SOURCES,SPECS,BUILD,RPMS,SRPMS}
            cp /out/odrive-linux-${VER}.tar.gz /root/rpmbuild/SOURCES/
            cp /work-packaging/rpm/odrive-linux.spec /root/rpmbuild/SPECS/
            rpmbuild -ba /root/rpmbuild/SPECS/odrive-linux.spec
            cp /root/rpmbuild/RPMS/*/*.rpm /out/
            cp /root/rpmbuild/SRPMS/*.rpm /out/
            echo '=== rpmlint ==='
            rpmlint /out/*.rpm 2>&1 | tail -20 || true
        "
fi

echo
echo ">>> done. Artifacts:"
ls -lh dist/ | grep -E '\.(deb|rpm|tar\.gz)$' || true
