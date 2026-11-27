# Y720 keyboard backlight

This repository is the controller. It is not a fork of the old Electron app and it does not live inside that tree.

## What this program is

One system service owns the keyboard. The window and the `set` command talk to it. Profiles are shared by every account.

- Service: `y720-kbled.service`, user `y720`, group `y720`, `WantedBy=multi-user.target`. It starts at boot and writes the saved color.
- Group `y720` contains only that system account. Do not add people to it, and do not `chmod 666` device nodes.
- Config: `/var/lib/y720-kbled/config.json`. Socket: `/run/y720-kbled/sock` mode `0666`. Any local account may change colors. Only root or the service account may stop the service.
- Closing the window leaves the service running. Numpad 7 opens the window for the person at the keyboard. The service runs `sudo -n /usr/bin/y720-kbled show-ui` for that. The sudoers rule must stay that one command.
- The window is GTK 4, in this process. Do not bring back a Tauri or WebKit view. On this Hyprland session the scale is 1.25, and a web view's painted controls and click targets drift apart when a tile is not a multiple of 4 logical pixels. GTK draws the border and the hit target as one widget. The tray is StatusNotifier over D-Bus, so this process does not link GTK 3.

## Hardware

Lenovo Y720-15IKB. Keep the ITE33D1 reports: four 6-byte feature reports `[0xCC, 0, mode, color, brightness, zone]` and a commit `[0xCC, 9]`. Open hidraw read-write, close the fd, and surface ioctl errors.

- Backlight: hidraw whose parent is `048d:837a`. In udev, match the parent HID device by name: `SUBSYSTEMS=="hid", KERNELS=="*:048D:837A.*"` (it is `0018:048D:837A.0006` here). That device has no `id/vendor` or `id/product` attributes, so `ATTRS{id/vendor}` never matches. There is no sysfs `name` equal to `ITE33D1:00 048D:837A`; the program finds it by `HID_NAME` in `uevent`.
- Hotkeys: input node named exactly `ITE Tech. Inc. ITE Device(8910) Keyboard`. Do not use the sibling `Wireless Radio Control` node (`048d:c100` as well) or the AT keyboard. Scan codes: Fn+Space `786512`, numpad 7 `786515`.
- Zones are left to right, then the numpad, indexes 0–3. Modes: heartbeat 0, breath 1, smooth 2, always_on 3, wave 4. Colors 0–19. The brightness byte is still 0–5. On this keyboard 2 and 3 are the same intensity, and 4 and 5 are the same intensity. The window offers Off, Low, Medium, and High, and writes 0, 1, 2, and 4. Keep accepting 3 and 5 in saved profiles and in `set`.
- Do not load a kernel module. Leave extreme cooling out.

The udev rule is `packaging/70-y720-kbled.rules`. Do not filter on `ACTION=="add"`; `udevadm trigger` would skip it. `GROUP="y720"` and `MODE="0660"` on those two nodes only.

## Packaging

Arch first, Ubuntu 24.04 as well. Build the Arch package with `makepkg` from `packaging/`. This machine uses rustup, so the PKGBUILD finds `~/.cargo/bin` and does not depend on the distro `rust` package. `options=('!debug' '!lto')`.

`pacman -U` upgrades in place. Do not tell the user to remove the package first: `post_remove` deletes `/var/lib/y720-kbled`. Removal must stop and disable the service, reload udev, delete the `y720` account, and delete `/run/y720-kbled` and `/var/lib/y720-kbled`. Every package build gets a higher build number than the last one: raise `pkgrel` in `packaging/PKGBUILD` and `release` in `packaging/build-deb.sh` (kept equal) before each `makepkg` or `build-deb.sh`, even when only the code changed. Reset both to 1 when the version changes.

Flatpak does not fit hidraw and evdev. Do not add it.

## Checks

`cargo test` covers the protocol helpers and the service. A green test run does not mean the keyboard changed color. Say so when the devices were not opened. `sudo` on this machine needs a password, so do not claim a udev rule or a package was installed unless the command succeeded.

## Changelog

`CHANGELOG.md` follows Keep a Changelog. Every user-visible change adds a line under `## [Unreleased]` in the same change, written for people who use the app. On a release, rename that section to the version and date, start a new empty `[Unreleased]`, and update the compare links at the bottom. The version in `Cargo.toml`, `pkgver` in `packaging/PKGBUILD`, `version` in `packaging/build-deb.sh`, and the git tag `vX.Y.Z` match.
