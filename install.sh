#!/usr/bin/env bash
#
# odrive-linux installer. Detects distro family (Debian/Ubuntu vs
# Fedora/RHEL), picks the right .deb / .rpm packages from the latest
# GitHub release, decides which file-manager integration to install
# based on the running desktop, and runs apt/dnf with sudo.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/keithvassallomt/odrive-linux/main/install.sh | bash
#
# Re-running the script upgrades to the latest release.
#
# Flags:
#   --tag <vX.Y.Z>    install a specific release instead of latest
#   --base-only       skip Nautilus/Dolphin integration even if the DE
#                     would normally pull one
#   --all             install both Nautilus and Dolphin integration
#                     packages regardless of the running DE
#   --dry-run         print the steps without executing them
#

set -euo pipefail

REPO="keithvassallomt/odrive-linux"
RELEASE_API_LATEST="https://api.github.com/repos/${REPO}/releases/latest"
RELEASE_API_TAG="https://api.github.com/repos/${REPO}/releases/tags"

# ---------- args ----------
TAG=""
BASE_ONLY=0
ALL=0
DRY_RUN=0
while [ $# -gt 0 ]; do
    case "$1" in
        --tag) TAG="$2"; shift 2 ;;
        --tag=*) TAG="${1#*=}"; shift ;;
        --base-only) BASE_ONLY=1; shift ;;
        --all) ALL=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
done

# ---------- helpers ----------
log()  { printf '\033[1;34m>>>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mxxx\033[0m %s\n' "$*" >&2; exit 1; }

run() {
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '\033[2m[dry-run]\033[0m %s\n' "$*"
    else
        eval "$@"
    fi
}

# ---------- privilege ----------
if [ "$(id -u)" -eq 0 ]; then
    SUDO=""
elif command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
else
    die "this script needs root or sudo to install packages"
fi

# ---------- distro detection ----------
[ -r /etc/os-release ] || die "/etc/os-release missing — cannot detect distro"
. /etc/os-release

family=""
case " ${ID:-} ${ID_LIKE:-} " in
    *" debian "*|*" ubuntu "*) family="deb" ;;
    *" rhel "*|*" fedora "*|*" centos "*) family="rpm" ;;
esac
[ -n "$family" ] || die "unsupported distro: ${PRETTY_NAME:-${ID:-unknown}}"
log "detected ${family} distro: ${PRETTY_NAME:-$ID}"

# ---------- arch ----------
case "$(uname -m)" in
    x86_64|amd64) arch_rpm="x86_64"; arch_deb="amd64" ;;
    aarch64|arm64) arch_rpm="aarch64"; arch_deb="arm64" ;;
    *) die "unsupported architecture: $(uname -m)" ;;
esac

# ---------- desktop / file manager detection ----------
de_token=""
for v in "${XDG_CURRENT_DESKTOP:-}" "${DESKTOP_SESSION:-}"; do
    case "${v,,}" in
        *kde*|*plasma*)        de_token="kde"   ; break ;;
        *gnome*|*pop*|*ubuntu*|*cinnamon*|*pantheon*|*unity*|*budgie*)
                               de_token="gnome" ; break ;;
    esac
done

have_nautilus=0; command -v nautilus >/dev/null 2>&1 && have_nautilus=1
have_dolphin=0;  command -v dolphin  >/dev/null 2>&1 && have_dolphin=1

want_nautilus=0
want_dolphin=0
if [ "$BASE_ONLY" -eq 1 ]; then
    : # neither
elif [ "$ALL" -eq 1 ]; then
    want_nautilus=1; want_dolphin=1
else
    case "$de_token" in
        kde)   want_dolphin=1 ;;
        gnome) want_nautilus=1 ;;
        *)
            # Unknown DE — fall back to what's actually installed.
            want_nautilus=$have_nautilus
            want_dolphin=$have_dolphin
            ;;
    esac
fi

log "integration: nautilus=${want_nautilus} dolphin=${want_dolphin}"

# ---------- find release ----------
command -v curl >/dev/null 2>&1 || die "curl is required"

if [ -n "$TAG" ]; then
    api="${RELEASE_API_TAG}/${TAG}"
else
    api="$RELEASE_API_LATEST"
fi
log "querying ${api}"
release_json="$(curl -fsSL "$api")" || die "could not query GitHub releases"

tag="$(printf '%s' "$release_json" | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name"[^"]*"([^"]+)".*/\1/')"
[ -n "$tag" ] || die "no tag_name in release response"
ver="${tag#v}"
log "release: ${tag}"

# ---------- asset names ----------
case "$family" in
    deb)
        base_pkg="odrive-linux_${ver}_${arch_deb}.deb"
        nautilus_pkg="odrive-linux-nautilus_${ver}_all.deb"
        dolphin_pkg="odrive-linux-dolphin_${ver}_${arch_deb}.deb"
        ;;
    rpm)
        base_pkg="odrive-linux-${ver}-1.${arch_rpm}.rpm"
        nautilus_pkg="odrive-linux-nautilus-${ver}-1.noarch.rpm"
        dolphin_pkg="odrive-linux-dolphin-${ver}-1.${arch_rpm}.rpm"
        ;;
esac

# ---------- download ----------
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

download() {
    local f="$1"
    log "downloading ${f}"
    run "curl -fL --progress-bar -o '$tmpdir/$f' 'https://github.com/${REPO}/releases/download/${tag}/${f}'" \
        || die "download failed: ${f}"
}

download "$base_pkg"
[ "$want_nautilus" -eq 1 ] && download "$nautilus_pkg"
[ "$want_dolphin" -eq 1 ]  && download "$dolphin_pkg"

# ---------- install ----------
files=("$tmpdir/$base_pkg")
[ "$want_nautilus" -eq 1 ] && files+=("$tmpdir/$nautilus_pkg")
[ "$want_dolphin" -eq 1 ]  && files+=("$tmpdir/$dolphin_pkg")

case "$family" in
    deb)
        log "installing via apt"
        # apt-get install of local .deb files resolves system Depends
        # against the configured repos. --install-recommends pulls
        # python3-nautilus / dolphin (declared as Recommends in our
        # control file) which apt would otherwise skip on local installs.
        run "$SUDO apt-get update -qq"
        run "$SUDO apt-get install -y --install-recommends ${files[*]}"
        ;;
    rpm)
        log "installing via dnf"
        # dnf honours Weak Deps (Recommends) by default, so
        # nautilus-python / dolphin pull in transparently.
        run "$SUDO dnf install -y ${files[*]}"
        ;;
esac

log "done."
echo
echo "Next steps:"
echo "  1. Launch the Manager: \`odrive-gui\` or via your app grid."
echo "  2. Walk through the wizard to install the agent and authenticate."
echo "  3. After your first mount, run: \`odrive-cli setup\`"
echo "     (pads existing placeholders, applies mount-folder icon, sets"
echo "      the placeholder MIME default app for your user)."
