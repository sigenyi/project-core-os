#!/bin/bash
# Assemble a bootable C.O.R.E. OS disk image from packages alone.
#
#   mkimage.sh --repo DIR --key PUBKEY --out IMAGE [--size 4G] [--password PW]
#
# Every file of the system comes from a .cpk installed with cpkg into an empty
# root; this script adds only what is specific to one machine image: the trusted
# repository key, the root password and the boot loader.
#
# Disk layout (GPT):
#   1  BIOS boot      1 MiB   GRUB core image for legacy BIOS
#   2  EFI system    64 MiB   GRUB as \EFI\BOOT\BOOTX64.EFI
#   3  root            rest   ext4, type "Linux root (x86-64)"
#
# The kernel mounts the root partition by PARTUUID; there is no initramfs.
set -euo pipefail

SIZE=4G
PASSWORD=core
REPO='' KEY='' OUT=''
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) REPO=$2; shift 2 ;;
    --key) KEY=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    --size) SIZE=$2; shift 2 ;;
    --password) PASSWORD=$2; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
[ -n "$REPO" ] && [ -n "$KEY" ] && [ -n "$OUT" ] || { sed -n '4p' "$0" >&2; exit 2; }
[ "$(id -u)" = 0 ] || { echo "must run as root" >&2; exit 1; }
for t in sfdisk mkfs.ext4 mkfs.vfat mmd mcopy losetup; do
  command -v "$t" >/dev/null || { echo "missing host tool: $t" >&2; exit 1; }
done
here=$(cd "$(dirname "$0")" && pwd)
CPKG=${CPKG:-$here/../../target/release/cpkg}

work=$(mktemp -d "${TMPDIR:-/var/tmp}/core-image.XXXXXX")
R="$work/root"
loop=''
dirloop=''
cleanup() {
  mountpoint -q "$R/mnt/grub" && umount "$R/mnt/grub" || true
  [ -n "$dirloop" ] && losetup -d "$dirloop" 2>/dev/null || true
  [ -n "$loop" ] && losetup -d "$loop" 2>/dev/null || true
  mountpoint -q "$R/dev" && umount -l "$R/dev" || true
  rm -rf "$work"
}
trap cleanup EXIT

step() { printf '\n== %s\n' "$*"; }

step "Installing every package of the repository into an empty root"
mkdir -p "$R/etc/cpkg/keys"
install -m644 "$KEY" "$R/etc/cpkg/keys/$(basename "$KEY")"
mapfile -t packages < <(python3 -c 'import json,sys; [print(p["name"]) for p in json.load(open(sys.argv[1]))["packages"]]' "$REPO/index.json")
# filesystem first: it owns the merged-/usr links every other package installs through.
"$CPKG" --root "$R" --repo "$REPO" install filesystem
"$CPKG" --root "$R" --repo "$REPO" install "${packages[@]}"
"$CPKG" --root "$R" verify >/dev/null

step "Machine configuration"
# Empty machine-id: systemd generates one on first boot.
: > "$R/etc/machine-id"
mkdir -p "$R/etc/systemd/network"
cat > "$R/etc/systemd/network/20-wired.network" <<'EOF'
[Match]
Name=en* eth*

[Network]
DHCP=yes
EOF
ln -sfn ../run/systemd/resolve/stub-resolv.conf "$R/etc/resolv.conf"
chroot "$R" /usr/bin/systemctl enable systemd-networkd.service systemd-resolved.service
echo "root:$PASSWORD" | chroot "$R" /usr/bin/chpasswd
# The password must be changed at first login.
chroot "$R" /usr/bin/chage -d 0 root

step "Partitioning"
truncate -s "$SIZE" "$OUT.tmp"
ROOT_PARTUUID=$(cat /proc/sys/kernel/random/uuid)
ROOT_FSUUID=$(cat /proc/sys/kernel/random/uuid)
sfdisk --quiet "$OUT.tmp" <<EOF
label: gpt
size=1MiB, type=21686148-6449-6E6F-744E-656564454649, name=bios
size=64MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name=esp
type=4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709, name=root, uuid=$ROOT_PARTUUID
EOF
part() { sfdisk -J "$OUT.tmp" | python3 -c "import json,sys; p=json.load(sys.stdin)['partitiontable']['partitions'][$1]; print(p['start'], p['size'])"; }
read -r esp_start esp_size < <(part 1)
read -r root_start root_size < <(part 2)

step "Boot loader"
mkdir -p "$R/boot/grub"
cp -r "$R/usr/lib/grub/i386-pc" "$R/usr/lib/grub/x86_64-efi" "$R/boot/grub/"
cat > "$R/boot/grub/grub.cfg" <<EOF
set timeout=3
serial --unit=0 --speed=115200
terminal_input console serial
terminal_output console serial
menuentry "C.O.R.E. OS" {
  linux /boot/vmlinuz root=PARTUUID=$ROOT_PARTUUID rw console=tty0 console=ttyS0,115200
}
EOF
cat > "$work/early.cfg" <<EOF
search --no-floppy --fs-uuid --set=root $ROOT_FSUUID
set prefix=(\$root)/boot/grub
configfile \$prefix/grub.cfg
EOF
mkdir -p "$R/tmp/grub"
cp "$work/early.cfg" "$R/tmp/grub/"
mods=(part_gpt ext2 normal linux search search_fs_uuid configfile echo test serial terminal)
chroot "$R" /usr/bin/grub-mkimage -O x86_64-efi -p /boot/grub -c /tmp/grub/early.cfg \
  -o /tmp/grub/BOOTX64.EFI "${mods[@]}" fat all_video efi_gop
chroot "$R" /usr/bin/grub-mkimage -O i386-pc -p /boot/grub -c /tmp/grub/early.cfg \
  -o /tmp/grub/core.img "${mods[@]}" biosdisk
cp "$R/usr/lib/grub/i386-pc/boot.img" "$R/tmp/grub/"
mv "$R/tmp/grub" "$work/grub"

step "EFI system partition"
mkfs.vfat -n CORE-ESP -C "$work/esp.img" $((esp_size / 2)) >/dev/null
mmd -i "$work/esp.img" ::/EFI ::/EFI/BOOT
mcopy -i "$work/esp.img" "$work/grub/BOOTX64.EFI" ::/EFI/BOOT/BOOTX64.EFI
dd if="$work/esp.img" of="$OUT.tmp" bs=512 seek="$esp_start" conv=notrunc status=none

step "Root file system"
mkfs.ext4 -q -L core-root -U "$ROOT_FSUUID" -O ^metadata_csum_seed -d "$R" \
  "$work/root.img" "$((root_size / 2))k"
dd if="$work/root.img" of="$OUT.tmp" bs=1M oflag=seek_bytes seek="$((root_start * 512))" conv=notrunc,sparse status=none

step "BIOS boot code"
# grub-bios-setup embeds core.img into the BIOS boot partition. It also insists on
# identifying the device holding its GRUB directory, so that directory goes on a
# small loop-mounted file system listed in the device map, next to the target.
loop=$(losetup --find --show "$OUT.tmp")
truncate -s 16M "$work/grubdir.img"
mkfs.ext4 -q "$work/grubdir.img"
dirloop=$(losetup --find --show "$work/grubdir.img")
mkdir -p "$R/dev" "$R/mnt/grub"
mount --bind /dev "$R/dev"
mount "$dirloop" "$R/mnt/grub"
cp "$work/grub/core.img" "$work/grub/boot.img" "$R/mnt/grub/"
printf '(hd0) %s\n(hd1) %s\n' "$loop" "$dirloop" > "$R/mnt/grub/device.map"
chroot "$R" /usr/bin/grub-bios-setup --skip-fs-probe --directory=/mnt/grub \
  --device-map=/mnt/grub/device.map "$loop"
umount "$R/mnt/grub"
rmdir "$R/mnt/grub"
umount -l "$R/dev"
losetup -d "$dirloop"; dirloop=
losetup -d "$loop"; loop=
# The boot sector must now carry GRUB's code and still the protective MBR.
dd if="$OUT.tmp" bs=512 count=1 status=none | od -An -tx1 -j510 | grep -q '55 aa'
dd if="$OUT.tmp" bs=512 count=1 status=none | grep -aq 'GRUB'

mv "$OUT.tmp" "$OUT"
step "Done: $OUT ($SIZE, root PARTUUID $ROOT_PARTUUID)"
