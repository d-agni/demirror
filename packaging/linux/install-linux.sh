#!/bin/sh
# Installs demirror: the app (with a menu entry), the command-line server and client, and
# the virtual display daemon as a system service.
# Usage: sudo packaging/linux/install-linux.sh [USER...]   (default: the user running sudo)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
release="$root/target/release"
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
sed "s|@USERS@|$users|" "$here/demirror-vdisplayd.service" > /etc/systemd/system/demirror-vdisplayd.service
systemctl daemon-reload
systemctl enable demirror-vdisplayd.service
# restart, not just start: picks up a new binary when reinstalling
systemctl restart demirror-vdisplayd.service
systemctl --no-pager --lines=5 status demirror-vdisplayd.service
