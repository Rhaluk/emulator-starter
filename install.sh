#!/bin/sh
# Install the applet for the current user. Paths come from $HOME and XDG, never a fixed account.
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
home=${HOME:?HOME is not set, so the applet cannot be installed for this user.}
data=${XDG_DATA_HOME:-$home/.local/share}
bindir=$home/.local/bin
appdir=$data/applications
icondir=$data/icons/hicolor/scalable/apps
binary=$root/target/release/cosmic-applet-emulator

if [ ! -x "$binary" ]; then
    echo "Release binary is missing. From the project directory, run: cargo build --release" >&2
    exit 1
fi

mkdir -p "$bindir" "$appdir" "$icondir"
install -m 755 "$binary" "$bindir/cosmic-applet-emulator"
awk -v exec="$bindir/cosmic-applet-emulator" '
    /^Exec=/ { print "Exec=" exec; next }
    { print }
' "$root/res/io.github.emulatorstarter.CosmicApplet.desktop" > "$appdir/io.github.emulatorstarter.CosmicApplet.desktop"
chmod 644 "$appdir/io.github.emulatorstarter.CosmicApplet.desktop"
cp "$root/images/android(1).svg" "$icondir/io.github.emulatorstarter.CosmicApplet.svg"
chmod 644 "$icondir/io.github.emulatorstarter.CosmicApplet.svg"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$appdir" >/dev/null 2>&1 || true
fi

echo "Installed cosmic-applet-emulator for $home."
echo "Add Emulators from the COSMIC panel applet list. This script does not change the panel layout."
