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
#   --from-dir <dir>  install from a local directory of .deb / .rpm files
#                     (skips GitHub entirely — used to test built artifacts
#                     before tagging a release; works against `dist/` after
#                     `packaging/build-local.sh`)
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
FROM_DIR=""
BASE_ONLY=0
ALL=0
DRY_RUN=0
while [ $# -gt 0 ]; do
    case "$1" in
        --tag) TAG="$2"; shift 2 ;;
        --tag=*) TAG="${1#*=}"; shift ;;
        --from-dir) FROM_DIR="$2"; shift 2 ;;
        --from-dir=*) FROM_DIR="${1#*=}"; shift ;;
        --base-only) BASE_ONLY=1; shift ;;
        --all) ALL=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
done

if [ -n "$TAG" ] && [ -n "$FROM_DIR" ]; then
    echo "--tag and --from-dir are mutually exclusive" >&2
    exit 2
fi

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

# ---------- locate packages ----------
# Pick a single best match for a glob, excluding debug subpackages. Empty
# string if nothing matches. Used by both --from-dir and the soon-to-be-
# fetched tmpdir; keeps the caller's selection logic uniform.
pick_one() {
    local dir="$1" pat="$2" f
    # `compgen -G` does the glob without a non-zero exit on no-match.
    while IFS= read -r f; do
        case "$(basename "$f")" in
            *-dbgsym_*|*-debuginfo-*|*-debugsource-*) continue ;;
        esac
        echo "$f"
        return
    done < <(compgen -G "${dir}/${pat}" || true)
}

tmpdir="$(mktemp -d)"
chmod 0755 "$tmpdir"  # apt's _apt user needs to be able to read inside.
trap 'rm -rf "$tmpdir"' EXIT

if [ -n "$FROM_DIR" ]; then
    [ -d "$FROM_DIR" ] || die "--from-dir: ${FROM_DIR} is not a directory"
    FROM_DIR="$(cd "$FROM_DIR" && pwd)"
    log "installing from local directory: ${FROM_DIR}"

    case "$family" in
        deb)
            src_base="$(pick_one "$FROM_DIR"     "odrive-linux_*_${arch_deb}.deb")"
            src_nautilus="$(pick_one "$FROM_DIR" "odrive-linux-nautilus_*_all.deb")"
            src_dolphin="$(pick_one "$FROM_DIR"  "odrive-linux-dolphin_*_${arch_deb}.deb")"
            ;;
        rpm)
            src_base="$(pick_one "$FROM_DIR"     "odrive-linux-[0-9]*.${arch_rpm}.rpm")"
            src_nautilus="$(pick_one "$FROM_DIR" "odrive-linux-nautilus-[0-9]*.noarch.rpm")"
            src_dolphin="$(pick_one "$FROM_DIR"  "odrive-linux-dolphin-[0-9]*.${arch_rpm}.rpm")"
            ;;
    esac

    [ -n "$src_base" ] || die "no base package found in ${FROM_DIR} (looked for odrive-linux* matching arch ${arch_deb}/${arch_rpm})"

    # Copy into a world-readable tmpdir so apt's sandboxed _apt user
    # can access them. (Files in $HOME are typically 0700-traversable
    # only by the owner; apt warns and falls back to unsandboxed root,
    # which is noisy even though the install still succeeds.)
    cp "$src_base" "$tmpdir/" && base_pkg="$tmpdir/$(basename "$src_base")"
    log "base:     $(basename "$base_pkg")"
    if [ "$want_nautilus" -eq 1 ] && [ -n "$src_nautilus" ]; then
        cp "$src_nautilus" "$tmpdir/" && nautilus_pkg="$tmpdir/$(basename "$src_nautilus")"
        log "nautilus: $(basename "$nautilus_pkg")"
    fi
    if [ "$want_dolphin" -eq 1 ] && [ -n "$src_dolphin" ]; then
        cp "$src_dolphin" "$tmpdir/" && dolphin_pkg="$tmpdir/$(basename "$src_dolphin")"
        log "dolphin:  $(basename "$dolphin_pkg")"
    fi
    chmod 0644 "$tmpdir"/*
else
    # ---------- GitHub release lookup ----------
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

    case "$family" in
        deb)
            base_name="odrive-linux_${ver}_${arch_deb}.deb"
            nautilus_name="odrive-linux-nautilus_${ver}_all.deb"
            dolphin_name="odrive-linux-dolphin_${ver}_${arch_deb}.deb"
            ;;
        rpm)
            # Workflow builds on fedora:41, so artifacts carry .fc41.
            base_name="odrive-linux-${ver}-1.fc41.${arch_rpm}.rpm"
            nautilus_name="odrive-linux-nautilus-${ver}-1.fc41.noarch.rpm"
            dolphin_name="odrive-linux-dolphin-${ver}-1.fc41.${arch_rpm}.rpm"
            ;;
    esac

    download() {
        local f="$1"
        log "downloading ${f}"
        run "curl -fL --progress-bar -o '$tmpdir/$f' 'https://github.com/${REPO}/releases/download/${tag}/${f}'" \
            || die "download failed: ${f}"
    }

    download "$base_name"
    [ "$want_nautilus" -eq 1 ] && download "$nautilus_name"
    [ "$want_dolphin" -eq 1 ]  && download "$dolphin_name"

    base_pkg="$tmpdir/$base_name"
    nautilus_pkg="$tmpdir/$nautilus_name"
    dolphin_pkg="$tmpdir/$dolphin_name"
fi

# ---------- install ----------
files=("$base_pkg")
[ "$want_nautilus" -eq 1 ] && [ -n "$nautilus_pkg" ] && files+=("$nautilus_pkg")
[ "$want_dolphin" -eq 1 ]  && [ -n "$dolphin_pkg"  ] && files+=("$dolphin_pkg")

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
