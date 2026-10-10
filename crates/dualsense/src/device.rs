//! Finding controllers and talking to them through `/dev/hidraw*`.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::input::{Battery, ChargingState, InputState, INPUT_LEN};
use crate::motion::MotionCalibration;
use crate::output::{OutputReport, COMMON_LEN};
use crate::{Error, Result};

const VENDOR_SONY: u32 = 0x054c;
const PRODUCT_DUALSENSE: u32 = 0x0ce6;
const PRODUCT_DUALSENSE_EDGE: u32 = 0x0df2;

const BUS_USB: u16 = 0x03;
const BUS_BLUETOOTH: u16 = 0x05;

const OUTPUT_REPORT_USB: u8 = 0x02;
const OUTPUT_REPORT_USB_LEN: usize = 63;
const OUTPUT_REPORT_BT: u8 = 0x31;
const OUTPUT_REPORT_BT_LEN: usize = 78;
/// Magic tag every Bluetooth output report must carry.
const OUTPUT_TAG_BT: u8 = 0x10;

const INPUT_REPORT_USB: u8 = 0x01;
const INPUT_REPORT_BT: u8 = 0x31;

const FEATURE_CALIBRATION: u8 = 0x05;
const FEATURE_CALIBRATION_LEN: usize = 41;
const FEATURE_PAIRING_INFO: u8 = 0x09;
const FEATURE_PAIRING_INFO_LEN: usize = 20;
const FEATURE_FIRMWARE_INFO: u8 = 0x20;
const FEATURE_FIRMWARE_INFO_LEN: usize = 64;

// Bluetooth reports end in a CRC32 over a one-byte HID header plus the data.
const CRC_SEED_INPUT: u8 = 0xa1;
const CRC_SEED_OUTPUT: u8 = 0xa2;
const CRC_SEED_FEATURE: u8 = 0xa3;

/// Firmware that understands the newer rumble flag (see hid-playstation).
const VIBRATION_V2_MIN_UPDATE_VERSION: u16 = 0x0215;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Usb,
    Bluetooth,
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Transport::Usb => "USB",
            Transport::Bluetooth => "Bluetooth",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    DualSense,
    DualSenseEdge,
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Model::DualSense => "DualSense",
            Model::DualSenseEdge => "DualSense Edge",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub hidraw: PathBuf,
    /// Kernel HID device name, e.g. `0005:054C:0CE6.0006`. The suffix grows on
    /// every (re)connection, so this tells a reconnected controller apart.
    pub instance: String,
    pub model: Model,
    pub transport: Transport,
    /// Controller MAC address as the kernel reports it, lowercase.
    pub mac: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareInfo {
    pub build_date: String,
    pub build_time: String,
    pub hardware_version: u32,
    pub firmware_version: u32,
    pub update_version: u16,
}

impl DeviceInfo {
    /// Battery state as the kernel driver last saw it. Works without opening
    /// the controller, so it is cheap to show for every connected one.
    pub fn sysfs_battery(&self) -> Option<Battery> {
        let dir = Path::new("/sys/class/power_supply")
            .join(format!("ps-controller-battery-{}", self.mac.as_deref()?));
        let percent = fs::read_to_string(dir.join("capacity"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        let state = match fs::read_to_string(dir.join("status")).ok()?.trim() {
            "Charging" => ChargingState::Charging,
            "Full" => ChargingState::Full,
            "Discharging" => ChargingState::Discharging,
            _ => ChargingState::Error,
        };
        Some(Battery { percent, state })
    }

    /// ALSA card number of the controller's own sound card (speaker,
    /// headphone jack, microphone). Only exists over USB.
    pub fn sound_card(&self) -> Option<u32> {
        if self.transport != Transport::Usb {
            return None;
        }
        let hid = fs::canonicalize(
            Path::new("/sys/class/hidraw")
                .join(self.hidraw.file_name()?)
                .join("device"),
        )
        .ok()?;
        // .../1-2/1-2:1.3/0003:054C:0CE6.000A: the USB device is two levels up,
        // and its audio interface (1-2:1.0) owns the sound card.
        let usb_device = hid.parent()?.parent()?;
        fs::read_dir("/sys/class/sound")
            .ok()?
            .flatten()
            .find_map(|entry| {
                let number = entry
                    .file_name()
                    .to_str()?
                    .strip_prefix("card")?
                    .parse()
                    .ok()?;
                let interface = fs::canonicalize(entry.path().join("device")).ok()?;
                (interface.parent() == Some(usb_device)).then_some(number)
            })
    }
}

/// Lists connected DualSense controllers, ordered by hidraw number.
pub fn enumerate() -> Result<Vec<DeviceInfo>> {
    let dir = match fs::read_dir("/sys/class/hidraw") {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };

    let mut devices = Vec::new();
    for entry in dir {
        let entry = entry?;
        let device = entry.path().join("device");
        let Ok(uevent) = fs::read_to_string(device.join("uevent")) else {
            continue;
        };
        let instance = fs::read_link(&device)
            .ok()
            .and_then(|target| target.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        if let Some(info) =
            parse_uevent(&uevent, Path::new("/dev").join(entry.file_name()), instance)
        {
            devices.push(info);
        }
    }
    devices.sort_by_key(|d| hidraw_number(&d.hidraw));
    Ok(devices)
}

fn hidraw_number(path: &Path) -> u32 {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("hidraw"))
        .and_then(|n| n.parse().ok())
        .unwrap_or(u32::MAX)
}

fn parse_uevent(uevent: &str, hidraw: PathBuf, instance: String) -> Option<DeviceInfo> {
    let mut hid_id = None;
    let mut mac = None;
    for line in uevent.lines() {
        if let Some(v) = line.strip_prefix("HID_ID=") {
            hid_id = Some(v);
        } else if let Some(v) = line.strip_prefix("HID_UNIQ=") {
            if !v.is_empty() {
                mac = Some(v.to_lowercase());
            }
        }
    }

    // HID_ID=BBBB:VVVVVVVV:PPPPPPPP
    let mut parts = hid_id?.split(':');
    let bus = u16::from_str_radix(parts.next()?, 16).ok()?;
    let vendor = u32::from_str_radix(parts.next()?, 16).ok()?;
    let product = u32::from_str_radix(parts.next()?, 16).ok()?;
    if vendor != VENDOR_SONY {
        return None;
    }
    let model = match product {
        PRODUCT_DUALSENSE => Model::DualSense,
        PRODUCT_DUALSENSE_EDGE => Model::DualSenseEdge,
        _ => return None,
    };
    let transport = match bus {
        BUS_USB => Transport::Usb,
        BUS_BLUETOOTH => Transport::Bluetooth,
        _ => return None,
    };
    Some(DeviceInfo {
        hidraw,
        instance,
        model,
        transport,
        mac,
    })
}

fn bt_crc(seed: u8, data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&[seed]);
    hasher.update(data);
    hasher.finalize()
}

/// `_IOC(_IOC_READ | _IOC_WRITE, 'H', 0x07, len)`
fn hidiocgfeature(len: usize) -> u64 {
    (3 << 30) | ((len as u64) << 16) | ((b'H' as u64) << 8) | 0x07
}

pub struct DualSense {
    file: File,
    info: DeviceInfo,
    output_seq: u8,
    vibration_v2: bool,
}

impl DualSense {
    pub fn open(info: DeviceInfo) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&info.hidraw)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::PermissionDenied {
                    Error::PermissionDenied(info.hidraw.clone())
                } else {
                    Error::Io(e)
                }
            })?;
        let mut ds = DualSense {
            file,
            info,
            output_seq: 0,
            vibration_v2: true,
        };
        if let Ok(fw) = ds.firmware_info() {
            ds.vibration_v2 = fw.update_version >= VIBRATION_V2_MIN_UPDATE_VERSION;
        }
        Ok(ds)
    }

    /// Opens the first controller found.
    pub fn open_first() -> Result<Self> {
        let info = enumerate()?.into_iter().next().ok_or(Error::NoDevice)?;
        Self::open(info)
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    pub fn send(&mut self, report: &OutputReport) -> Result<()> {
        let common = report.finalize(self.vibration_v2);
        match self.info.transport {
            Transport::Usb => {
                let mut buf = [0u8; OUTPUT_REPORT_USB_LEN];
                buf[0] = OUTPUT_REPORT_USB;
                buf[1..1 + COMMON_LEN].copy_from_slice(&common);
                self.file.write_all(&buf)?;
            }
            Transport::Bluetooth => {
                let mut buf = [0u8; OUTPUT_REPORT_BT_LEN];
                buf[0] = OUTPUT_REPORT_BT;
                buf[1] = self.output_seq << 4;
                buf[2] = OUTPUT_TAG_BT;
                buf[3..3 + COMMON_LEN].copy_from_slice(&common);
                let crc = bt_crc(CRC_SEED_OUTPUT, &buf[..OUTPUT_REPORT_BT_LEN - 4]);
                buf[OUTPUT_REPORT_BT_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
                self.file.write_all(&buf)?;
                self.output_seq = (self.output_seq + 1) % 16;
            }
        }
        Ok(())
    }

    /// Reads feature report `id`. `len` includes the report ID byte.
    pub fn feature_report(&self, id: u8, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        buf[0] = id;
        // SAFETY: HIDIOCGFEATURE writes at most `len` bytes into `buf`.
        let n = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                hidiocgfeature(len) as _,
                buf.as_mut_ptr(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error().into());
        }
        buf.truncate(n as usize);
        if buf.first() != Some(&id) {
            return Err(Error::BadReport(format!(
                "feature report 0x{id:02x} came back with another ID"
            )));
        }
        if self.info.transport == Transport::Bluetooth && buf.len() == len {
            let (data, crc) = buf.split_at(len - 4);
            if bt_crc(CRC_SEED_FEATURE, data).to_le_bytes() != crc {
                return Err(Error::BadReport(format!(
                    "feature report 0x{id:02x} failed CRC check"
                )));
            }
        }
        Ok(buf)
    }

    pub fn firmware_info(&self) -> Result<FirmwareInfo> {
        let b = self.feature_report(FEATURE_FIRMWARE_INFO, FEATURE_FIRMWARE_INFO_LEN)?;
        if b.len() < 46 {
            return Err(Error::BadReport("firmware info too short".into()));
        }
        let text = |range: std::ops::Range<usize>| {
            String::from_utf8_lossy(&b[range])
                .trim_end_matches('\0')
                .trim()
                .to_string()
        };
        let u32_at = |off: usize| u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]);
        Ok(FirmwareInfo {
            build_date: text(1..12),
            build_time: text(12..20),
            hardware_version: u32_at(24),
            firmware_version: u32_at(28),
            update_version: u16::from_le_bytes([b[44], b[45]]),
        })
    }

    /// The controller's own Bluetooth MAC address, read from the device.
    /// Factory calibration of the gyroscope and accelerometer.
    pub fn motion_calibration(&self) -> Result<MotionCalibration> {
        let b = self.feature_report(FEATURE_CALIBRATION, FEATURE_CALIBRATION_LEN)?;
        MotionCalibration::parse(&b)
    }

    pub fn mac_address(&self) -> Result<String> {
        let b = self.feature_report(FEATURE_PAIRING_INFO, FEATURE_PAIRING_INFO_LEN)?;
        if b.len() < 7 {
            return Err(Error::BadReport("pairing info too short".into()));
        }
        let octets: Vec<String> = b[1..7].iter().rev().map(|o| format!("{o:02x}")).collect();
        Ok(octets.join(":"))
    }

    /// Waits for the next full input report.
    pub fn read_input(&mut self, timeout: Duration) -> Result<InputState> {
        let deadline = Instant::now() + timeout;
        let mut asked_for_full_reports = false;
        let mut buf = [0u8; 128];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }
            let mut pfd = libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
            // SAFETY: one valid pollfd.
            let ready = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if ready < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e.into());
            }
            if ready == 0 {
                return Err(Error::Timeout);
            }

            let n = self.file.read(&mut buf)?;
            let report = &buf[..n];
            match (self.info.transport, report.first().copied()) {
                (Transport::Usb, Some(INPUT_REPORT_USB)) if n > INPUT_LEN => {
                    return InputState::parse(&report[1..]);
                }
                (Transport::Bluetooth, Some(INPUT_REPORT_BT)) if n >= 2 + INPUT_LEN + 4 => {
                    let (data, crc) = report.split_at(n - 4);
                    if bt_crc(CRC_SEED_INPUT, data).to_le_bytes() == crc {
                        return InputState::parse(&report[2..]);
                    }
                }
                // Until calibration is read, Bluetooth sends a reduced 0x01
                // report; reading it switches the controller to full reports.
                (Transport::Bluetooth, Some(INPUT_REPORT_USB)) if !asked_for_full_reports => {
                    self.feature_report(FEATURE_CALIBRATION, FEATURE_CALIBRATION_LEN)?;
                    asked_for_full_reports = true;
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bluetooth_uevent() {
        let uevent = "DRIVER=playstation\nHID_ID=0005:0000054C:00000CE6\n\
                      HID_NAME=DualSense Wireless Controller\nHID_UNIQ=A0:FA:9C:0A:2C:77\n";
        let info = parse_uevent(uevent, "/dev/hidraw4".into(), String::new()).unwrap();
        assert_eq!(info.model, Model::DualSense);
        assert_eq!(info.transport, Transport::Bluetooth);
        assert_eq!(info.mac.as_deref(), Some("a0:fa:9c:0a:2c:77"));
    }

    #[test]
    fn ignores_other_devices() {
        let uevent = "HID_ID=0003:0000046D:0000C542\nHID_NAME=Logitech\n";
        assert!(parse_uevent(uevent, "/dev/hidraw0".into(), String::new()).is_none());
    }

    #[test]
    fn ioctl_number_matches_kernel_macro() {
        // HIDIOCGFEATURE(64) from <linux/hidraw.h>
        assert_eq!(hidiocgfeature(64), 0xc040_4807);
    }
}
