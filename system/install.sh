#!/usr/bin/env bash
# Install C.O.R.E. onto an existing systemd-based Linux system (an Arch VM is the
# reference target). Turns the machine into a zero-UI, conversational system:
# the chosen user's login shell becomes core-shell.
#
#   sudo system/install.sh [options]
#
# Options:
#   --user NAME       account that gets core-shell as its shell (default: core)
#   --autologin       log that user in on tty1 without a password
#   --disable-gui     disable display managers and boot to the console
#   --with-llama      build llama.cpp into /usr/lib/core/llama
#   --with-whisper    build whisper.cpp into /usr/lib/core/whisper
#   --with-models     download the models in image/models.conf (add --allow-unpinned
#                     if they are not pinned yet)
#   --no-build        use binaries already in target/release
#   --no-enable       install files only; do not enable or start services
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
user_name=core
autologin=0
disable_gui=0
with_llama=0
with_whisper=0
with_models=0
allow_unpinned=()
build=1
enable=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --user) user_name="$2"; shift 2 ;;
        --autologin) autologin=1; shift ;;
        --disable-gui) disable_gui=1; shift ;;
        --with-llama) with_llama=1; shift ;;
        --with-whisper) with_whisper=1; shift ;;
        --with-models) with_models=1; shift ;;
        --allow-unpinned) allow_unpinned=(--allow-unpinned); shift ;;
        --no-build) build=0; shift ;;
        --no-enable) enable=0; shift ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

log() { printf '==> %s\n' "$*"; }
die() { echo "error: $*" >&2; exit 1; }

[[ $EUID -eq 0 ]] || die "run as root (sudo)"
[[ -d /run/systemd/system ]] || die "C.O.R.E. needs systemd as init"

as_user() {
    if [[ -n "${SUDO_USER:-}" && "$SUDO_USER" != root ]]; then
        sudo -u "$SUDO_USER" --preserve-env=PATH,CARGO_HOME,RUSTUP_HOME "$@"
    else
        "$@"
    fi
}

if [[ "$build" == 1 ]]; then
    log "building C.O.R.E. (release)"
    as_user cargo build --manifest-path "$repo/Cargo.toml" --release --locked
fi
for bin in core-shell core-guardian core-sensed core-ctl; do
    [[ -x "$repo/target/release/$bin" ]] || die "missing target/release/$bin; build first"
    install -Dm755 "$repo/target/release/$bin" "/usr/bin/$bin"
done

log "installing units, sysusers and tmpfiles"
while IFS= read -r -d '' f; do
    install -Dm644 "$repo/system/$f" "/$f"
done < <(cd "$repo/system" && find usr -type f -print0)

detect() {
    local pm audio net
    for candidate in pacman apt-get dnf zypper apk xbps-install; do
        if command -v "$candidate" >/dev/null; then pm="$candidate"; break; fi
    done
    case "${pm:-}" in
        apt-get) pm=apt ;;
        xbps-install) pm=xbps ;;
        "") pm=pacman ;;
    esac
    if command -v wpctl >/dev/null; then audio=wpctl
    elif command -v pactl >/dev/null; then audio=pactl
    else audio=amixer; fi
    if command -v nmcli >/dev/null; then net=networkmanager
    elif command -v iwctl >/dev/null; then net=iwd
    else net=networkmanager; fi
    echo "$pm $audio $net"
}

log "installing configuration"
install -dm755 /etc/core
for f in guardian.toml agent.toml inference.env; do
    if [[ -e "/etc/core/$f" ]]; then
        install -m644 "$repo/system/etc/core/$f" "/etc/core/$f.new"
        echo "    kept existing /etc/core/$f (new version at $f.new)"
    else
        install -m644 "$repo/system/etc/core/$f" "/etc/core/$f"
        if [[ "$f" == guardian.toml ]]; then
            read -r pm audio net < <(detect)
            sed -i -e "s/^package_manager = .*/package_manager = \"$pm\"     # detected/" \
                -e "s/^audio = .*/audio = \"$audio\"     # detected/" \
                -e "s/^network = .*/network = \"$net\"     # detected/" /etc/core/guardian.toml
            echo "    detected: packages=$pm audio=$audio network=$net"
        fi
    fi
done
chown root:root /etc/core/*.toml /etc/core/inference.env
chmod 644 /etc/core/*.toml /etc/core/inference.env

systemd-sysusers /usr/lib/sysusers.d/core.conf
systemd-tmpfiles --create /usr/lib/tmpfiles.d/core.conf

log "setting up user $user_name"
grep -qx /usr/bin/core-shell /etc/shells || echo /usr/bin/core-shell >> /etc/shells
if id "$user_name" >/dev/null 2>&1; then
    usermod -a -G core -s /usr/bin/core-shell "$user_name"
else
    useradd -m -G core -s /usr/bin/core-shell -c "C.O.R.E. user" "$user_name"
    echo "    created $user_name; set a password with: passwd $user_name"
fi
home="$(getent passwd "$user_name" | cut -d: -f6)"
touch "$home/.hushlogin" && chown "$user_name:" "$home/.hushlogin"

if [[ "$autologin" == 1 ]]; then
    install -Dm644 "$repo/system/etc/systemd/system/getty@tty1.service.d/autologin.conf" \
        /etc/systemd/system/getty@tty1.service.d/autologin.conf
    sed -i "s/--autologin core /--autologin $user_name /" /etc/systemd/system/getty@tty1.service.d/autologin.conf
fi

if [[ "$disable_gui" == 1 ]]; then
    log "disabling graphical login"
    for dm in gdm sddm lightdm lxdm ly greetd xdm; do
        systemctl disable "$dm.service" 2>/dev/null || true
    done
    systemctl set-default multi-user.target
fi

if [[ "$with_llama" == 1 ]]; then
    log "building llama.cpp"
    "$repo/image/scripts/build-llama.sh" --prefix /usr/lib/core/llama
fi
if [[ "$with_whisper" == 1 ]]; then
    log "building whisper.cpp"
    "$repo/image/scripts/build-whisper.sh" --prefix /usr/lib/core/whisper
fi
if [[ "$with_models" == 1 ]]; then
    log "fetching models"
    "$repo/image/scripts/fetch-models.sh" --dest /usr/share/core/models "${allow_unpinned[@]}"
fi

systemctl daemon-reload
if [[ "$enable" == 1 ]]; then
    log "enabling services"
    systemctl enable --now core-guardian.socket core-sensed.service
    if [[ -e /usr/share/core/models/core.gguf && -x /usr/lib/core/llama/bin/llama-server ]]; then
        systemctl enable --now core-inference.service
    else
        echo "    core-inference not enabled: needs llama.cpp (--with-llama) and a model (--with-models)"
    fi
fi

cat <<EOF

C.O.R.E. is installed.
  check health:   core-ctl doctor
  try it now:     sudo -u $user_name core-shell
  at next login:  $user_name gets the conversational shell on the console
EOF
