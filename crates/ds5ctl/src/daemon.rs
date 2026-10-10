//! `ds5ctl daemon`: gives every controller its saved profile when it connects.

use std::collections::{HashMap, HashSet};
use std::thread;
use std::time::{Duration, Instant};

use dualsense::{enumerate, DeviceInfo, DualSense, OutputReport};

use crate::config::{controller_key, Config};
use crate::profile::Profile;

const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// The kernel driver sets its own lightbar and player LEDs right after
/// connecting; wait until it is done so it does not undo the profile.
const SETTLE_TIME: Duration = Duration::from_secs(1);

pub fn run() -> ! {
    eprintln!("ds5ctl daemon: watching for DualSense controllers");
    let mut first_seen: HashMap<String, Instant> = HashMap::new();
    let mut applied: HashSet<String> = HashSet::new();
    loop {
        let devices = enumerate().unwrap_or_else(|e| {
            eprintln!("ds5ctl daemon: {e}");
            Vec::new()
        });
        let present: HashSet<&str> = devices.iter().map(|d| d.instance.as_str()).collect();
        first_seen.retain(|id, _| present.contains(id.as_str()));
        applied.retain(|id| present.contains(id.as_str()));

        for device in &devices {
            let since = *first_seen
                .entry(device.instance.clone())
                .or_insert_with(Instant::now);
            if since.elapsed() >= SETTLE_TIME && applied.insert(device.instance.clone()) {
                match apply(device) {
                    Ok(name) => eprintln!(
                        "ds5ctl daemon: applied the profile of \"{name}\" to {}",
                        describe(device)
                    ),
                    Err(e) => eprintln!("ds5ctl daemon: {}: {e}", describe(device)),
                }
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn describe(device: &DeviceInfo) -> String {
    format!(
        "{} ({}, {})",
        device.hidraw.display(),
        device.transport,
        device.mac.as_deref().unwrap_or("?")
    )
}

/// Sends a controller its whole saved profile and returns its name.
pub fn apply(device: &DeviceInfo) -> Result<String, String> {
    // Re-read every time so changes saved by the GUI are picked up.
    let config = Config::load()?;
    send_profile(device, config.profile_for(&controller_key(device)))?;
    Ok(config.name_of(device))
}

/// Opens the controller just long enough to send `profile`.
pub fn send_profile(device: &DeviceInfo, profile: &Profile) -> Result<(), String> {
    let mut ds = DualSense::open(device.clone()).map_err(|e| e.to_string())?;
    let mut report = OutputReport::new();
    profile
        .write_changes(None, &mut report)
        .map_err(|e| e.to_string())?;
    ds.send(&report).map_err(|e| e.to_string())
}
