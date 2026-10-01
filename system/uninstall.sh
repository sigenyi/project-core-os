#!/usr/bin/env bash
# Remove C.O.R.E. from a system installed with system/install.sh.
# Configuration in /etc/core, models and the audit log are kept unless --purge.
#
#   sudo system/uninstall.sh [--user NAME] [--purge]
set -euo pipefail

user_name=core
purge=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --user) user_name="$2"; shift 2 ;;
        --purge) purge=1; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 1; }

systemctl disable --now core-inference.service core-whisper.service core-sensed.service \
    core-guardian.service core-guardian.socket 2>/dev/null || true
if id "$user_name" >/dev/null 2>&1 && [[ "$(getent passwd "$user_name" | cut -d: -f7)" == /usr/bin/core-shell ]]; then
    usermod -s /bin/bash "$user_name"
    echo "restored /bin/bash as $user_name's shell"
fi
rm -f /etc/systemd/system/getty@tty1.service.d/autologin.conf
rm -f /usr/bin/core-shell /usr/bin/core-guardian /usr/bin/core-sensed /usr/bin/core-ctl
rm -f /usr/lib/systemd/system/core-{guardian.socket,guardian.service,sensed.service,inference.service,whisper.service}
rm -f /usr/lib/sysusers.d/core.conf /usr/lib/tmpfiles.d/core.conf /usr/lib/systemd/system-preset/80-core.preset
sed -i '\|^/usr/bin/core-shell$|d' /etc/shells
if [[ "$purge" == 1 ]]; then
    rm -rf /etc/core /usr/lib/core /usr/share/core /var/log/core
fi
systemctl daemon-reload
echo "C.O.R.E. removed."
