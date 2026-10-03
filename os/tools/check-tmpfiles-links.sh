#!/bin/bash
# Check a built image against its own systemd-tmpfiles rules.
#
#   sudo os/tools/check-tmpfiles-links.sh IMAGE
#
# Every symlink a package ships that a tmpfiles.d `L` rule also manages must come
# through the image's own systemd-tmpfiles unchanged (a forcing `L+` rule would
# otherwise rewrite it at every boot, and cpkg verify would then report the
# file as changed), and /etc/mtab must read the same as /proc/self/mounts.
# Works on a disposable copy of IMAGE; the image itself is not modified.
set -euo pipefail

image=${1:?usage: check-tmpfiles-links.sh IMAGE}
if [ "$(id -u)" != 0 ]; then
  echo "check-tmpfiles-links.sh: needs root (loop mount, chroot)" >&2
  exit 2
fi

work=$(mktemp -d /var/tmp/check-tmpfiles.XXXXXX)
root="$work/root"
cleanup() {
  if mountpoint -q "$root/proc"; then umount "$root/proc"; fi
  if mountpoint -q "$root"; then umount "$root"; fi
  rm -rf "$work"
}
trap cleanup EXIT

# The partition named "root" in mkimage.sh's GPT layout.
start=$(sfdisk -d "$image" | sed -n 's/.*start= *\([0-9]*\),.*name="root".*/\1/p')
if [ -z "$start" ]; then
  echo "check-tmpfiles-links.sh: no partition named root in $image" >&2
  exit 2
fi
cp --sparse=always "$image" "$work/disk.img"
mkdir "$root"
mount -o "loop,offset=$((start * 512))" "$work/disk.img" "$root"
mount -t proc proc "$root/proc"

# Symlinks the installed packages ship, "path<TAB>target", and the paths the
# image's tmpfiles.d rules manage with a symlink rule (L, L+, L$, L!, ...).
links=$(awk -F'\t' '$1 == "l" { print $5 "\t" $6 }' "$root"/var/lib/cpkg/installed/*/files | sort)
rules=$(chroot "$root" /usr/bin/systemd-tmpfiles --cat-config | awk '$1 ~ /^L/ { print $2 }' | sort -u)

failed=0
checked=()
while IFS=$'\t' read -r path target; do
  if ! grep -qxF "/$path" <<<"$rules"; then
    continue
  fi
  checked+=("/$path")
  before=$(stat -c '%i %N' "$root/$path")
  # --boot also applies the lines only safe at boot (the ! modifier, as in
  # L! /etc/resolv.conf).
  if ! chroot "$root" /usr/bin/systemd-tmpfiles --create --boot --prefix="/$path"; then
    echo "FAILED  systemd-tmpfiles --create for /$path"
    failed=$((failed + 1))
    continue
  fi
  after=$(stat -c '%i %N' "$root/$path")
  now=$(readlink "$root/$path" || true)
  if [ "$before" = "$after" ] && [ "$now" = "$target" ]; then
    echo "ok      /$path -> $target, unchanged by systemd-tmpfiles"
  else
    echo "CHANGED /$path: packaged -> $target, after systemd-tmpfiles -> $now"
    failed=$((failed + 1))
  fi
done <<<"$links"

if ! printf '%s\n' "${checked[@]}" | grep -qxF /etc/mtab; then
  echo "FAILED  /etc/mtab is not both a packaged symlink and managed by a tmpfiles.d rule"
  failed=$((failed + 1))
fi

# What the link means, not how it is spelled: read through /etc/mtab, a process
# sees its own mount table.
if chroot "$root" /usr/bin/cmp -s /etc/mtab /proc/self/mounts; then
  echo "ok      /etc/mtab reads the same as /proc/self/mounts"
else
  echo "DIFFERS /etc/mtab does not read the same as /proc/self/mounts"
  failed=$((failed + 1))
fi

# The image's own cpkg must succeed afterwards (changed configuration alone
# does not make it fail) and must not report any of these paths.
status=0
report=$(chroot "$root" /usr/bin/cpkg verify 2>&1) || status=$?
if [ "$status" -ne 0 ]; then
  echo "FAILED  cpkg verify exited $status:"
  printf '%s\n' "$report" | sed 's/^/        /'
  failed=$((failed + 1))
fi
for p in "${checked[@]}"; do
  if grep -qF ": $p " <<<"$report"; then
    echo "REPORTED by cpkg verify: $(grep -F ": $p " <<<"$report")"
    failed=$((failed + 1))
  fi
done

echo "checked ${#checked[@]} packaged symlinks managed by tmpfiles.d rules"
if [ "$failed" -gt 0 ]; then
  echo "FAILED ($failed)"
  exit 1
fi
echo PASS
