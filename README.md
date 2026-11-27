<div align="center">
  <img src="assets/icon.png" width="96" alt="Y720 keyboard backlight icon">

  # Y720 keyboard backlight

  *Control the Lenovo Legion Y720 keyboard lights from Linux.*

  [Features](#features) • [Install](#install) • [Usage](#usage)

</div>

One system service owns the keyboard. The GTK 4 window and the `y720-kbled set` command talk to it. Profiles are shared by every account on the machine. Closing the window leaves the lights as they are.

## Features

- Four zones, left to right and then the numpad, each with its own color and effect.
- Effects: Always on, Heartbeat, Breath, Smooth, and Wave. Twenty colors.
- Brightness steps Off, Low, Medium, and High. Saved profiles may still use the in-between hardware values 3 and 5.
- Fn+Space cycles profiles. Fn+numpad 7 opens the window.
- A StatusNotifier tray icon, so the window does not need GTK 3.
- The saved color is written again at boot.

> [!NOTE]
> Fn+numpad 7 is a key the keyboard sends only while Num Lock is off. With Num Lock on, that key is an ordinary 7 and the service never sees the shortcut.

## Install

Arch, from `packaging/`:

```bash
makepkg -si
```

That builds `y720-kbled`, creates the system account `y720`, reloads udev, and enables `y720-kbled.service`. The service starts at boot. Upgrade an existing install with `sudo pacman -U` on the new package. Do not remove the package first: removal deletes `/var/lib/y720-kbled`.

Ubuntu 24.04, from the repository root:

```bash
packaging/build-deb.sh
sudo apt install ./packaging/y720-kbled_1.0.0_amd64.deb
```

The `.deb` depends on `libgtk-4-1`, `sudo`, and `systemd`. Arch uses the `gtk4` package. Flatpak is not used: the program needs hidraw and the keyboard's evdev node.

> [!WARNING]
> `sudo pacman -Rns y720-kbled` stops the service, reloads udev, deletes the `y720` account, and deletes the saved profiles.

### Run from a checkout

Install a stable Rust toolchain and GTK 4, then:

```bash
cargo build --release
./target/release/y720-kbled devices
```

`devices` only prints the LED and keyboard nodes. It does not change the color. The installed service is what applies a profile.

## Usage

Open the window:

```bash
y720-kbled --ui
```

Set the selected profile from a shell. Color is an index from 0 to 19. Brightness is 0 to 5. Zone 0 is the left block and zone 3 is the numpad. Omit `--zone` to change every zone.

```bash
y720-kbled set --mode always_on --color 0 --brightness 2
y720-kbled set --zone 3 --mode wave --color 4
y720-kbled apply
```

Modes are `always_on`, `heartbeat`, `breath`, `smooth`, and `wave`.

The service listens on `/run/y720-kbled/sock`. Profiles live in `/var/lib/y720-kbled/config.json`. Any local account may change colors. Stopping the service needs root or the `y720` account.

> [!TIP]
> `sudo y720-kbled watch` prints Fn+Space and Fn+numpad 7 until you press Ctrl-C. The keyboard node is not readable by a normal login, so this needs root. The service sees the same events and will also open the window or cycle a profile.

## Thanks

Many thanks to [Izurii](https://github.com/Izurii) for [Lenovo-Y720-KB-Led-Controller](https://github.com/Izurii/Lenovo-Y720-KB-Led-Controller), the original Linux controller for this keyboard. That project worked out how to talk to the Y720's backlight, and this app builds on what it showed.
