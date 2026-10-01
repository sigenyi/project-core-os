#!/usr/bin/env bash
# Build the C.O.R.E. OS live ISO.
#
# Runs on Arch Linux as root (needs archiso, rust, cmake, git, curl); on any other
# host use image/build-in-container.sh. Steps:
#   1. build the C.O.R.E. binaries (cargo, release)
#   2. build llama.cpp and whisper.cpp (portable CPU + optional Vulkan)
#   3. fetch and verify the models in image/models.conf
#   4. assemble an archiso profile: releng + our packages + system/ + image/profile/
#   5. run mkarchiso
#
#   sudo image/build-iso.sh [--no-models] [--allow-unpinned] [--vulkan] [--out DIR]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
work="${WORK_DIR:-$here/work}"
out="$here/out"
cache="${CACHE_DIR:-$here/cache}"
releng="${RELENG:-/usr/share/archiso/configs/releng}"
models=1
allow_unpinned=()
vulkan=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-models) models=0; shift ;;
        --allow-unpinned) allow_unpinned=(--allow-unpinned); shift ;;
        --vulkan) vulkan=(--vulkan); shift ;;
        --out) out="$2"; shift 2 ;;
        -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

log() { printf '\n==> %s\n' "$*"; }
die() { echo "error: $*" >&2; exit 1; }

[[ $EUID -eq 0 ]] || die "mkarchiso needs root; run with sudo"
for tool in mkarchiso cargo cmake git curl; do
    command -v "$tool" >/dev/null || die "$tool is required"
done
[[ -d "$releng" ]] || die "archiso releng profile not found at $releng"

# Build as the invoking user so the cargo target dir is not root-owned.
as_user() {
    if [[ -n "${SUDO_USER:-}" && "$SUDO_USER" != root ]]; then
        sudo -u "$SUDO_USER" --preserve-env=PATH,CARGO_HOME,RUSTUP_HOME "$@"
    else
        "$@"
    fi
}

log "building C.O.R.E. binaries"
as_user cargo build --manifest-path "$repo/Cargo.toml" --release --locked

log "building llama.cpp"
mkdir -p "$cache"
LLAMA_SRC="$cache/llama.cpp" "$here/scripts/build-llama.sh" --prefix "$cache/llama" "${vulkan[@]}"

log "building whisper.cpp"
WHISPER_SRC="$cache/whisper.cpp" "$here/scripts/build-whisper.sh" --prefix "$cache/whisper"

if [[ "$models" == 1 ]]; then
    log "fetching models"
    "$here/scripts/fetch-models.sh" --dest "$cache/models" "${allow_unpinned[@]}"
fi

log "assembling archiso profile"
profile="$work/profile"
rm -rf "$profile"
mkdir -p "$work"
cp -a "$releng" "$profile"
cp "$here/profile/packages.x86_64" "$profile/packages.x86_64"
cat "$here/profile/profiledef.append.sh" >> "$profile/profiledef.sh"
airootfs="$profile/airootfs"
# releng enables its own network stack (systemd-networkd + iwd) and services we do
# not ship; NetworkManager owns networking here. systemd-resolved stays enabled.
rm -rf "$airootfs/etc/systemd/network"
find "$airootfs/etc/systemd/system" -path '*.wants/*' \( -name 'systemd-networkd*' -o -name 'iwd.service' \
    -o -name 'sshd.service' -o -name 'reflector.service' -o -name 'choose-mirror.service' \
    -o -name 'livecd-*' \) -delete
cp -a "$repo/system/etc" "$repo/system/usr" "$airootfs/"
cp -a "$here/profile/airootfs/." "$airootfs/"

install -Dm755 -t "$airootfs/usr/bin" \
    "$repo/target/release/core-shell" "$repo/target/release/core-guardian" \
    "$repo/target/release/core-sensed" "$repo/target/release/core-ctl"
mkdir -p "$airootfs/usr/lib/core"
cp -a "$cache/llama" "$airootfs/usr/lib/core/llama"
cp -a "$cache/whisper" "$airootfs/usr/lib/core/whisper"
if [[ "$models" == 1 ]]; then
    cp -a "$cache/models/." "$airootfs/usr/share/core/models/"
fi

# archiso does not apply presets, so enable units with explicit symlinks.
enable() {
    local target="$1" unit="$2"
    mkdir -p "$airootfs/etc/systemd/system/$target.wants"
    ln -sfn "/usr/lib/systemd/system/$unit" "$airootfs/etc/systemd/system/$target.wants/$unit"
}
enable sockets.target core-guardian.socket
enable multi-user.target core-sensed.service
enable multi-user.target core-inference.service
enable multi-user.target NetworkManager.service

# Quiet, branded boot straight into the conversational shell.
kernel_args="quiet loglevel=3 rd.udev.log_level=3 systemd.show_status=auto"
while IFS= read -r -d '' cfg; do
    sed -i -e "/archisobasedir=/ s|\$| $kernel_args|" \
        -e 's/Arch Linux install medium/C.O.R.E. OS/g' -e 's/Arch Linux/C.O.R.E. OS/g' "$cfg"
done < <(find "$profile/syslinux" "$profile/efiboot" "$profile/grub" -type f \( -name '*.cfg' -o -name '*.conf' \) -print0 2>/dev/null)

log "running mkarchiso"
mkdir -p "$out"
rm -rf "$work/build"
mkarchiso -v -r -w "$work/build" -o "$out" "$profile"
log "done: ISO written to $out"
