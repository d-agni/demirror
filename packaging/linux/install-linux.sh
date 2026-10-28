#!/bin/sh
# Installs demirror: the app (with a menu entry), the command-line server and client, and
# the virtual display daemon as a system service.
# Usage: sudo ./install-linux.sh [USER...]   (default: the user running sudo)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# From an unpacked release the programs sit next to this script; in the source tree they're
# in target/release.
if [ -x "$here/vmon-vdisplayd" ]; then
    release="$here"
else
    release="$(cd "$here/../.." && pwd)/target/release"
fi
bin="$release/vmon-vdisplayd"

if [ "$(id -u)" -ne 0 ]; then
    echo "run with sudo" >&2
    exit 1
fi
if [ ! -x "$bin" ]; then
    echo "build first: cargo build --release" >&2
    exit 1
fi

[ $# -eq 0 ] && set -- "${SUDO_USER:-}"
users=""
for user in "$@"; do
    [ -n "$user" ] && users="$users --user $user"
done

install -m 0755 "$bin" /usr/local/bin/vmon-vdisplayd
for program in demirror vmon-server vmon-client; do
    if [ -x "$release/$program" ]; then
        install -m 0755 "$release/$program" "/usr/local/bin/$program"
    fi
done
install -m 0644 "$here/demirror.desktop" /usr/share/applications/demirror.desktop
icon="$here/demirror.png"
[ -f "$icon" ] || icon="$here/../icons/demirror.png"
install -D -m 0644 "$icon" /usr/share/icons/hicolor/256x256/apps/demirror.png
gtk-update-icon-cache -q /usr/share/icons/hicolor 2>/dev/null || true
sed "s|@USERS@|$users|" "$here/demirror-vdisplayd.service" > /etc/systemd/system/demirror-vdisplayd.service
systemctl daemon-reload
systemctl enable demirror-vdisplayd.service
# restart, not just start: picks up a new binary when reinstalling
systemctl restart demirror-vdisplayd.service
systemctl --no-pager --lines=5 status demirror-vdisplayd.service
