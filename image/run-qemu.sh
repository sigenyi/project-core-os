#!/usr/bin/env bash
# Boot a C.O.R.E. OS ISO in QEMU.
#
#   image/run-qemu.sh [ISO] [--uefi] [--memory 8G] [--cpus 4] [--disk FILE]
#
# The model needs RAM: give the VM at least 6 GB for the default 4B model.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Newest ISO in image/out by default.
# shellcheck disable=SC2012
iso="$(ls -1t "$here"/out/*.iso 2>/dev/null | head -n 1 || true)"
memory=8G
cpus=4
uefi=0
disk=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --uefi) uefi=1; shift ;;
        --memory) memory="$2"; shift 2 ;;
        --cpus) cpus="$2"; shift 2 ;;
        --disk) disk="$2"; shift 2 ;;
        -h|--help) sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) iso="$1"; shift ;;
    esac
done
[[ -f "$iso" ]] || { echo "no ISO found; build one with image/build-iso.sh" >&2; exit 1; }

# QEMU options contain commas by design.
# shellcheck disable=SC2054
args=(
    -m "$memory" -smp "$cpus"
    -cdrom "$iso" -boot order=d
    -nic user,model=virtio-net-pci
    -device virtio-vga
    -audiodev pa,id=snd0 -device intel-hda -device hda-duplex,audiodev=snd0
)
if [[ -r /dev/kvm ]]; then
    args+=(-enable-kvm -cpu host)
fi
if [[ "$uefi" == 1 ]]; then
    for fw in /usr/share/edk2/x64/OVMF.4m.fd /usr/share/OVMF/OVMF_CODE.fd /usr/share/ovmf/OVMF.fd; do
        [[ -f "$fw" ]] && { args+=(-bios "$fw"); break; }
    done
fi
if [[ -n "$disk" ]]; then
    [[ -f "$disk" ]] || qemu-img create -f qcow2 "$disk" 40G
    args+=(-drive "file=$disk,if=virtio,format=qcow2")
fi
exec qemu-system-x86_64 "${args[@]}"
