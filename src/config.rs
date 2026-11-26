use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Error;

pub const ZONE_COUNT: usize = 4;
pub const COLOR_MAX: u8 = 19;
pub const BRIGHTNESS_MAX: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum BacklightMode {
    Heartbeat = 0,
    Breath = 1,
    Smooth = 2,
    AlwaysOn = 3,
    Wave = 4,
}

impl BacklightMode {
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn parse(text: &str) -> Result<Self, Error> {
        match text.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "heartbeat" | "0" => Ok(Self::Heartbeat),
            "breath" | "1" => Ok(Self::Breath),
            "smooth" | "2" => Ok(Self::Smooth),
            "always_on" | "always" | "3" => Ok(Self::AlwaysOn),
            "wave" | "4" => Ok(Self::Wave),
            other => Err(Error::Msg(format!(
                "unknown backlight mode '{other}'. Use heartbeat, breath, smooth, always_on, or wave"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    pub color: u8,
    pub brightness: u8,
}

impl Zone {
    pub fn check(self) -> Result<(), Error> {
        if self.color > COLOR_MAX {
            return Err(Error::Msg(format!(
                "color must be 0-{COLOR_MAX}, got {}",
                self.color
            )));
        }
        if self.brightness > BRIGHTNESS_MAX {
            return Err(Error::Msg(format!(
                "brightness must be 0-{BRIGHTNESS_MAX}, got {}",
                self.brightness
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub mode: BacklightMode,
    pub zones: [Zone; ZONE_COUNT],
}

impl Profile {
    pub fn check(&self) -> Result<(), Error> {
        if self.name.trim().is_empty() {
            return Err(Error::Msg("profile name is empty".into()));
        }
        for zone in self.zones {
            zone.check()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub selected: usize,
    pub profiles: Vec<Profile>,
}

impl Default for Config {
    fn default() -> Self {
        let zone = Zone {
            color: 0,
            brightness: 3,
        };
        Self {
            selected: 0,
            profiles: vec![Profile {
                name: "Profile 1".into(),
                mode: BacklightMode::AlwaysOn,
                zones: [zone; ZONE_COUNT],
            }],
        }
    }
}

impl Config {
    pub fn check(&self) -> Result<(), Error> {
        if self.profiles.is_empty() {
            return Err(Error::Msg("at least one profile is required".into()));
        }
        if self.selected >= self.profiles.len() {
            return Err(Error::Msg(
                "selected profile is outside the profile list".into(),
            ));
        }
        for profile in &self.profiles {
            profile.check()?;
        }
        Ok(())
    }

    pub fn selected_profile(&self) -> Result<&Profile, Error> {
        self.check()?;
        Ok(&self.profiles[self.selected])
    }

    pub fn selected_profile_mut(&mut self) -> Result<&mut Profile, Error> {
        self.check()?;
        let index = self.selected;
        Ok(&mut self.profiles[index])
    }

    pub fn cycle(&mut self) {
        if self.profiles.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.profiles.len();
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        if !path.exists() {
            let config = Config::default();
            config.save(path)?;
            return Ok(config);
        }
        let text = fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&text)?;
        config.check()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<(), Error> {
        self.check()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&tmp, path)?;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o640));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_cycle_do_not_alias_zones() {
        let dir = std::env::temp_dir().join(format!(
            "y720-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let mut config = Config::default();
        let mut second = config.profiles[0].clone();
        second.name = "Profile 2".into();
        second.zones[0].color = 12;
        config.profiles.push(second);
        config.save(&path).unwrap();
        let mut loaded = Config::load(&path).unwrap();
        loaded.profiles[1].zones[0].color = 4;
        assert_eq!(loaded.profiles[0].zones[0].color, 0);
        loaded.cycle();
        assert_eq!(loaded.selected, 1);
        loaded.cycle();
        assert_eq!(loaded.selected, 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_empty_and_out_of_range() {
        let mut config = Config::default();
        config.profiles.clear();
        assert!(config.check().is_err());
        let mut config = Config::default();
        config.profiles[0].zones[0].color = 20;
        assert!(config.check().is_err());
        assert!(BacklightMode::parse("always-on").is_ok());
        assert!(BacklightMode::parse("nope").is_err());
    }
}
