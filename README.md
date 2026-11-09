# COSMIC Emulator Applet

A COSMIC panel applet that lists the Android emulators installed on the machine, starts the one you pick, and stops the ones that are running. If a path is missing or a device fails, the popup explains why.

## Install

Requires the COSMIC desktop, Rust, and the Android SDK (`emulator` and `adb` on `PATH`, or `ANDROID_HOME`).

```sh
git clone https://github.com/Rhaluk/emulator-starter.git
cd emulator-starter
cargo build --release
./install.sh
```

Then add **Emulators** from the COSMIC panel applet list. Open the popup to read the installed devices once. Press **Read emulators** when you want the list refreshed.
