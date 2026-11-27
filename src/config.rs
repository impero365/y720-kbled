use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Error;

pub const ZONE_COUNT: usize = 4;
pub const COLOR_MAX: u8 = 19;
pub const BRIGHTNESS_MAX: u8 = 5;
pub const OFF_NAME: &str = "Off";

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
    /// The built-in profile that turns the backlight off. Every config has
    /// exactly one. It cannot be renamed, edited, or deleted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub off: bool,
}

impl Profile {
    pub fn off() -> Self {
        Self {
            name: OFF_NAME.into(),
            mode: BacklightMode::AlwaysOn,
            zones: [Zone {
                color: 0,
                brightness: 0,
            }; ZONE_COUNT],
            off: true,
        }
    }

    /// A lit profile with the default color, for a new profile made while
    /// Off is selected.
    pub fn lit(name: &str) -> Self {
        Self {
            name: name.into(),
            mode: BacklightMode::AlwaysOn,
            zones: [Zone {
                color: 0,
                brightness: 3,
            }; ZONE_COUNT],
            off: false,
        }
    }

    pub fn check(&self) -> Result<(), Error> {
        if self.name.trim().is_empty() {
            return Err(Error::Msg("profile name is empty".into()));
        }
        for zone in self.zones {
            zone.check()?;
        }
        if self.off && *self != Self::off() {
            return Err(Error::Msg("the Off profile cannot be changed".into()));
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
        Self {
            selected: 1,
            profiles: vec![Profile::off(), Profile::lit("Profile 1")],
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
        match self.profiles.iter().filter(|profile| profile.off).count() {
            1 => {}
            0 => return Err(Error::Msg("the Off profile cannot be deleted".into())),
            _ => return Err(Error::Msg("there can be only one Off profile".into())),
        }
        if !self.profiles[0].off {
            return Err(Error::Msg("the Off profile must stay first".into()));
        }
        Ok(())
    }

    /// Adds the Off profile to a config saved before it existed, and keeps
    /// the same profile selected. Returns true when it changed anything.
    pub fn ensure_off(&mut self) -> bool {
        let mut changed = false;
        let mut seen = false;
        let mut index = 0;
        while index < self.profiles.len() {
            if self.profiles[index].off {
                if seen {
                    self.profiles.remove(index);
                    if self.selected > index {
                        self.selected -= 1;
                    }
                    changed = true;
                    continue;
                }
                seen = true;
                if self.profiles[index] != Profile::off() {
                    self.profiles[index] = Profile::off();
                    changed = true;
                }
            }
            index += 1;
        }
        if !seen {
            self.profiles.insert(0, Profile::off());
            if self.profiles.len() > 1 {
                self.selected += 1;
            }
            changed = true;
        } else if let Some(at) = self.profiles.iter().position(|profile| profile.off) {
            // Off always heads the list. Move it there and keep the same
            // profile selected.
            if at != 0 {
                let off = self.profiles.remove(at);
                self.profiles.insert(0, off);
                if self.selected == at {
                    self.selected = 0;
                } else if self.selected < at {
                    self.selected += 1;
                }
                changed = true;
            }
        }
        if self.selected >= self.profiles.len() {
            self.selected = self.profiles.len() - 1;
            changed = true;
        }
        changed
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
        let mut config: Config = serde_json::from_str(&text)?;
        if config.ensure_off() {
            config.save(path)?;
        }
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
    fn older_configs_gain_off_and_keep_their_selection() {
        let json = r#"{"selected":1,"profiles":[
            {"name":"A","mode":"always_on","zones":[{"color":1,"brightness":2},{"color":1,"brightness":2},{"color":1,"brightness":2},{"color":1,"brightness":2}]},
            {"name":"B","mode":"wave","zones":[{"color":2,"brightness":4},{"color":2,"brightness":4},{"color":2,"brightness":4},{"color":2,"brightness":4}]}
        ]}"#;
        let mut config: Config = serde_json::from_str(json).unwrap();
        assert!(config.ensure_off());
        assert!(config.check().is_ok());
        assert_eq!(config.profiles.len(), 3);
        assert!(config.profiles[0].off);
        assert_eq!(config.profiles[config.selected].name, "B");
        assert!(!config.ensure_off(), "a second pass changes nothing");

        // Off found further down moves to the top, and the selection follows
        // the same profile.
        let mut moved = Config {
            selected: 2,
            profiles: vec![Profile::lit("A"), Profile::off(), Profile::lit("B")],
        };
        assert!(moved.check().is_err(), "Off must be first");
        assert!(moved.ensure_off());
        assert!(moved.profiles[0].off);
        assert_eq!(moved.profiles[moved.selected].name, "B");
        assert!(moved.check().is_ok());
        let saved = serde_json::to_string(&config).unwrap();
        assert_eq!(saved.matches("\"off\"").count(), 1, "only Off stores the flag");
    }

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
        let mut second = config.profiles[1].clone();
        second.name = "Profile 2".into();
        second.zones[0].color = 12;
        config.profiles.push(second);
        config.save(&path).unwrap();
        let mut loaded = Config::load(&path).unwrap();
        loaded.profiles[2].zones[0].color = 4;
        assert_eq!(loaded.profiles[1].zones[0].color, 0);
        // Fn+Space cycles through Off as well.
        loaded.cycle();
        assert_eq!(loaded.selected, 2);
        loaded.cycle();
        assert_eq!(loaded.selected, 0);
        assert!(loaded.profiles[0].off);
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
        let mut config = Config::default();
        config.profiles.remove(0);
        config.selected = 0;
        assert!(config.check().is_err(), "Off cannot be deleted");
        let mut config = Config::default();
        config.profiles[0].zones[2].brightness = 4;
        assert!(config.check().is_err(), "Off cannot be lit");
        let mut config = Config::default();
        config.profiles[0].name = "Dark".into();
        assert!(config.check().is_err(), "Off cannot be renamed");
        let mut config = Config::default();
        config.profiles.push(Profile::off());
        assert!(config.check().is_err(), "only one Off");
        assert!(BacklightMode::parse("always-on").is_ok());
        assert!(BacklightMode::parse("nope").is_err());
    }
}
