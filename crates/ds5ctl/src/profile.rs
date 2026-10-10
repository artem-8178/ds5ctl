//! The settings of one controller and how they map onto output reports.
//! [`crate::config::Config`] stores one per controller.

use dualsense::{
    AudioOutput, MicLed, OutputReport, PlayerLedBrightness, TriggerEffect, TriggerSide,
    HEADPHONE_VOLUME_MAX, MOTOR_POWER_REDUCTION_MAX, PLAYER_PATTERNS, SPEAKER_VOLUME_MAX,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub lightbar: Lightbar,
    pub player_leds: PlayerLeds,
    pub mic_led: MicLed,
    pub left_trigger: TriggerEffect,
    pub right_trigger: TriggerEffect,
    pub vibration: Vibration,
    pub audio: Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lightbar {
    pub color: [u8; 3],
    /// Percent, 0-100. The hardware has no brightness of its own, so the
    /// colour is scaled.
    pub brightness: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerLeds {
    /// Bit 0 is the leftmost LED.
    pub mask: u8,
    pub brightness: PlayerLedBrightness,
}

/// Strengths in 12.5% steps: 1 (weakest) to 8 (full power).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Vibration {
    pub rumble_strength: u8,
    pub trigger_strength: u8,
}

/// Only sent when `enabled`, so the kernel's automatic headphone/speaker
/// switching keeps working for people who never touch these settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    pub enabled: bool,
    pub output: AudioOutput,
    pub speaker_volume: u8,
    pub headphone_volume: u8,
    pub mic_volume: u8,
    pub mic_muted: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            lightbar: Lightbar::default(),
            player_leds: PlayerLeds::default(),
            mic_led: MicLed::Off,
            left_trigger: TriggerEffect::Off,
            right_trigger: TriggerEffect::Off,
            vibration: Vibration::default(),
            audio: Audio::default(),
        }
    }
}

impl Default for Lightbar {
    fn default() -> Self {
        Lightbar {
            color: [0, 0, 255],
            brightness: 100,
        }
    }
}

impl Default for PlayerLeds {
    fn default() -> Self {
        PlayerLeds {
            mask: PLAYER_PATTERNS[0],
            brightness: PlayerLedBrightness::High,
        }
    }
}

impl Default for Vibration {
    fn default() -> Self {
        Vibration {
            rumble_strength: 8,
            trigger_strength: 8,
        }
    }
}

impl Default for Audio {
    fn default() -> Self {
        Audio {
            enabled: false,
            output: AudioOutput::Speaker,
            speaker_volume: SPEAKER_VOLUME_MAX,
            headphone_volume: HEADPHONE_VOLUME_MAX / 2,
            mic_volume: 0x20,
            mic_muted: false,
        }
    }
}

impl Lightbar {
    pub fn output_rgb(&self) -> (u8, u8, u8) {
        let scale = |c: u8| (u16::from(c) * u16::from(self.brightness.min(100)) / 100) as u8;
        (
            scale(self.color[0]),
            scale(self.color[1]),
            scale(self.color[2]),
        )
    }
}

/// Maps a 1 (weakest) to 8 (full) strength to the controller's power reduction.
pub fn strength_to_reduction(strength: u8) -> u8 {
    (MOTOR_POWER_REDUCTION_MAX + 1).saturating_sub(strength.clamp(1, 8))
}

impl Profile {
    /// Adds every setting that differs from `previous` (all of them when
    /// `None`) to `report`. Returns whether anything was added.
    pub fn write_changes(
        &self,
        previous: Option<&Profile>,
        report: &mut OutputReport,
    ) -> dualsense::Result<bool> {
        let changed =
            |differs: fn(&Profile, &Profile) -> bool| previous.is_none_or(|p| differs(p, self));
        let mut any = false;

        if changed(|a, b| a.lightbar != b.lightbar) {
            let (r, g, b) = self.lightbar.output_rgb();
            report.lightbar(r, g, b);
            any = true;
        }
        if changed(|a, b| a.player_leds != b.player_leds) {
            report.player_leds(self.player_leds.mask, false);
            report.player_led_brightness(self.player_leds.brightness);
            any = true;
        }
        if changed(|a, b| a.mic_led != b.mic_led) {
            report.mic_led(self.mic_led);
            any = true;
        }
        if changed(|a, b| a.left_trigger != b.left_trigger) {
            report.trigger(TriggerSide::Left, &self.left_trigger)?;
            any = true;
        }
        if changed(|a, b| a.right_trigger != b.right_trigger) {
            report.trigger(TriggerSide::Right, &self.right_trigger)?;
            any = true;
        }
        if changed(|a, b| a.vibration != b.vibration) {
            report.motor_power_reduction(
                strength_to_reduction(self.vibration.rumble_strength),
                strength_to_reduction(self.vibration.trigger_strength),
            );
            any = true;
        }
        if self.audio.enabled && changed(|a, b| a.audio != b.audio) {
            let audio = &self.audio;
            report
                .audio_output(audio.output)
                .speaker_volume(audio.speaker_volume)
                .headphone_volume(audio.headphone_volume)
                .mic_volume(audio.mic_volume)
                .mic_mute(audio.mic_muted);
            any = true;
        }
        Ok(any)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_profile_on_first_apply_then_only_changes() {
        let mut profile = Profile::default();
        let mut report = OutputReport::new();
        assert!(profile.write_changes(None, &mut report).unwrap());

        let previous = profile.clone();
        let mut report = OutputReport::new();
        assert!(!profile.write_changes(Some(&previous), &mut report).unwrap());
        assert_eq!(report, OutputReport::new());

        profile.lightbar.color = [255, 0, 0];
        let mut expected = OutputReport::new();
        expected.lightbar(255, 0, 0);
        let mut report = OutputReport::new();
        assert!(profile.write_changes(Some(&previous), &mut report).unwrap());
        assert_eq!(report, expected);
    }

    #[test]
    fn audio_is_only_sent_when_enabled() {
        let mut profile = Profile::default();
        let previous = profile.clone();
        profile.audio.speaker_volume = 10;
        let mut report = OutputReport::new();
        assert!(!profile.write_changes(Some(&previous), &mut report).unwrap());
        profile.audio.enabled = true;
        assert!(profile.write_changes(Some(&previous), &mut report).unwrap());
    }

    #[test]
    fn brightness_scales_colour() {
        let lightbar = Lightbar {
            color: [200, 100, 0],
            brightness: 50,
        };
        assert_eq!(lightbar.output_rgb(), (100, 50, 0));
    }

    #[test]
    fn strength_maps_to_reduction() {
        assert_eq!(strength_to_reduction(8), 0);
        assert_eq!(strength_to_reduction(1), 7);
        assert_eq!(strength_to_reduction(0), 7);
    }

    #[test]
    fn json_round_trip_and_partial_files() {
        let profile = Profile {
            right_trigger: TriggerEffect::Weapon {
                start: 2,
                end: 6,
                strength: 8,
            },
            ..Profile::default()
        };
        let json = serde_json::to_string(&profile).unwrap();
        assert!(json.contains(r#""mode":"weapon""#));
        assert_eq!(serde_json::from_str::<Profile>(&json).unwrap(), profile);

        // Missing fields fall back to defaults so old files keep loading.
        let partial: Profile = serde_json::from_str(r#"{"mic_led":"pulse"}"#).unwrap();
        assert_eq!(partial.mic_led, MicLed::Pulse);
        assert_eq!(partial.lightbar, Lightbar::default());
    }
}
