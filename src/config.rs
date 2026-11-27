use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Error;

pub const ZONE_COUNT: usize = 4;
pub const COLOR_MAX: u8 = 19;
pub const BRIGHTNESS_MAX: u8 = 5;
pub const OFF_NAME: &str = "Off";
/// Color 19 writes no light. A new profile starts with it.
pub const BLACK: u8 = 19;

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

/// The color, brightness and effect a profile uses when Split zones is off.
/// The same zone is written to all four parts of the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Whole {
    pub mode: BacklightMode,
    pub zone: Zone,
}

impl Whole {
    /// No color, Medium, Always on.
    pub fn blank() -> Self {
        Self {
            mode: BacklightMode::AlwaysOn,
            zone: Zone::blank(),
        }
    }
}

impl Zone {
    /// Black at Medium: the color a new profile starts with.
    pub fn blank() -> Self {
        Self {
            color: BLACK,
            brightness: 2,
        }
    }
}

fn divided_by_default() -> bool {
    // Profiles saved before Split zones existed used four zones.
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// The effect while Split zones is on.
    pub mode: BacklightMode,
    /// The four zones while Split zones is on.
    pub zones: [Zone; ZONE_COUNT],
    /// Split zones. When false, `whole` lights the keyboard and `mode` and
    /// `zones` are kept for when it is turned back on.
    #[serde(default = "divided_by_default")]
    pub divided: bool,
    #[serde(default = "Whole::blank")]
    pub whole: Whole,
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
            // The values an Off saved before Split zones loads with, so it
            // still matches.
            divided: true,
            whole: Whole::blank(),
            off: true,
        }
    }

    /// A new profile: no color, Medium, Always on, on both sides, with
    /// Split zones off.
    pub fn blank(name: &str) -> Self {
        Self {
            name: name.into(),
            mode: BacklightMode::AlwaysOn,
            zones: [Zone::blank(); ZONE_COUNT],
            divided: false,
            whole: Whole::blank(),
            off: false,
        }
    }

    /// True when the four zones are lit one by one. Off always writes its
    /// four zones, which are all dark.
    pub fn uses_zones(&self) -> bool {
        self.off || self.divided
    }

    /// The effect and the four zones written to the keyboard.
    pub fn effective(&self) -> (BacklightMode, [Zone; ZONE_COUNT]) {
        if self.uses_zones() {
            (self.mode, self.zones)
        } else {
            (self.whole.mode, [self.whole.zone; ZONE_COUNT])
        }
    }

    /// The effect of the side that is in use.
    pub fn active_mode(&self) -> BacklightMode {
        self.effective().0
    }

    pub fn active_mode_mut(&mut self) -> &mut BacklightMode {
        if self.uses_zones() {
            &mut self.mode
        } else {
            &mut self.whole.mode
        }
    }

    /// Zone `index` of the side in use. With Split zones off every index
    /// is the whole keyboard.
    pub fn zone(&self, index: usize) -> Zone {
        self.effective().1[index.min(ZONE_COUNT - 1)]
    }

    pub fn zone_mut(&mut self, index: usize) -> &mut Zone {
        if self.uses_zones() {
            &mut self.zones[index.min(ZONE_COUNT - 1)]
        } else {
            &mut self.whole.zone
        }
    }

    pub fn check(&self) -> Result<(), Error> {
        if self.name.trim().is_empty() {
            return Err(Error::Msg("profile name is empty".into()));
        }
        for zone in self.zones {
            zone.check()?;
        }
        self.whole.zone.check()?;
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
            profiles: vec![Profile::off(), Self::first_profile()],
        }
    }
}

impl Config {
    /// A fresh install lights the whole keyboard red, so it is plain that
    /// the service works.
    fn first_profile() -> Profile {
        let mut profile = Profile::blank("Profile 1");
        profile.whole.zone.color = 0;
        profile
    }

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
            profiles: vec![Profile::blank("A"), Profile::off(), Profile::blank("B")],
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
    fn older_profiles_keep_their_zones_and_gain_a_blank_whole_side() {
        let json = r#"{"selected":1,"profiles":[
            {"name":"Off","mode":"always_on","zones":[{"color":0,"brightness":0},{"color":0,"brightness":0},{"color":0,"brightness":0},{"color":0,"brightness":0}],"off":true},
            {"name":"A","mode":"wave","zones":[{"color":1,"brightness":2},{"color":5,"brightness":4},{"color":1,"brightness":2},{"color":1,"brightness":2}]}
        ]}"#;
        let mut config: Config = serde_json::from_str(json).unwrap();
        assert!(!config.ensure_off(), "an old Off still matches, so nothing is rewritten");
        assert!(config.check().is_ok());
        let old = &config.profiles[1];
        assert!(old.divided);
        assert_eq!(old.whole, Whole::blank());
        assert_eq!(old.effective().0, BacklightMode::Wave);
        assert_eq!(old.effective().1[1].color, 5);

        // Saved again, the new fields are written out and read back.
        let again: Config = serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(again, config);
    }

    #[test]
    fn each_side_keeps_its_own_values() {
        let mut profile = Profile::blank("P");
        assert!(!profile.divided);
        assert_eq!(profile.whole, Whole::blank());
        assert!(profile.zones.iter().all(|zone| *zone == Zone::blank()));
        assert_eq!(Zone::blank(), Zone { color: BLACK, brightness: 2 });
        assert_eq!(Whole::blank().mode, BacklightMode::AlwaysOn);

        // Whole side: one zone and one effect, written four times.
        profile.zone_mut(2).color = 7;
        *profile.active_mode_mut() = BacklightMode::Breath;
        assert_eq!(profile.whole.zone.color, 7);
        assert_eq!(profile.zones[2], Zone::blank(), "the zones are left alone");
        assert_eq!(profile.mode, BacklightMode::AlwaysOn);
        let (mode, zones) = profile.effective();
        assert_eq!(mode, BacklightMode::Breath);
        assert!(zones.iter().all(|zone| zone.color == 7));

        // Divided side: per zone, and the whole side is kept.
        profile.divided = true;
        profile.zone_mut(2).color = 3;
        *profile.active_mode_mut() = BacklightMode::Wave;
        let (mode, zones) = profile.effective();
        assert_eq!(mode, BacklightMode::Wave);
        assert_eq!(zones.map(|zone| zone.color), [BLACK, BLACK, 3, BLACK]);
        assert_eq!(profile.whole.zone.color, 7);
        assert_eq!(profile.whole.mode, BacklightMode::Breath);

        profile.divided = false;
        assert_eq!(profile.zone(0).color, 7);
        assert_eq!(profile.active_mode(), BacklightMode::Breath);

        // Off writes its dark zones whatever it holds.
        assert!(Profile::off().effective().1.iter().all(|zone| zone.brightness == 0));

        let mut bad = Profile::blank("P");
        bad.whole.zone.color = 20;
        assert!(bad.check().is_err());
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
        let first_color = config.profiles[1].zones[0].color;
        let mut second = config.profiles[1].clone();
        second.name = "Profile 2".into();
        second.zones[0].color = 12;
        config.profiles.push(second);
        config.save(&path).unwrap();
        let mut loaded = Config::load(&path).unwrap();
        loaded.profiles[2].zones[0].color = 4;
        assert_eq!(loaded.profiles[1].zones[0].color, first_color);
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
