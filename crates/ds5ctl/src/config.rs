//! Saved settings: a name and a profile for every controller ever configured,
//! keyed by MAC address. Stored in `$XDG_CONFIG_HOME/ds5ctl/config.json`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use dualsense::DeviceInfo;
use serde::{Deserialize, Serialize};

use crate::profile::Profile;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// What a controller gets the first time it is seen.
    pub new_controller_profile: Profile,
    /// Keyed by [`controller_key`].
    pub controllers: BTreeMap<String, Controller>,
    /// The controller the settings window showed last.
    pub last_selected: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Controller {
    pub name: String,
    pub profile: Profile,
}

/// Stable identity of a controller across reconnects and USB/Bluetooth.
pub fn controller_key(info: &DeviceInfo) -> String {
    info.mac.clone().unwrap_or_else(|| info.instance.clone())
}

/// "DualSense 2C:77": the model plus the end of the MAC address, which is
/// also printed on the label inside the controller.
pub fn default_name(info: &DeviceInfo) -> String {
    match info.mac.as_deref() {
        Some(mac) if mac.len() >= 5 => {
            format!("{} {}", info.model, mac[mac.len() - 5..].to_uppercase())
        }
        _ => info.model.to_string(),
    }
}

fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("ds5ctl"))
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        config_dir().map(|dir| dir.join("config.json"))
    }

    /// Loads the saved config. Without one, a profile saved by ds5ctl 0.1
    /// (`profile.json`) becomes the profile for new controllers.
    pub fn load() -> Result<Config, String> {
        let dir = config_dir().ok_or("cannot locate the config directory ($HOME is unset)")?;
        if let Some(config) = read_json(&dir.join("config.json"))? {
            return Ok(config);
        }
        let legacy = read_json::<Profile>(&dir.join("profile.json"))?;
        Ok(Config {
            new_controller_profile: legacy.unwrap_or_default(),
            ..Config::default()
        })
    }

    pub fn save(&self) -> io::Result<PathBuf> {
        let path = Config::path().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "cannot locate the config directory",
            )
        })?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        // Write then rename so a crash never leaves a half-written file.
        let tmp = path.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_string_pretty(self).map_err(io::Error::other)?,
        )?;
        fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// The profile to send to a controller; unknown ones get
    /// `new_controller_profile`.
    pub fn profile_for(&self, key: &str) -> &Profile {
        self.controllers
            .get(key)
            .map_or(&self.new_controller_profile, |c| &c.profile)
    }

    /// Adds a controller seen for the first time. Returns whether it was new.
    pub fn remember(&mut self, info: &DeviceInfo) -> bool {
        let key = controller_key(info);
        if self.controllers.contains_key(&key) {
            return false;
        }
        let controller = Controller {
            name: default_name(info),
            profile: self.new_controller_profile.clone(),
        };
        self.controllers.insert(key, controller);
        true
    }

    /// Name shown for a controller: its saved name, else a generated one.
    pub fn name_of(&self, info: &DeviceInfo) -> String {
        self.controllers
            .get(&controller_key(info))
            .map(|c| c.name.trim())
            .filter(|name| !name.is_empty())
            .map_or_else(|| default_name(info), str::to_string)
    }

    /// Finds a controller key by its saved name, ignoring case.
    pub fn key_by_name(&self, name: &str) -> Option<&str> {
        self.controllers
            .iter()
            .find(|(_, c)| c.name.trim().eq_ignore_ascii_case(name.trim()))
            .map(|(key, _)| key.as_str())
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<Option<T>, String> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dualsense::{Model, Transport};

    fn device(mac: Option<&str>) -> DeviceInfo {
        DeviceInfo {
            hidraw: "/dev/hidraw4".into(),
            instance: "0005:054C:0CE6.0006".into(),
            model: Model::DualSense,
            transport: Transport::Bluetooth,
            mac: mac.map(str::to_string),
        }
    }

    #[test]
    fn new_controllers_start_from_the_template() {
        let mut config = Config::default();
        config.new_controller_profile.lightbar.color = [1, 2, 3];
        let pad = device(Some("a0:fa:9c:0a:2c:77"));

        assert_eq!(
            config.profile_for("a0:fa:9c:0a:2c:77").lightbar.color,
            [1, 2, 3]
        );
        assert!(config.remember(&pad));
        assert!(!config.remember(&pad));

        let saved = &config.controllers["a0:fa:9c:0a:2c:77"];
        assert_eq!(saved.name, "DualSense 2C:77");
        assert_eq!(saved.profile.lightbar.color, [1, 2, 3]);
    }

    #[test]
    fn controllers_keep_separate_profiles() {
        let mut config = Config::default();
        config.remember(&device(Some("aa:aa:aa:aa:aa:aa")));
        config.remember(&device(Some("bb:bb:bb:bb:bb:bb")));
        config
            .controllers
            .get_mut("aa:aa:aa:aa:aa:aa")
            .unwrap()
            .profile
            .mic_led = dualsense::MicLed::On;
        assert_eq!(
            config.profile_for("aa:aa:aa:aa:aa:aa").mic_led,
            dualsense::MicLed::On
        );
        assert_eq!(
            config.profile_for("bb:bb:bb:bb:bb:bb").mic_led,
            dualsense::MicLed::Off
        );
    }

    #[test]
    fn names() {
        let mut config = Config::default();
        let pad = device(Some("a0:fa:9c:0a:2c:77"));
        assert_eq!(config.name_of(&pad), "DualSense 2C:77");
        config.remember(&pad);
        config
            .controllers
            .get_mut("a0:fa:9c:0a:2c:77")
            .unwrap()
            .name = "White".into();
        assert_eq!(config.name_of(&pad), "White");
        assert_eq!(config.key_by_name("white"), Some("a0:fa:9c:0a:2c:77"));
        assert_eq!(config.key_by_name("black"), None);

        // Without a MAC the kernel instance identifies the controller.
        let anonymous = device(None);
        assert_eq!(controller_key(&anonymous), "0005:054C:0CE6.0006");
        assert_eq!(default_name(&anonymous), "DualSense");
    }

    #[test]
    fn json_round_trip() {
        let mut config = Config::default();
        config.remember(&device(Some("a0:fa:9c:0a:2c:77")));
        config.last_selected = Some("a0:fa:9c:0a:2c:77".into());
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<Config>(&json).unwrap(), config);
    }
}
