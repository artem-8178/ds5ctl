//! DualSense (PS5) controller protocol over Linux hidraw.
//!
//! The kernel `hid-playstation` driver only exposes the lightbar, player LEDs,
//! rumble and battery. Everything else the controller supports (adaptive
//! triggers, mic LED, audio, motor power, ...) is reachable only through raw
//! HID reports, which is what this crate builds and sends.
//!
//! Report layouts follow `drivers/hid/hid-playstation.c` and the community
//! reverse-engineering notes (Nielk1, dualsensectl).

mod device;
mod input;
mod motion;
mod output;
mod trigger;

use std::{fmt, io, path::PathBuf};

pub use device::{enumerate, DeviceInfo, DualSense, FirmwareInfo, Model, Transport};
pub use input::{Battery, Buttons, ChargingState, InputState};
pub use motion::{MotionCalibration, MotionReading, MotionTracker, Quat};
pub use output::{
    AudioOutput, LightbarFade, MicLed, OutputReport, PlayerLedBrightness, HEADPHONE_VOLUME_MAX,
    MIC_VOLUME_MAX, MOTOR_POWER_REDUCTION_MAX, PLAYER_PATTERNS, SPEAKER_VOLUME_MAX,
};
pub use trigger::{TriggerEffect, TriggerSide};

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The hidraw node exists but the current user may not open it.
    PermissionDenied(PathBuf),
    NoDevice,
    InvalidParameter(String),
    BadReport(String),
    Timeout,
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::PermissionDenied(path) => write!(f, "no permission to open {}", path.display()),
            Error::NoDevice => write!(f, "no DualSense controller found"),
            Error::InvalidParameter(msg) => write!(f, "invalid parameter: {msg}"),
            Error::BadReport(msg) => write!(f, "unexpected report from controller: {msg}"),
            Error::Timeout => write!(f, "timed out waiting for the controller"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}
