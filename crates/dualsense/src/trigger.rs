//! Adaptive trigger effects.
//!
//! Each trigger takes an 11-byte block: a mode byte followed by 10 parameter
//! bytes. The trigger travel is split into 10 zones (0 = released, 9 = fully
//! pressed); most effects pack a 3-bit strength per zone plus a 10-bit mask of
//! active zones. Encodings follow Nielk1's TriggerEffectGenerator.

// The effect encodings are based on Nielk1's TriggerEffectGenerator
// (https://gist.github.com/Nielk1/6d54cc2c00d2201ccb8c2720ad7538db),
// used under the MIT license:
//
// Copyright (c) 2021-2022 John "Nielk1" Klein
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to
// deal in the Software without restriction, including without limitation the
// rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
// sell copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
// IN THE SOFTWARE.

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum TriggerSide {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(tag = "mode", rename_all = "snake_case")
)]
pub enum TriggerEffect {
    /// No resistance.
    Off,
    /// Constant resistance from `position` (0-9) to the end, `strength` 1-8.
    Feedback { position: u8, strength: u8 },
    /// Resistance between `start` (2-7) and `end` (start+1..=8) that gives way
    /// with a click, like a gun trigger. `strength` 1-8.
    Weapon { start: u8, end: u8, strength: u8 },
    /// Vibration from `position` (0-9) to the end. `amplitude` 1-8,
    /// `frequency` 1-255 Hz.
    Vibration {
        position: u8,
        amplitude: u8,
        frequency: u8,
    },
    /// Resistance between `start` (0-8) and `end` (start+1..=8) that snaps
    /// back with `snap` force once released, like drawing a bow. Both 1-8.
    Bow {
        start: u8,
        end: u8,
        strength: u8,
        snap: u8,
    },
    /// Two-step "hoof" pulse between `start` (0-8) and `end` (start+1..=9).
    /// `first_foot` 0-6 and `second_foot` (first_foot+1..=7) set the rhythm.
    Galloping {
        start: u8,
        end: u8,
        first_foot: u8,
        second_foot: u8,
        frequency: u8,
    },
    /// Vibration alternating between `amplitude_a` and `amplitude_b` (0-7)
    /// between `start` (1-8) and `end` (start+1..=9), every `period` tenths of a second.
    Machine {
        start: u8,
        end: u8,
        amplitude_a: u8,
        amplitude_b: u8,
        frequency: u8,
        period: u8,
    },
    /// Per-zone resistance, each 0 (none) to 8.
    MultiFeedback { strengths: [u8; 10] },
    /// Resistance ramping linearly from `start_strength` at `start` (0-8) to
    /// `end_strength` at `end` (start+1..=9). Strengths 1-8.
    Slope {
        start: u8,
        end: u8,
        start_strength: u8,
        end_strength: u8,
    },
}

const MODE_OFF: u8 = 0x05;
const MODE_FEEDBACK: u8 = 0x21;
const MODE_BOW: u8 = 0x22;
const MODE_GALLOPING: u8 = 0x23;
const MODE_WEAPON: u8 = 0x25;
const MODE_VIBRATION: u8 = 0x26;
const MODE_MACHINE: u8 = 0x27;

fn check(ok: bool, msg: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidParameter(msg.to_string()))
    }
}

/// Packs per-zone 1-8 strengths into the active-zone mask and 3-bit fields
/// shared by the feedback and vibration modes.
fn pack_zones(mode: u8, strengths: &[u8; 10]) -> [u8; 11] {
    let mut zones: u32 = 0;
    let mut active: u16 = 0;
    for (i, &s) in strengths.iter().enumerate() {
        if s > 0 {
            zones |= u32::from((s - 1) & 0x07) << (3 * i);
            active |= 1 << i;
        }
    }
    let mut out = [0u8; 11];
    out[0] = mode;
    out[1..3].copy_from_slice(&active.to_le_bytes());
    out[3..7].copy_from_slice(&zones.to_le_bytes());
    out
}

fn start_stop_zones(start: u8, end: u8) -> [u8; 2] {
    ((1u16 << start) | (1u16 << end)).to_le_bytes()
}

impl TriggerEffect {
    pub fn encode(&self) -> Result<[u8; 11]> {
        let mut out = [0u8; 11];
        match *self {
            TriggerEffect::Off => out[0] = MODE_OFF,

            TriggerEffect::Feedback { position, strength } => {
                check(position <= 9, "feedback position must be 0-9")?;
                check((1..=8).contains(&strength), "feedback strength must be 1-8")?;
                let mut strengths = [0u8; 10];
                strengths[position as usize..].fill(strength);
                out = pack_zones(MODE_FEEDBACK, &strengths);
            }

            TriggerEffect::Weapon {
                start,
                end,
                strength,
            } => {
                check((2..=7).contains(&start), "weapon start must be 2-7")?;
                check(end > start && end <= 8, "weapon end must be start+1..=8")?;
                check((1..=8).contains(&strength), "weapon strength must be 1-8")?;
                out[0] = MODE_WEAPON;
                out[1..3].copy_from_slice(&start_stop_zones(start, end));
                out[3] = strength - 1;
            }

            TriggerEffect::Vibration {
                position,
                amplitude,
                frequency,
            } => {
                check(position <= 9, "vibration position must be 0-9")?;
                check(
                    (1..=8).contains(&amplitude),
                    "vibration amplitude must be 1-8",
                )?;
                check(frequency > 0, "vibration frequency must be 1-255")?;
                let mut strengths = [0u8; 10];
                strengths[position as usize..].fill(amplitude);
                out = pack_zones(MODE_VIBRATION, &strengths);
                out[7] = 0;
                out[8] = 0;
                out[9] = frequency;
            }

            TriggerEffect::Bow {
                start,
                end,
                strength,
                snap,
            } => {
                check(start <= 8, "bow start must be 0-8")?;
                check(end > start && end <= 8, "bow end must be start+1..=8")?;
                check((1..=8).contains(&strength), "bow strength must be 1-8")?;
                check((1..=8).contains(&snap), "bow snap must be 1-8")?;
                let forces = u16::from((strength - 1) & 0x07) | (u16::from((snap - 1) & 0x07) << 3);
                out[0] = MODE_BOW;
                out[1..3].copy_from_slice(&start_stop_zones(start, end));
                out[3..5].copy_from_slice(&forces.to_le_bytes());
            }

            TriggerEffect::Galloping {
                start,
                end,
                first_foot,
                second_foot,
                frequency,
            } => {
                check(start <= 8, "galloping start must be 0-8")?;
                check(end > start && end <= 9, "galloping end must be start+1..=9")?;
                check(first_foot <= 6, "galloping first foot must be 0-6")?;
                check(
                    second_foot > first_foot && second_foot <= 7,
                    "galloping second foot must be first_foot+1..=7",
                )?;
                check(frequency > 0, "galloping frequency must be 1-255")?;
                out[0] = MODE_GALLOPING;
                out[1..3].copy_from_slice(&start_stop_zones(start, end));
                out[3] = (second_foot & 0x07) | ((first_foot & 0x07) << 3);
                out[4] = frequency;
            }

            TriggerEffect::Machine {
                start,
                end,
                amplitude_a,
                amplitude_b,
                frequency,
                period,
            } => {
                check((1..=8).contains(&start), "machine start must be 1-8")?;
                check(end > start && end <= 9, "machine end must be start+1..=9")?;
                check(
                    amplitude_a <= 7 && amplitude_b <= 7,
                    "machine amplitudes must be 0-7",
                )?;
                check(frequency > 0, "machine frequency must be 1-255")?;
                out[0] = MODE_MACHINE;
                out[1..3].copy_from_slice(&start_stop_zones(start, end));
                out[3] = (amplitude_a & 0x07) | ((amplitude_b & 0x07) << 3);
                out[4] = frequency;
                out[5] = period;
            }

            TriggerEffect::MultiFeedback { strengths } => {
                check(
                    strengths.iter().all(|&s| s <= 8),
                    "zone strengths must be 0-8",
                )?;
                if strengths.iter().all(|&s| s == 0) {
                    out[0] = MODE_OFF;
                } else {
                    out = pack_zones(MODE_FEEDBACK, &strengths);
                }
            }

            TriggerEffect::Slope {
                start,
                end,
                start_strength,
                end_strength,
            } => {
                check(start <= 8, "slope start must be 0-8")?;
                check(end > start && end <= 9, "slope end must be start+1..=9")?;
                check(
                    (1..=8).contains(&start_strength) && (1..=8).contains(&end_strength),
                    "slope strengths must be 1-8",
                )?;
                let slope =
                    (f32::from(end_strength) - f32::from(start_strength)) / f32::from(end - start);
                let mut strengths = [0u8; 10];
                for i in start..10 {
                    strengths[i as usize] = if i <= end {
                        (f32::from(start_strength) + slope * f32::from(i - start)).round() as u8
                    } else {
                        end_strength
                    };
                }
                out = pack_zones(MODE_FEEDBACK, &strengths);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_full_travel_max_strength() {
        let e = TriggerEffect::Feedback {
            position: 0,
            strength: 8,
        }
        .encode()
        .unwrap();
        // All 10 zones active, each with strength 7 (3 bits) -> 30 bits set.
        assert_eq!(e[0], MODE_FEEDBACK);
        assert_eq!(u16::from_le_bytes([e[1], e[2]]), 0x03ff);
        assert_eq!(u32::from_le_bytes([e[3], e[4], e[5], e[6]]), 0x3fff_ffff);
    }

    #[test]
    fn weapon_marks_start_and_end_zones() {
        let e = TriggerEffect::Weapon {
            start: 2,
            end: 6,
            strength: 8,
        }
        .encode()
        .unwrap();
        assert_eq!(&e[..4], &[MODE_WEAPON, 0b0100_0100, 0, 7]);
    }

    #[test]
    fn vibration_puts_frequency_in_byte_9() {
        let e = TriggerEffect::Vibration {
            position: 5,
            amplitude: 1,
            frequency: 40,
        }
        .encode()
        .unwrap();
        assert_eq!(e[0], MODE_VIBRATION);
        assert_eq!(u16::from_le_bytes([e[1], e[2]]), 0b11_1110_0000);
        assert_eq!(u32::from_le_bytes([e[3], e[4], e[5], e[6]]), 0);
        assert_eq!(e[9], 40);
    }

    #[test]
    fn slope_matches_multi_feedback() {
        let slope = TriggerEffect::Slope {
            start: 0,
            end: 9,
            start_strength: 1,
            end_strength: 8,
        };
        // round(1 + 7/9 * i)
        let strengths = [1, 2, 3, 3, 4, 5, 6, 6, 7, 8];
        let multi = TriggerEffect::MultiFeedback { strengths };
        assert_eq!(slope.encode().unwrap(), multi.encode().unwrap());
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(TriggerEffect::Feedback {
            position: 10,
            strength: 1
        }
        .encode()
        .is_err());
        assert!(TriggerEffect::Weapon {
            start: 1,
            end: 5,
            strength: 3
        }
        .encode()
        .is_err());
        assert!(TriggerEffect::Vibration {
            position: 0,
            amplitude: 3,
            frequency: 0
        }
        .encode()
        .is_err());
    }
}
