//! Background thread that talks to the controllers. The selected one stays
//! open: its input is read continuously and profile edits are sent as they
//! happen. Every other controller gets its saved profile once it connects.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dualsense::{
    enumerate, Battery, DeviceInfo, DualSense, Error, FirmwareInfo, InputState, MotionReading,
    MotionTracker, OutputReport, Transport,
};

use crate::bluez;
use crate::config::{controller_key, Config};
use crate::daemon::send_profile;
use crate::profile::Profile;

const SCAN_INTERVAL: Duration = Duration::from_secs(1);
const INPUT_TIMEOUT: Duration = Duration::from_millis(30);
/// Repaint rate while the UI shows live input.
const LIVE_FRAME: Duration = Duration::from_millis(33);
/// The kernel resets the LEDs right after a controller connects; profiles
/// sent before that would be undone.
const SETTLE_TIME: Duration = Duration::from_millis(1500);

pub enum Request {
    Config(Box<Config>),
    /// The controller to keep open, by [`controller_key`].
    Select(Option<String>),
    Rumble {
        left: u8,
        right: u8,
        duration: Duration,
    },
    /// Flash and rumble a controller so it can be found among several.
    Identify(String),
    PowerOff,
    /// Make the controller's current heading the 3D view's "straight ahead".
    Recenter,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum Connection {
    /// Nothing selected yet.
    #[default]
    Searching,
    /// The selected controller is not connected.
    Offline,
    PermissionDenied(PathBuf),
    Connected(DeviceInfo),
}

/// What the UI renders; replaced by the worker as things change.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Set once the first device scan finished.
    pub scanned: bool,
    pub devices: Vec<DeviceInfo>,
    /// Kernel battery readings for every connected controller, by key.
    pub batteries: HashMap<String, Battery>,
    pub connection: Connection,
    pub firmware: Option<FirmwareInfo>,
    pub mac: Option<String>,
    pub input: Option<InputState>,
    /// Calibrated motion sensors and the estimated orientation.
    pub motion: Option<MotionReading>,
    pub error: Option<String>,
}

pub struct Handle {
    pub snapshot: Arc<Mutex<Snapshot>>,
    /// Set by the UI while a page with live input is visible.
    pub live: Arc<AtomicBool>,
}

pub fn spawn(config: Config, requests: Receiver<Request>, ctx: eframe::egui::Context) -> Handle {
    let snapshot = Arc::new(Mutex::new(Snapshot::default()));
    let live = Arc::new(AtomicBool::new(false));
    let mut worker = Worker {
        snapshot: snapshot.clone(),
        live: live.clone(),
        ctx,
        config,
        selected: None,
        device: None,
        applied: None,
        first_seen: HashMap::new(),
        handled: HashSet::new(),
        initial_devices: None,
        reapply_at: None,
        rumble_stop: None,
        last_scan: None,
        last_repaint: Instant::now(),
        motion: None,
    };
    thread::Builder::new()
        .name("ds5ctl-device".into())
        .spawn(move || worker.run(requests))
        .expect("failed to start the device thread");
    Handle { snapshot, live }
}

struct Worker {
    snapshot: Arc<Mutex<Snapshot>>,
    live: Arc<AtomicBool>,
    ctx: eframe::egui::Context,
    config: Config,
    selected: Option<String>,
    device: Option<DualSense>,
    /// What the open controller currently has; `None` forces a full re-send.
    applied: Option<Profile>,
    /// When each connection (by kernel instance) first showed up.
    first_seen: HashMap<String, Instant>,
    /// Connections that already got their profile while not selected.
    handled: HashSet<String>,
    /// Connections that existed when the app started.
    initial_devices: Option<HashSet<String>>,
    reapply_at: Option<Instant>,
    rumble_stop: Option<Instant>,
    last_scan: Option<Instant>,
    last_repaint: Instant,
    /// Orientation filter for the open controller.
    motion: Option<MotionTracker>,
}

impl Worker {
    fn run(&mut self, requests: Receiver<Request>) {
        loop {
            if self.device.is_some() {
                loop {
                    match requests.try_recv() {
                        Ok(request) => self.handle(request),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
            } else {
                // Nothing to read, so block on requests between scans.
                match requests.recv_timeout(Duration::from_millis(250)) {
                    Ok(request) => self.handle(request),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }

            if self.last_scan.is_none_or(|t| t.elapsed() >= SCAN_INTERVAL) {
                self.scan();
            }
            if self.device.is_some() {
                self.sync_outputs();
                self.read_input();
            }
        }
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Config(config) => self.config = *config,
            Request::Select(key) => {
                if key != self.selected {
                    self.selected = key;
                    self.close();
                    self.last_scan = None;
                }
            }
            Request::Rumble {
                left,
                right,
                duration,
            } => {
                let mut report = OutputReport::new();
                report.rumble(left, right);
                if self.send(&report) {
                    self.rumble_stop = Some(Instant::now() + duration);
                }
            }
            Request::Identify(key) => {
                let device = self
                    .snapshot
                    .lock()
                    .unwrap()
                    .devices
                    .iter()
                    .find(|d| controller_key(d) == key)
                    .cloned();
                if let Some(device) = device {
                    identify(device, self.config.profile_for(&key).clone());
                }
            }
            Request::Recenter => {
                if let Some(motion) = &mut self.motion {
                    motion.recenter();
                }
            }
            Request::PowerOff => {
                let info = self.device.as_ref().map(|d| d.info().clone());
                let result = match info {
                    Some(info) if info.transport == Transport::Bluetooth => match info.mac {
                        Some(mac) => bluez::disconnect(&mac),
                        None => Err("controller MAC address is unknown".into()),
                    },
                    Some(_) => Err("only Bluetooth controllers can be switched off".into()),
                    None => Err("no controller connected".into()),
                };
                if let Err(e) = result {
                    self.set_error(Some(e));
                }
            }
        }
    }

    fn scan(&mut self) {
        let now = Instant::now();
        self.last_scan = Some(now);
        let devices = enumerate().unwrap_or_default();
        let initial = self
            .initial_devices
            .get_or_insert_with(|| devices.iter().map(|d| d.instance.clone()).collect())
            .clone();

        if let Some(current) = self.device.as_ref().map(|d| d.info().instance.clone()) {
            if !devices.iter().any(|d| d.instance == current) {
                self.close();
            }
        }
        if self.device.is_none() {
            match &self.selected {
                Some(key) => match devices.iter().find(|d| &controller_key(d) == key).cloned() {
                    Some(info) => self.open(info, &initial),
                    None => self.set_connection(Connection::Offline),
                },
                None => self.set_connection(Connection::Searching),
            }
        }

        // Give controllers that are not selected their own profile once.
        let open_instance = self.device.as_ref().map(|d| d.info().instance.clone());
        for device in &devices {
            let since = *self
                .first_seen
                .entry(device.instance.clone())
                .or_insert_with(|| {
                    // Ones that were already there need no settling time.
                    if initial.contains(&device.instance) {
                        now - SETTLE_TIME
                    } else {
                        now
                    }
                });
            if open_instance.as_ref() == Some(&device.instance) || since.elapsed() < SETTLE_TIME {
                continue;
            }
            if self.handled.insert(device.instance.clone()) {
                let profile = self.config.profile_for(&controller_key(device));
                if let Err(e) = send_profile(device, profile) {
                    self.set_error(Some(format!("{}: {e}", self.config.name_of(device))));
                }
            }
        }
        let present: HashSet<&str> = devices.iter().map(|d| d.instance.as_str()).collect();
        self.first_seen
            .retain(|id, _| present.contains(id.as_str()));
        self.handled.retain(|id| present.contains(id.as_str()));

        let batteries: HashMap<String, Battery> = devices
            .iter()
            .filter_map(|d| Some((controller_key(d), d.sysfs_battery()?)))
            .collect();
        let changed = {
            let mut snap = self.snapshot.lock().unwrap();
            let changed = snap.devices != devices || snap.batteries != batteries || !snap.scanned;
            snap.devices = devices;
            snap.batteries = batteries;
            snap.scanned = true;
            changed
        };
        if changed {
            self.ctx.request_repaint();
        }
    }

    fn open(&mut self, info: DeviceInfo, initial: &HashSet<String>) {
        match DualSense::open(info.clone()) {
            Ok(ds) => {
                let firmware = ds.firmware_info().ok();
                let calibration = ds.motion_calibration().unwrap_or_default();
                self.motion = Some(MotionTracker::new(calibration));
                let mac = ds.mac_address().ok().or_else(|| info.mac.clone());
                if !initial.contains(&info.instance) {
                    self.reapply_at = Some(Instant::now() + SETTLE_TIME);
                }
                self.device = Some(ds);
                self.applied = None;
                self.update(|snap| {
                    snap.connection = Connection::Connected(info);
                    snap.firmware = firmware;
                    snap.mac = mac;
                    snap.error = None;
                });
            }
            Err(Error::PermissionDenied(path)) => {
                self.set_connection(Connection::PermissionDenied(path))
            }
            Err(e) => {
                self.set_connection(Connection::Offline);
                self.set_error(Some(format!("{}: {e}", info.hidraw.display())));
            }
        }
    }

    fn close(&mut self) {
        self.device = None;
        self.applied = None;
        self.reapply_at = None;
        self.rumble_stop = None;
        self.motion = None;
    }

    fn set_connection(&self, connection: Connection) {
        let unchanged = self.snapshot.lock().unwrap().connection == connection;
        if !unchanged {
            self.update(|snap| {
                snap.connection = connection;
                snap.firmware = None;
                snap.mac = None;
                snap.input = None;
                snap.motion = None;
            });
        }
    }

    fn sync_outputs(&mut self) {
        let now = Instant::now();
        if self.reapply_at.is_some_and(|t| now >= t) {
            self.reapply_at = None;
            self.applied = None;
        }
        if self.rumble_stop.is_some_and(|t| now >= t) {
            self.rumble_stop = None;
            let mut report = OutputReport::new();
            report.rumble(0, 0);
            self.send(&report);
        }

        let Some(key) = &self.selected else {
            return;
        };
        let profile = self.config.profile_for(key).clone();
        if self.applied.as_ref() == Some(&profile) {
            return;
        }
        let mut report = OutputReport::new();
        match profile.write_changes(self.applied.as_ref(), &mut report) {
            Ok(true) => {
                if self.send(&report) {
                    self.applied = Some(profile);
                }
            }
            Ok(false) => self.applied = Some(profile),
            Err(e) => {
                // Remember it anyway so an invalid value is not retried forever.
                self.applied = Some(profile);
                self.set_error(Some(e.to_string()));
            }
        }
    }

    /// Returns false (and closes the device) when the controller went away.
    fn send(&mut self, report: &OutputReport) -> bool {
        let Some(ds) = self.device.as_mut() else {
            return false;
        };
        match ds.send(report) {
            Ok(()) => true,
            Err(e) => {
                self.close();
                self.set_connection(Connection::Offline);
                self.set_error(Some(format!("controller disconnected: {e}")));
                false
            }
        }
    }

    fn read_input(&mut self) {
        let Some(ds) = self.device.as_mut() else {
            return;
        };
        match ds.read_input(INPUT_TIMEOUT) {
            Ok(state) => {
                // The filter needs every report (~800 per second over
                // Bluetooth), not just the ones the UI paints.
                let motion = self.motion.as_mut().map(|tracker| {
                    tracker.update(&state);
                    tracker.reading()
                });
                let battery_changed = {
                    let mut snap = self.snapshot.lock().unwrap();
                    let changed = snap.input.as_ref().map(|i| i.battery) != Some(state.battery);
                    snap.input = Some(state);
                    snap.motion = motion;
                    changed
                };
                let live = self.live.load(Ordering::Relaxed);
                if battery_changed || (live && self.last_repaint.elapsed() >= LIVE_FRAME) {
                    self.last_repaint = Instant::now();
                    self.ctx.request_repaint();
                }
            }
            Err(Error::Timeout) => {}
            Err(_) => {
                self.close();
                self.set_connection(Connection::Offline);
            }
        }
    }

    fn set_error(&self, error: Option<String>) {
        self.update(|snap| snap.error = error);
    }

    fn update(&self, f: impl FnOnce(&mut Snapshot)) {
        f(&mut self.snapshot.lock().unwrap());
        self.ctx.request_repaint();
    }
}

/// Blinks the lightbar white with a short rumble, then restores `profile`.
/// Runs on its own thread with its own handle, so any controller can be
/// identified, open or not.
fn identify(device: DeviceInfo, profile: Profile) {
    thread::spawn(move || {
        let Ok(mut ds) = DualSense::open(device) else {
            return;
        };
        for step in 0..4 {
            let mut report = OutputReport::new();
            if step % 2 == 0 {
                report
                    .lightbar(255, 255, 255)
                    .player_leds(0b11111, true)
                    .rumble(90, 140);
            } else {
                report.lightbar(0, 0, 0).player_leds(0, true).rumble(0, 0);
            }
            if ds.send(&report).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(220));
        }
        let mut report = OutputReport::new();
        report.rumble(0, 0);
        if profile.write_changes(None, &mut report).is_ok() {
            let _ = ds.send(&report);
        }
    });
}
