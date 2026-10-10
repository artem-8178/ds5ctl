//! The full input report (USB 0x01, Bluetooth 0x31).

use crate::{Error, Result};

/// Bytes of the common input block that we parse.
pub(crate) const INPUT_LEN: usize = 54;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargingState {
    Discharging,
    Charging,
    Full,
    /// Voltage/temperature out of range or a charging fault.
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub state: ChargingState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons {
    pub cross: bool,
    pub circle: bool,
    pub square: bool,
    pub triangle: bool,
    pub l1: bool,
    pub r1: bool,
    pub l2: bool,
    pub r2: bool,
    pub create: bool,
    pub options: bool,
    pub l3: bool,
    pub r3: bool,
    pub ps: bool,
    pub touchpad: bool,
    pub mute: bool,
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
}

impl Buttons {
    fn parse(b: &[u8]) -> Self {
        let bit = |byte: usize, n: u8| b[byte] & (1 << n) != 0;
        // D-pad is a hat switch: 0 = up, clockwise, 8 = released.
        let hat = b[0] & 0x0f;
        Buttons {
            up: matches!(hat, 7 | 0 | 1),
            right: matches!(hat, 1..=3),
            down: matches!(hat, 3..=5),
            left: matches!(hat, 5..=7),
            square: bit(0, 4),
            cross: bit(0, 5),
            circle: bit(0, 6),
            triangle: bit(0, 7),
            l1: bit(1, 0),
            r1: bit(1, 1),
            l2: bit(1, 2),
            r2: bit(1, 3),
            create: bit(1, 4),
            options: bit(1, 5),
            l3: bit(1, 6),
            r3: bit(1, 7),
            ps: bit(2, 0),
            touchpad: bit(2, 1),
            mute: bit(2, 2),
        }
    }

    /// Names of the buttons currently held, in a stable order.
    pub fn pressed(&self) -> Vec<&'static str> {
        [
            (self.up, "up"),
            (self.down, "down"),
            (self.left, "left"),
            (self.right, "right"),
            (self.cross, "cross"),
            (self.circle, "circle"),
            (self.square, "square"),
            (self.triangle, "triangle"),
            (self.l1, "l1"),
            (self.r1, "r1"),
            (self.l2, "l2"),
            (self.r2, "r2"),
            (self.l3, "l3"),
            (self.r3, "r3"),
            (self.create, "create"),
            (self.options, "options"),
            (self.ps, "ps"),
            (self.touchpad, "touchpad"),
            (self.mute, "mute"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputState {
    /// Sticks as (x, y), 0-255 with 128 centred; y grows downwards.
    pub left_stick: (u8, u8),
    pub right_stick: (u8, u8),
    /// Analog trigger travel, 0-255.
    pub l2: u8,
    pub r2: u8,
    pub buttons: Buttons,
    /// Raw, uncalibrated gyroscope and accelerometer readings (x, y, z).
    pub gyro: [i16; 3],
    pub accel: [i16; 3],
    /// Sensor clock in units of 1/3 µs; wraps around.
    pub sensor_timestamp: u32,
    /// Up to two touchpad contacts, (x, y) in 0..1920 x 0..1080.
    pub touch: [Option<(u16, u16)>; 2],
    pub battery: Battery,
    pub headphones_plugged: bool,
    pub mic_plugged: bool,
}

fn i16_at(b: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([b[off], b[off + 1]])
}

impl InputState {
    /// `b` is the common block that follows the report header.
    pub(crate) fn parse(b: &[u8]) -> Result<Self> {
        if b.len() < INPUT_LEN {
            return Err(Error::BadReport(format!(
                "input report too short ({} bytes)",
                b.len()
            )));
        }

        let touch_point = |off: usize| {
            // Bit 7 of the contact byte set means "no finger".
            if b[off] & 0x80 != 0 {
                return None;
            }
            let x = u16::from(b[off + 1]) | (u16::from(b[off + 2] & 0x0f) << 8);
            let y = u16::from(b[off + 2] >> 4) | (u16::from(b[off + 3]) << 4);
            Some((x, y))
        };

        let status = b[52];
        let level = status & 0x0f;
        let battery = match status >> 4 {
            0x0 => Battery {
                percent: (level * 10 + 5).min(100),
                state: ChargingState::Discharging,
            },
            0x1 => Battery {
                percent: (level * 10 + 5).min(100),
                state: ChargingState::Charging,
            },
            0x2 => Battery {
                percent: 100,
                state: ChargingState::Full,
            },
            _ => Battery {
                percent: 0,
                state: ChargingState::Error,
            },
        };

        Ok(InputState {
            left_stick: (b[0], b[1]),
            right_stick: (b[2], b[3]),
            l2: b[4],
            r2: b[5],
            buttons: Buttons::parse(&b[7..10]),
            gyro: [i16_at(b, 15), i16_at(b, 17), i16_at(b, 19)],
            accel: [i16_at(b, 21), i16_at(b, 23), i16_at(b, 25)],
            sensor_timestamp: u32::from_le_bytes([b[27], b[28], b[29], b[30]]),
            touch: [touch_point(32), touch_point(36)],
            battery,
            headphones_plugged: b[53] & 0x01 != 0,
            mic_plugged: b[53] & 0x02 != 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_idle_report() {
        let mut b = [0u8; INPUT_LEN];
        b[..4].fill(128);
        b[7] = 0x08; // d-pad released
        b[32] = 0x80; // no touches
        b[36] = 0x80;
        b[52] = 0x07; // discharging, level 7
        let s = InputState::parse(&b).unwrap();
        assert_eq!(s.left_stick, (128, 128));
        assert!(s.buttons.pressed().is_empty());
        assert_eq!(s.touch, [None, None]);
        assert_eq!(
            s.battery,
            Battery {
                percent: 75,
                state: ChargingState::Discharging
            }
        );
    }

    #[test]
    fn parses_buttons_and_touch() {
        let mut b = [0u8; INPUT_LEN];
        b[7] = 0x20 | 0x01; // cross + d-pad up-right
        b[9] = 0x04; // mute
        b[32] = 0x01; // finger down at (0x234, 0x567)
        b[33] = 0x34;
        b[34] = 0x72;
        b[35] = 0x56;
        b[36] = 0x80;
        let s = InputState::parse(&b).unwrap();
        assert_eq!(s.buttons.pressed(), ["up", "right", "cross", "mute"]);
        assert_eq!(s.touch[0], Some((0x234, 0x567)));
    }
}
