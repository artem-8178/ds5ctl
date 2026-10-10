//! The main output report ("set state").
//!
//! USB and Bluetooth share a 47-byte common block; only fields whose valid
//! flag is set are applied by the controller, so a report that sets the
//! lightbar leaves triggers, rumble etc. untouched.

use crate::trigger::{TriggerEffect, TriggerSide};
use crate::Result;

pub(crate) const COMMON_LEN: usize = 47;

// Byte offsets inside the common block.
const FLAG0: usize = 0;
const FLAG1: usize = 1;
const MOTOR_RIGHT: usize = 2;
const MOTOR_LEFT: usize = 3;
const HEADPHONE_VOLUME: usize = 4;
const SPEAKER_VOLUME: usize = 5;
const MIC_VOLUME: usize = 6;
const AUDIO_CONTROL: usize = 7;
const MIC_LED: usize = 8;
const POWER_SAVE_CONTROL: usize = 9;
const RIGHT_TRIGGER: usize = 10;
const LEFT_TRIGGER: usize = 21;
const MOTOR_POWER: usize = 36;
const FLAG2: usize = 38;
const LIGHTBAR_SETUP: usize = 41;
const PLAYER_LED_BRIGHTNESS: usize = 42;
const PLAYER_LEDS: usize = 43;
const LIGHTBAR_RGB: usize = 44;

// valid_flag0
const F0_COMPATIBLE_VIBRATION: u8 = 1 << 0;
const F0_HAPTICS_SELECT: u8 = 1 << 1;
const F0_RIGHT_TRIGGER: u8 = 1 << 2;
const F0_LEFT_TRIGGER: u8 = 1 << 3;
const F0_HEADPHONE_VOLUME: u8 = 1 << 4;
const F0_SPEAKER_VOLUME: u8 = 1 << 5;
const F0_MIC_VOLUME: u8 = 1 << 6;
const F0_AUDIO_CONTROL: u8 = 1 << 7;
// valid_flag1
const F1_MIC_LED: u8 = 1 << 0;
const F1_POWER_SAVE_CONTROL: u8 = 1 << 1;
const F1_LIGHTBAR: u8 = 1 << 2;
const F1_PLAYER_LEDS: u8 = 1 << 4;
const F1_MOTOR_POWER: u8 = 1 << 6;
// valid_flag2
const F2_PLAYER_LED_BRIGHTNESS: u8 = 1 << 0;
const F2_LIGHTBAR_SETUP: u8 = 1 << 1;
const F2_COMPATIBLE_VIBRATION2: u8 = 1 << 2;

const POWER_SAVE_MIC_MUTE: u8 = 1 << 4;
const AUDIO_OUTPUT_PATH_SHIFT: u8 = 4;

pub const HEADPHONE_VOLUME_MAX: u8 = 0x7f;
/// Highest speaker volume the PS5 uses; the controller accepts up to 0xff.
pub const SPEAKER_VOLUME_MAX: u8 = 0x64;
/// The mic gain stops increasing past this value.
pub const MIC_VOLUME_MAX: u8 = 0x40;
/// Power reduction steps are 12.5% each; 0 is full power.
pub const MOTOR_POWER_REDUCTION_MAX: u8 = 7;

/// Player LED patterns used by the PS5 (and the kernel) for players 1-5.
/// Bit 0 is the leftmost LED.
pub const PLAYER_PATTERNS: [u8; 5] = [0b00100, 0b01010, 0b10101, 0b11011, 0b11111];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum MicLed {
    Off = 0,
    On = 1,
    Pulse = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum PlayerLedBrightness {
    High = 0,
    Medium = 1,
    Low = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum LightbarFade {
    /// Fade the lightbar in to the current colour.
    In = 1,
    /// Fade the lightbar out (used by the kernel to drop the pairing pulse).
    Out = 2,
}

/// Where the controller sends its USB audio stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum AudioOutput {
    /// Stereo to the 3.5 mm jack, speaker off.
    Headphones = 0,
    /// Mono to the built-in speaker.
    Speaker = 3,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutputReport {
    pub(crate) common: [u8; COMMON_LEN],
    /// The rumble enable flag depends on the firmware version, so it is only
    /// chosen in [`OutputReport::finalize`].
    pub(crate) rumble_requested: bool,
}

impl Default for OutputReport {
    fn default() -> Self {
        Self {
            common: [0; COMMON_LEN],
            rumble_requested: false,
        }
    }
}

impl OutputReport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lightbar(&mut self, r: u8, g: u8, b: u8) -> &mut Self {
        self.common[FLAG1] |= F1_LIGHTBAR;
        self.common[LIGHTBAR_RGB..LIGHTBAR_RGB + 3].copy_from_slice(&[r, g, b]);
        self
    }

    pub fn lightbar_fade(&mut self, fade: LightbarFade) -> &mut Self {
        self.common[FLAG2] |= F2_LIGHTBAR_SETUP;
        self.common[LIGHTBAR_SETUP] = fade as u8;
        self
    }

    /// `mask` uses the low 5 bits, bit 0 = leftmost LED. With `instant` the
    /// LEDs switch immediately instead of fading.
    pub fn player_leds(&mut self, mask: u8, instant: bool) -> &mut Self {
        self.common[FLAG1] |= F1_PLAYER_LEDS;
        self.common[PLAYER_LEDS] = (mask & 0x1f) | if instant { 1 << 5 } else { 0 };
        self
    }

    pub fn player_led_brightness(&mut self, brightness: PlayerLedBrightness) -> &mut Self {
        self.common[FLAG2] |= F2_PLAYER_LED_BRIGHTNESS;
        self.common[PLAYER_LED_BRIGHTNESS] = brightness as u8;
        self
    }

    pub fn mic_led(&mut self, mode: MicLed) -> &mut Self {
        self.common[FLAG1] |= F1_MIC_LED;
        self.common[MIC_LED] = mode as u8;
        self
    }

    /// Classic two-motor rumble. `left` is the heavy (low frequency) motor.
    pub fn rumble(&mut self, left: u8, right: u8) -> &mut Self {
        self.common[FLAG0] |= F0_HAPTICS_SELECT;
        self.common[MOTOR_LEFT] = left;
        self.common[MOTOR_RIGHT] = right;
        self.rumble_requested = true;
        self
    }

    /// Weakens the rumble motors and the trigger effects, 0 (full power) to
    /// [`MOTOR_POWER_REDUCTION_MAX`] in 12.5% steps. Values above are clamped.
    pub fn motor_power_reduction(&mut self, rumble: u8, triggers: u8) -> &mut Self {
        self.common[FLAG1] |= F1_MOTOR_POWER;
        let rumble = rumble.min(MOTOR_POWER_REDUCTION_MAX);
        let triggers = triggers.min(MOTOR_POWER_REDUCTION_MAX);
        self.common[MOTOR_POWER] = (rumble << 4) | triggers;
        self
    }

    /// 0 to [`HEADPHONE_VOLUME_MAX`]; clamped.
    pub fn headphone_volume(&mut self, volume: u8) -> &mut Self {
        self.common[FLAG0] |= F0_HEADPHONE_VOLUME;
        self.common[HEADPHONE_VOLUME] = volume.min(HEADPHONE_VOLUME_MAX);
        self
    }

    /// 0 to [`SPEAKER_VOLUME_MAX`]; clamped.
    pub fn speaker_volume(&mut self, volume: u8) -> &mut Self {
        self.common[FLAG0] |= F0_SPEAKER_VOLUME;
        self.common[SPEAKER_VOLUME] = volume.min(SPEAKER_VOLUME_MAX);
        self
    }

    /// 0 to [`MIC_VOLUME_MAX`]; clamped.
    pub fn mic_volume(&mut self, volume: u8) -> &mut Self {
        self.common[FLAG0] |= F0_MIC_VOLUME;
        self.common[MIC_VOLUME] = volume.min(MIC_VOLUME_MAX);
        self
    }

    /// Selects the audio output. This rewrites the whole audio control byte,
    /// so mic selection goes back to automatic and echo/noise cancelling off.
    pub fn audio_output(&mut self, output: AudioOutput) -> &mut Self {
        self.common[FLAG0] |= F0_AUDIO_CONTROL;
        self.common[AUDIO_CONTROL] = (output as u8) << AUDIO_OUTPUT_PATH_SHIFT;
        self
    }

    /// Mutes the microphone in hardware. This rewrites the power-save byte,
    /// which also un-mutes the speaker, headphones and haptics.
    pub fn mic_mute(&mut self, muted: bool) -> &mut Self {
        self.common[FLAG1] |= F1_POWER_SAVE_CONTROL;
        self.common[POWER_SAVE_CONTROL] = if muted { POWER_SAVE_MIC_MUTE } else { 0 };
        self
    }

    pub fn trigger(&mut self, side: TriggerSide, effect: &TriggerEffect) -> Result<&mut Self> {
        let encoded = effect.encode()?;
        let (flag, offset) = match side {
            TriggerSide::Right => (F0_RIGHT_TRIGGER, RIGHT_TRIGGER),
            TriggerSide::Left => (F0_LEFT_TRIGGER, LEFT_TRIGGER),
        };
        self.common[FLAG0] |= flag;
        self.common[offset..offset + 11].copy_from_slice(&encoded);
        Ok(self)
    }

    /// Picks the rumble enable flag: firmware 2.21+ ignores the legacy one.
    pub(crate) fn finalize(&self, vibration_v2: bool) -> [u8; COMMON_LEN] {
        let mut common = self.common;
        if self.rumble_requested {
            if vibration_v2 {
                common[FLAG2] |= F2_COMPATIBLE_VIBRATION2;
            } else {
                common[FLAG0] |= F0_COMPATIBLE_VIBRATION;
            }
        }
        common
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lightbar_sets_flag_and_colour() {
        let c = OutputReport::new().lightbar(1, 2, 3).finalize(true);
        assert_eq!(c[FLAG1], F1_LIGHTBAR);
        assert_eq!(&c[44..47], &[1, 2, 3]);
        assert_eq!(c[FLAG0], 0);
    }

    #[test]
    fn rumble_flag_depends_on_firmware() {
        let mut r = OutputReport::new();
        r.rumble(10, 20);
        let v2 = r.finalize(true);
        assert_eq!(
            v2[FLAG2] & F2_COMPATIBLE_VIBRATION2,
            F2_COMPATIBLE_VIBRATION2
        );
        assert_eq!(v2[FLAG0] & F0_COMPATIBLE_VIBRATION, 0);
        let v1 = r.finalize(false);
        assert_eq!(v1[FLAG0] & F0_COMPATIBLE_VIBRATION, F0_COMPATIBLE_VIBRATION);
        assert_eq!((v1[MOTOR_LEFT], v1[MOTOR_RIGHT]), (10, 20));
    }

    #[test]
    fn motor_power_packs_rumble_high_triggers_low() {
        let c = OutputReport::new()
            .motor_power_reduction(3, 9)
            .finalize(true);
        assert_eq!(c[FLAG1], F1_MOTOR_POWER);
        assert_eq!(c[MOTOR_POWER], 0x37);
    }

    #[test]
    fn audio_fields() {
        let c = OutputReport::new()
            .speaker_volume(0xff)
            .audio_output(AudioOutput::Speaker)
            .mic_mute(true)
            .finalize(true);
        assert_eq!(c[FLAG0], F0_SPEAKER_VOLUME | F0_AUDIO_CONTROL);
        assert_eq!(c[FLAG1], F1_POWER_SAVE_CONTROL);
        assert_eq!(c[SPEAKER_VOLUME], SPEAKER_VOLUME_MAX);
        assert_eq!(c[AUDIO_CONTROL], 0x30);
        assert_eq!(c[POWER_SAVE_CONTROL], 0x10);
    }

    #[test]
    fn triggers_go_to_their_own_slots() {
        let mut r = OutputReport::new();
        r.trigger(TriggerSide::Left, &TriggerEffect::Off).unwrap();
        r.trigger(
            TriggerSide::Right,
            &TriggerEffect::Feedback {
                position: 0,
                strength: 8,
            },
        )
        .unwrap();
        let c = r.finalize(true);
        assert_eq!(c[FLAG0], F0_LEFT_TRIGGER | F0_RIGHT_TRIGGER);
        assert_eq!(c[LEFT_TRIGGER], 0x05);
        assert_eq!(c[RIGHT_TRIGGER], 0x21);
    }
}
