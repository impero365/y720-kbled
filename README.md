# Y720 Keyboard LEDs

Userspace controller for the Lenovo Legion Y720 keyboard backlight.

The service writes four 6-byte ITE33D1 feature reports (`0xCC`, mode, color, brightness, zone) and a 2-byte commit (`0xCC`, `9`). Fn+Space cycles profiles. Numpad 7 opens the window.

## Run from a checkout

Install the Rust stable toolchain, plus `gtk3`, `webkit2gtk-4.1`, and `libayatana-appindicator` (Arch names). Ubuntu 24.04 uses `libwebkit2gtk-4.1-0`, `libgtk-3-0t64`, and `libayatana-appindicator3-1`.

```bash
cd y720-kbled
cargo build --release
./target/release/y720-kbled devices
```

## Install

Arch, from `packaging/`:

```bash
makepkg -si
```

One system service owns the backlight and the ITE hotkey keyboard. It runs as the system account `y720`. People are not added to that group. The install script creates that account, reloads udev, and enables `y720-kbled.service`. The service starts at boot and writes the saved color. Every account uses the same profiles.

```bash
y720-kbled --ui
y720-kbled set --mode always_on --color 0 --brightness 3
```

The window talks to the service. Closing it leaves the service running. Fn+Space cycles profiles. Numpad 7 opens the window for the person logged in at the keyboard.

Remove it with pacman. The script stops the service, reloads udev, deletes the system account, and deletes `/var/lib/y720-kbled`.

```bash
sudo pacman -Rns y720-kbled
```

Ubuntu 24.04, from `y720-kbled/`:

```bash
packaging/build-deb.sh
sudo apt install ./packaging/y720-kbled_0.1.0_amd64.deb
```

The `.deb` dependencies are the Ubuntu 24.04 package names above. Flatpak is not used: hidraw and evdev access does not fit the sandbox.

Profiles are stored in `/var/lib/y720-kbled/config.json`. The service socket is `/run/y720-kbled/sock`.
