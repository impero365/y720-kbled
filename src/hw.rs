use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use evdev::{Device, EventType};

use crate::config::{Profile, ZONE_COUNT};
use crate::error::Error;

pub const HID_NAME_NEEDLE: &str = "HID_NAME=ITE33D1:00 048D:837A";
pub const KEYBOARD_NAME: &str = "ITE Tech. Inc. ITE Device(8910) Keyboard";
pub const HOTKEY_FN_SPACE: i32 = 786512;
pub const HOTKEY_NUMPAD_7: i32 = 786515;

/// `_IOC(_IOC_READ|_IOC_WRITE, 'H', 0x06, len)`, checked against `<linux/hidraw.h>`.
pub fn hid_set_feature_request(len: usize) -> libc::c_ulong {
    const NRSHIFT: u32 = 0;
    const TYPESHIFT: u32 = 8;
    const SIZESHIFT: u32 = 16;
    const DIRSHIFT: u32 = 30;
    const READ: u32 = 2;
    const WRITE: u32 = 1;
    ((READ | WRITE) as libc::c_ulong) << DIRSHIFT
        | (len as libc::c_ulong) << SIZESHIFT
        | (b'H' as libc::c_ulong) << TYPESHIFT
        | (0x06 as libc::c_ulong) << NRSHIFT
}

pub fn zone_report(mode: u8, color: u8, brightness: u8, zone: u8) -> [u8; 6] {
    [0xcc, 0, mode, color, brightness, zone]
}

/// The four zone reports for the side of `profile` in use. With Split zones
/// off, the same color and brightness go to every zone.
pub fn profile_reports(profile: &Profile) -> [[u8; 6]; ZONE_COUNT] {
    let (mode, zones) = profile.effective();
    std::array::from_fn(|index| {
        let zone = zones[index];
        zone_report(mode.code(), zone.color, zone.brightness, index as u8)
    })
}

pub fn commit_report() -> [u8; 2] {
    [0xcc, 9]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hotkey {
    Cycle,
    Show,
}

impl Hotkey {
    pub fn from_scan(value: i32) -> Option<Self> {
        match value {
            HOTKEY_FN_SPACE => Some(Self::Cycle),
            HOTKEY_NUMPAD_7 => Some(Self::Show),
            _ => None,
        }
    }
}

pub fn find_hidraw_device(sys_class_hidraw: &Path) -> Result<PathBuf, Error> {
    let entries = fs::read_dir(sys_class_hidraw).map_err(|source| Error::Open {
        path: sys_class_hidraw.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let uevent = entry.path().join("device/uevent");
        let text = fs::read_to_string(&uevent).unwrap_or_default();
        if text.lines().any(|line| line.contains(HID_NAME_NEEDLE)) {
            return Ok(PathBuf::from("/dev").join(name.as_ref()));
        }
    }
    Err(Error::Msg(
        "LED controller not found (no hidraw device with HID_NAME ITE33D1:00 048D:837A)".into(),
    ))
}

pub fn find_keyboard_device(sys_class_input: &Path) -> Result<PathBuf, Error> {
    let entries = fs::read_dir(sys_class_input).map_err(|source| Error::Open {
        path: sys_class_input.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("event") {
            continue;
        }
        let device_name = fs::read_to_string(entry.path().join("device/name")).unwrap_or_default();
        if device_name.trim() == KEYBOARD_NAME {
            return Ok(PathBuf::from("/dev/input").join(name.as_ref()));
        }
    }
    Err(Error::Msg(format!(
        "keyboard event device not found (expected name '{KEYBOARD_NAME}')"
    )))
}

pub fn keyboard_ready() -> Result<PathBuf, Error> {
    let path = find_keyboard_device(Path::new("/sys/class/input"))?;
    OpenOptions::new()
        .read(true)
        .open(&path)
        .map_err(|source| open_error(&path, source))?;
    Ok(path)
}

pub fn apply_profile(profile: &Profile) -> Result<(), Error> {
    profile.check()?;
    let path = find_hidraw_device(Path::new("/sys/class/hidraw"))?;
    apply_profile_to(&path, profile)
}

pub fn apply_profile_to(path: &Path, profile: &Profile) -> Result<(), Error> {
    profile.check()?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| open_error(path, source))?;
    for mut report in profile_reports(profile) {
        set_feature(file.as_raw_fd(), &mut report)?;
    }
    let mut commit = commit_report();
    set_feature(file.as_raw_fd(), &mut commit)?;
    file.flush()?;
    Ok(())
}

fn open_error(path: &Path, source: std::io::Error) -> Error {
    if source.kind() == ErrorKind::PermissionDenied {
        Error::Msg(format!(
            "cannot open {}: permission denied. The y720-kbled service must be running so group y720 owns this device",
            path.display()
        ))
    } else {
        Error::Open {
            path: path.to_path_buf(),
            source,
        }
    }
}

fn set_feature(fd: i32, buffer: &mut [u8]) -> Result<(), Error> {
    let request = hid_set_feature_request(buffer.len());
    let rc = unsafe { libc::ioctl(fd, request, buffer.as_mut_ptr()) };
    if rc < 0 {
        Err(Error::Msg(format!(
            "HID feature report failed: {}",
            std::io::Error::last_os_error()
        )))
    } else {
        Ok(())
    }
}

pub fn watch(tx: Sender<Hotkey>) -> Result<(), Error> {
    let path = find_keyboard_device(Path::new("/sys/class/input"))?;
    let mut device = Device::open(&path).map_err(|source| open_error(&path, source))?;
    let mut pending: Option<Hotkey> = None;
    loop {
        let events = device.fetch_events().map_err(|source| Error::Open {
            path: path.clone(),
            source,
        })?;
        for event in events {
            match event.event_type() {
                EventType::MISC => pending = Hotkey::from_scan(event.value()),
                EventType::KEY if event.value() == 0 => {
                    if let Some(hotkey) = pending.take() {
                        if tx.send(hotkey).is_err() {
                            return Ok(());
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

pub fn device_report() -> String {
    let mut lines = Vec::new();
    match find_hidraw_device(Path::new("/sys/class/hidraw")) {
        Ok(path) => lines.push(format!(
            "LED hidraw: {} ({})",
            path.display(),
            access_label(&path, true)
        )),
        Err(error) => lines.push(format!("LED hidraw: {error}")),
    }
    match find_keyboard_device(Path::new("/sys/class/input")) {
        Ok(path) => lines.push(format!(
            "Keyboard: {} ({})",
            path.display(),
            access_label(&path, false)
        )),
        Err(error) => lines.push(format!("Keyboard: {error}")),
    }
    lines.join("\n")
}

fn access_label(path: &Path, write: bool) -> String {
    let mut options = OpenOptions::new();
    options.read(true);
    if write {
        options.write(true);
    }
    match options.open(path) {
        Ok(_) => "ok".into(),
        Err(error) if error.kind() == ErrorKind::PermissionDenied => "permission denied".into(),
        Err(error) => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "y720-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn ioctl_numbers_match_linux_headers() {
        assert_eq!(hid_set_feature_request(6), 0xc0064806);
        assert_eq!(hid_set_feature_request(2), 0xc0024806);
        assert_eq!(zone_report(3, 1, 4, 2), [0xcc, 0, 3, 1, 4, 2]);
        assert_eq!(commit_report(), [0xcc, 9]);
    }

    #[test]
    fn reports_follow_divide_area() {
        use crate::config::{BacklightMode, Zone};
        let mut profile = Profile::blank("P");
        profile.whole.mode = BacklightMode::Breath;
        profile.whole.zone = Zone { color: 8, brightness: 4 };
        profile.mode = BacklightMode::Wave;
        profile.zones[1] = Zone { color: 3, brightness: 1 };
        assert_eq!(
            profile_reports(&profile),
            [
                [0xcc, 0, 1, 8, 4, 0],
                [0xcc, 0, 1, 8, 4, 1],
                [0xcc, 0, 1, 8, 4, 2],
                [0xcc, 0, 1, 8, 4, 3],
            ]
        );
        profile.divided = true;
        assert_eq!(
            profile_reports(&profile),
            [
                [0xcc, 0, 4, 19, 2, 0],
                [0xcc, 0, 4, 3, 1, 1],
                [0xcc, 0, 4, 19, 2, 2],
                [0xcc, 0, 4, 19, 2, 3],
            ]
        );
        assert!(profile_reports(&Profile::off())
            .iter()
            .all(|report| report[4] == 0));
    }

    #[test]
    fn finds_the_named_devices_and_skips_the_radio_node() {
        let root = scratch("sys");
        let hid = root.join("hidraw/hidraw2/device");
        fs::create_dir_all(&hid).unwrap();
        fs::write(
            hid.join("uevent"),
            "DRIVER=hid-generic\nHID_NAME=ITE33D1:00 048D:837A\n",
        )
        .unwrap();
        let input = root.join("input");
        let radio = input.join("event9/device");
        let keyboard = input.join("event8/device");
        fs::create_dir_all(&radio).unwrap();
        fs::create_dir_all(&keyboard).unwrap();
        fs::write(
            radio.join("name"),
            "ITE Tech. Inc. ITE Device(8910) Wireless Radio Control\n",
        )
        .unwrap();
        fs::write(keyboard.join("name"), format!("{KEYBOARD_NAME}\n")).unwrap();

        assert_eq!(
            find_hidraw_device(&root.join("hidraw")).unwrap(),
            PathBuf::from("/dev/hidraw2")
        );
        assert_eq!(
            find_keyboard_device(&input).unwrap(),
            PathBuf::from("/dev/input/event8")
        );
        let _ = fs::remove_dir_all(root);
    }
}
