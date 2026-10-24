#!/bin/sh
# Install the Worlds launcher and icon for the current user, so the taskbar
# and app menu show the Worlds icon instead of a placeholder.
#
# On Wayland a taskbar doesn't take the icon from the window: it looks up
# a .desktop file whose name matches the window's app id ("worlds"), and
# reads the icon named there from the icon theme. Without the file, KDE
# shows the Wayland logo (a W on an orange circle) and GNOME a grey cog.
#
#     assets/icon/install-desktop.sh [path/to/worlds]
#
# The argument is the editor binary the launcher should run; the default
# is target/release/worlds in this checkout. Pass a bare "worlds" to run
# whatever is on PATH. Files go to ~/.local/share (XDG_DATA_HOME); run it
# again after rebuilding the icon or moving the binary. Nothing needs root.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
bin=${1:-"$repo/target/release/worlds"}
case "$bin" in
    */*) bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin") ;;
esac
# From inside a toolbox or distrobox the host's filesystem shows under
# /run/host; the launcher runs on the host, so give it the host's path.
case "$bin" in
    /run/host/*) bin=${bin#/run/host} ;;
esac
data=${XDG_DATA_HOME:-"$HOME/.local/share"}

for size in 64 128 256 512; do
    dir="$data/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$dir"
    cp "$here/icon-$size.png" "$dir/worlds.png"
done

mkdir -p "$data/applications"
sed "s|^Exec=worlds |Exec=$bin |" \
    "$repo/assets/worlds.desktop" > "$data/applications/worlds.desktop"

# Tell the desktop about the new files; both are optional helpers.
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$data/applications" || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q "$data/icons/hicolor" 2>/dev/null || true

echo "installed $data/applications/worlds.desktop (Exec=$bin) and the icons"
