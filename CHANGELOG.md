# Changelog

All notable changes to this project are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Arrow keys move through the color palette for the zone you are editing.
  Left and Right step to the previous or next color, and Up and Down move one
  row. Each step applies right away. The arrows do nothing while Off is
  selected, while you type in a text field, or while the profile list is open.
- Tab selects the next keyboard zone, and Shift+Tab the previous one, wrapping
  from the numpad back to the left zone. Tab does this instead of moving
  between buttons, except while Off is selected or the profile list is open.
- The window reconnects to the backlight service on its own. When the service
  is not running, or restarts during a package upgrade, the window retries
  every 1 to 5 seconds and loads the profiles again once the service answers.

### Changed

- The keyboard and the on-screen keyboard change color at the same time. The
  window sends a change to the keyboard once the frame that shows it has been
  drawn, instead of before.
- Messages in the window start with a capital letter.
- All text in the window is one step smaller.
- The profile dropdown is slimmer.
- "All zones" is always underlined.
- The README, the desktop entry, and the Debian package use the name
  "Lenovo Y720 Keyboard Backlight". The PKGBUILD points at the renamed
  repository.

## [1.0.0] - 2026-11-27

First release.

### Added

- A system service, `y720-kbled.service`, that owns the keyboard backlight. It
  starts at boot and restores the saved profile before anyone logs in.
- A GTK 4 settings window, opened with Fn+numpad 7, from the tray icon, or
  from the app launcher. It fits a quarter tile on a 1920×1080 screen at
  scale 1.25.
- Four zones: three across the main keys, left to right, and the numpad. Click
  a key to pick its zone.
- 20 colors, four brightness levels (Off, Low, Medium, High), and five effects
  (Always on, Heartbeat, Breath, Smooth, Wave).
- Profiles you can add, rename, and delete. Fn+Space cycles through them.
- A built-in Off profile. It is always first in the list, turns every zone
  off, and cannot be renamed, edited, or deleted. Older configs gain it the
  first time the service loads them.
- The `set` command changes the selected profile from a terminal.
- Packages for Arch Linux (`makepkg`) and Ubuntu 24.04 (`build-deb.sh`).

[Unreleased]: https://github.com/impero365/lenovo-Y720-keyboard-backlight/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/impero365/lenovo-Y720-keyboard-backlight/releases/tag/v1.0.0
