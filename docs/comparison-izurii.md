# Comparison with Izurii/Lenovo-Y720-KB-Led-Controller

Compared on 2026-11-27 against https://github.com/Izurii/Lenovo-Y720-KB-Led-Controller at commit `88f4811` (2022-05-21, the last commit). That project is an Electron app with two C++ Node addons (LED and hotkeys) and an optional extreme-cooling kernel module. Its source was read, not built or run.

Both projects send the same ITE33D1 feature reports (`[0xCC, 0, mode, color, brightness, zone]` and the commit `[0xCC, 9]`) and listen for the same scan codes (Fn+Space `786512`, numpad 7 `786515`). The differences are in everything around that.

## Where this project is better

- **Device permissions are safer.** His README tells users to set `MODE="0666"` on every input device whose name contains "Keyboard", and the app offers a `chmod 666` button. That matches the AT keyboard too, so any program or account can read every keystroke. This project gives the two nodes to group `y720` with mode `0660`, and only the service account is in that group.
- **Colors come back at boot.** His runs as the user and only starts after login. This project's system service restores the colors before the login screen.
- **Hotkeys use the right device.** He finds the input device by `048d:c100` and takes the first match. The sibling Wireless Radio Control node has the same IDs. His final 2022 commit was a fix to that device search. This project matches the exact node name `ITE Tech. Inc. ITE Device(8910) Keyboard`.
- **Errors are reported.**
  - His LED addon ignores every `ioctl` result.
  - It checks for a failed open with `if (!fd)`; `open` returns -1 on failure, not 0.
  - It opens the device `O_WRONLY` and never closes it, so it leaks one handle per color change.
  - This project checks each `ioctl`, closes the device, and reports the error.
- **Recovers when the keyboard isn't ready.** This project retries for up to 15 s at boot (30 × 500 ms) and restarts the hotkey listener every 2 s if it stops. On any error, his shows a dialog and quits.
- **Hotkeys react sooner.** His hotkey thread sleeps 100 ms after every event. This project blocks on evdev.
- **Fewer latent bugs.**
  - In his hotkey search, `bool isDeviceProductCorrect, isDeviceVendorCorrect = false;` leaves the first flag uninitialised.
  - He leaks the vendor file handle when the product doesn't match.
  - If a profile has fewer than four zones, he sends uninitialised zone values.
  - His validation messages say "between 0 and 10", but he checks 0–19 and 0–5.
- **Much lighter.** One native GTK 4 binary, compared with an Electron runtime pinned to Node 16.9.1 (now end of life).
- **Brightness matches the hardware.** His offers six levels, with 2 = 3 and 4 = 5. This project offers Off, Low, Medium and High.
- **Also:**
  - A `set` command for scripts.
  - `cargo test` coverage; he has no tests.
  - Profiles shared by every account; his are per account, so two logged-in users would fight over the keyboard.
  - Clean uninstall: stops the service, removes the account, reloads udev.

## Where this project is worse

- **Harder to install.**
  - He ships prebuilt AppImage and `.deb` releases from GitHub Actions; users download and run.
  - This project must be built with Rust first, and there are no GitHub releases yet.
- **More moving parts, though the user sets up none of them.** The Arch package and the `.deb` create the system account, install the udev rule, the sudoers rule and the service, and enable it. Removal tears it all down. On Ubuntu, the profiles and the `y720` account are only deleted on `apt purge`. His needs *more* manual setup: the user writes a udev rule by hand or runs `chmod 666`. What remains on this side:
  - A service, a sudoers rule and a system account are more pieces that can break, and more to check for security.
  - The app works only when installed as a package. A copy built from source but not installed can't reach the keyboard. His AppImage runs without installing, but only after the devices are opened up.
- **Fewer features.**
  - No extreme cooling (left out on purpose).
  - The tray has only Show and Quit. His tray lists profiles to switch between and has a "Start at login" toggle.
  - He shows a preview image for each effect.
- **Always running.** The service can't be switched off from the app. His can be turned off at login.
- **Less proven.** He has releases, users and issue reports from 2021–2022. This project has only been tested on one Y720-15IKB.

## Licensing

His `LICENSE` file is GPL-3.0, but his `package.json` says ISC. This project is ISC. Reusing protocol facts (report layout, scan codes, color indexes) is fine. It would matter only if his code were copied, and what was read doesn't look copied.

## Ideas worth taking from his project

- Prebuilt releases (an Arch package and a `.deb`) on GitHub.
- A profile list in the tray menu.
- Effect previews in the window.
