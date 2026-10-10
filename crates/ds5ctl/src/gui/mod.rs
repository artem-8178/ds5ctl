//! The settings window. Edits go to the selected controller immediately and
//! are saved shortly after; every controller keeps its own profile.

mod model3d;
mod screenshot;
mod widgets;
mod worker;

use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, color_picker, Color32, RichText};

use dualsense::{
    AudioOutput, Buttons, ChargingState, DeviceInfo, MicLed, PlayerLedBrightness, Transport,
    TriggerEffect, HEADPHONE_VOLUME_MAX, MIC_VOLUME_MAX, PLAYER_PATTERNS, SPEAKER_VOLUME_MAX,
};

use crate::config::{controller_key, default_name, Config};
use crate::profile::Profile;
use crate::{sound, udev};
use worker::{Connection, Handle, Request, Snapshot};

const SAVE_DELAY: Duration = Duration::from_millis(700);
/// How long the window waits for the controller before its first frame, so
/// it does not flash "waiting for a controller" when one is connected.
const STARTUP_WAIT: Duration = Duration::from_millis(800);

/// `controller` preselects one by name, MAC address or hidraw path.
pub fn run(start_page: Option<Page>, controller: Option<String>) -> Result<(), String> {
    let (config, notice) = match Config::load() {
        Ok(config) => (config, None),
        Err(e) => (
            Config::default(),
            Some(format!(
                "Could not read the saved settings ({e}); using defaults."
            )),
        ),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ds5ctl")
            .with_app_id("ds5ctl")
            .with_inner_size([920.0, 640.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ds5ctl",
        options,
        Box::new(move |cc| {
            Ok(Box::new(App::new(
                cc,
                config,
                notice,
                start_page.unwrap_or(Page::Lighting),
                controller,
            )))
        }),
    )
    .map_err(|e| format!("cannot open the window: {e}"))
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Page {
    Lighting,
    Triggers,
    Vibration,
    Audio,
    Input,
    Motion,
    Info,
}

impl Page {
    const ALL: [Page; 7] = [
        Page::Lighting,
        Page::Triggers,
        Page::Vibration,
        Page::Audio,
        Page::Input,
        Page::Motion,
        Page::Info,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::Lighting => "Lighting",
            Page::Triggers => "Adaptive triggers",
            Page::Vibration => "Vibration",
            Page::Audio => "Audio",
            Page::Input => "Input test",
            Page::Motion => "Motion sensors",
            Page::Info => "Controller info",
        }
    }

    fn shows_live_input(self) -> bool {
        matches!(self, Page::Triggers | Page::Input | Page::Motion)
    }
}

/// A slow action started from the UI and run on its own thread.
#[derive(Clone, Default)]
enum Job {
    #[default]
    Idle,
    Running,
    Finished(Result<(), String>),
}

fn spawn_job(
    job: &Arc<Mutex<Job>>,
    ctx: &egui::Context,
    task: impl FnOnce() -> Result<(), String> + Send + 'static,
) {
    *job.lock().unwrap() = Job::Running;
    let job = job.clone();
    let ctx = ctx.clone();
    thread::spawn(move || {
        let result = task();
        *job.lock().unwrap() = Job::Finished(result);
        ctx.request_repaint();
    });
}

struct App {
    config: Config,
    /// Last config handed to the worker.
    sent: Config,
    /// Key of the controller being configured.
    selected: Option<String>,
    /// Working copy of the selected controller's profile; the pages edit it
    /// and it is written back into `config` every frame.
    profile: Profile,
    unsaved_since: Option<Instant>,
    page: Page,
    requests: Sender<Request>,
    worker: Handle,
    notice: Option<String>,
    udev: Arc<Mutex<Job>>,
    test_sound: Arc<Mutex<Job>>,
    test_rumble: (u8, u8),
    /// Draw the sensor axes on the 3D controller.
    show_axes: bool,
    screenshot: Option<screenshot::Screenshot>,
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        notice: Option<String>,
        page: Page,
        controller: Option<String>,
    ) -> Self {
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 8.0);
            style.spacing.slider_width = 220.0;
        });
        let (requests, receiver) = mpsc::channel();
        let worker = worker::spawn(config.clone(), receiver, cc.egui_ctx.clone());
        let mut app = App {
            sent: config.clone(),
            config,
            selected: None,
            profile: Profile::default(),
            unsaved_since: None,
            page,
            requests,
            worker,
            notice,
            udev: Arc::default(),
            test_sound: Arc::default(),
            test_rumble: (180, 180),
            show_axes: true,
            screenshot: screenshot::Screenshot::from_env(),
        };

        // Pick a controller and let it connect before the first frame.
        let started = Instant::now();
        let mut controller = controller;
        while started.elapsed() < STARTUP_WAIT {
            let snap = app.worker.snapshot.lock().unwrap().clone();
            if !snap.scanned {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            for device in &snap.devices {
                app.config.remember(device);
            }
            if let Some(wanted) = controller.take() {
                match app.find_controller(&wanted, &snap) {
                    Some(key) => app.select(Some(key)),
                    None => app.notice = Some(format!("No controller called \"{wanted}\"")),
                }
            }
            app.track_controllers(&snap);
            let ready = app.selected.is_none()
                || snap.input.is_some()
                || matches!(snap.connection, Connection::PermissionDenied(_));
            if ready {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        app
    }

    fn request(&self, request: Request) {
        // Only fails if the worker thread died, which leaves nothing to tell.
        let _ = self.requests.send(request);
    }

    fn save(&mut self) {
        self.unsaved_since = None;
        if let Err(e) = self.config.save() {
            self.notice = Some(format!("Could not save the settings: {e}"));
        }
    }

    fn select(&mut self, key: Option<String>) {
        if key == self.selected {
            return;
        }
        if key.is_some() {
            self.config.last_selected = key.clone();
        }
        self.selected = key.clone();
        self.request(Request::Config(Box::new(self.config.clone())));
        self.request(Request::Select(key));
    }

    /// Remembers controllers seen for the first time and keeps a valid
    /// selection: the last used controller if connected, else the first
    /// connected one, else the last used one even if it is offline.
    fn track_controllers(&mut self, snap: &Snapshot) {
        for device in &snap.devices {
            self.config.remember(device);
        }
        let valid = self
            .selected
            .as_ref()
            .is_some_and(|key| self.config.controllers.contains_key(key));
        if valid {
            return;
        }
        let connected: Vec<String> = snap.devices.iter().map(controller_key).collect();
        let last = self.config.last_selected.clone();
        let next = last
            .clone()
            .filter(|key| connected.contains(key))
            .or_else(|| connected.first().cloned())
            .or_else(|| last.filter(|key| self.config.controllers.contains_key(key)));
        self.select(next);
    }

    fn find_controller(&self, wanted: &str, snap: &Snapshot) -> Option<String> {
        if let Some(key) = self.config.key_by_name(wanted) {
            return Some(key.to_string());
        }
        let lower = wanted.to_lowercase();
        if self.config.controllers.contains_key(&lower) {
            return Some(lower);
        }
        snap.devices
            .iter()
            .find(|d| d.hidraw.to_str() == Some(wanted))
            .map(controller_key)
    }

    fn selected_name(&self) -> Option<&str> {
        let key = self.selected.as_ref()?;
        self.config.controllers.get(key).map(|c| c.name.as_str())
    }

    fn sync_config(&mut self, ctx: &egui::Context) {
        if let Some(controller) = self
            .selected
            .as_ref()
            .and_then(|k| self.config.controllers.get_mut(k))
        {
            controller.profile = self.profile.clone();
        }
        if self.config != self.sent {
            self.sent = self.config.clone();
            self.request(Request::Config(Box::new(self.config.clone())));
            self.unsaved_since = Some(Instant::now());
        }
        if let Some(since) = self.unsaved_since {
            if since.elapsed() >= SAVE_DELAY {
                self.save();
            } else {
                ctx.request_repaint_after(SAVE_DELAY - since.elapsed());
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.horizontal(|ui| {
            ui.heading("ds5ctl");
            ui.separator();
            let name = self.selected_name().unwrap_or_default().to_string();

            match &snap.connection {
                Connection::Connected(info) => {
                    widgets::status_dot(ui, widgets::ONLINE);
                    ui.label(RichText::new(name).strong());
                    ui.label(format!("{} · {}", info.model, info.transport));
                    if let Some(input) = &snap.input {
                        ui.separator();
                        ui.label(format!(
                            "Battery {}",
                            battery_text(input.battery.percent, input.battery.state)
                        ));
                    }
                }
                Connection::Offline => {
                    widgets::status_dot(ui, widgets::OFFLINE);
                    ui.label(RichText::new(name).strong());
                    ui.weak("not connected");
                }
                Connection::Searching => {
                    ui.spinner();
                    ui.label("Waiting for a controller (connect it by USB or press the PS button)");
                }
                Connection::PermissionDenied(_) => {
                    widgets::status_dot(ui, widgets::FAULT);
                    ui.label("No access to the controller");
                }
            }

            if let Connection::Connected(info) = &snap.connection {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if info.transport == Transport::Bluetooth
                        && ui
                            .button("Power off")
                            .on_hover_text("Disconnect and switch off the controller")
                            .clicked()
                    {
                        self.request(Request::PowerOff);
                    }
                    if ui
                        .button("Identify")
                        .on_hover_text("Blink the lightbar and rumble, to tell controllers apart")
                        .clicked()
                    {
                        if let Some(key) = self.selected.clone() {
                            self.request(Request::Identify(key));
                        }
                    }
                });
            }
        });
    }

    /// Connected controllers first, then known ones that are offline.
    fn controller_list(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.label(RichText::new("Controllers").small().weak());
        let mut rows: Vec<(String, Option<&DeviceInfo>)> = Vec::new();
        for device in &snap.devices {
            let key = controller_key(device);
            if !rows.iter().any(|(k, _)| *k == key) {
                rows.push((key, Some(device)));
            }
        }
        for key in self.config.controllers.keys() {
            if !rows.iter().any(|(k, _)| k == key) {
                rows.push((key.clone(), None));
            }
        }
        if rows.is_empty() {
            ui.weak("None connected");
        }

        for (key, device) in rows {
            let name = self
                .config
                .controllers
                .get(&key)
                .map(|c| c.name.trim().to_string())
                .filter(|name| !name.is_empty())
                .or_else(|| device.map(default_name))
                .unwrap_or_else(|| key.clone());
            let detail = match device {
                Some(device) => match snap.batteries.get(&key) {
                    Some(b) => format!(
                        "{} · {}",
                        device.transport,
                        battery_text(b.percent, b.state)
                    ),
                    None => device.transport.to_string(),
                },
                None => "Not connected".to_string(),
            };
            let selected = self.selected.as_deref() == Some(key.as_str());
            if widgets::controller_row(ui, &name, &detail, device.is_some(), selected).clicked() {
                self.select(Some(key));
            }
        }
    }

    fn permission_banner(&mut self, ui: &mut egui::Ui, path: &Path) {
        egui::Frame::group(ui.style())
            .fill(ui.visuals().faint_bg_color)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("ds5ctl cannot open the controller").strong());
                ui.label(format!(
                "{} is only accessible to root. Installing the udev rule gives your user access \
                 (asks for the admin password once).",
                path.display()
            ));
                let state = self.udev.lock().unwrap().clone();
                match state {
                    Job::Idle => {
                        if ui.button("Install udev rule").clicked() {
                            self.install_udev(ui.ctx());
                        }
                    }
                    Job::Running => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Waiting for authentication…");
                        });
                    }
                    Job::Finished(Ok(())) => {
                        ui.label("Rule installed. Reconnect the controller if it does not appear.");
                    }
                    Job::Finished(Err(e)) => {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                        if ui.button("Try again").clicked() {
                            self.install_udev(ui.ctx());
                        }
                    }
                }
            });
    }

    fn install_udev(&self, ctx: &egui::Context) {
        spawn_job(&self.udev, ctx, udev::install);
    }

    fn lighting_page(&mut self, ui: &mut egui::Ui) {
        section(ui, "Lightbar", |ui| {
            let lightbar = &mut self.profile.lightbar;
            ui.horizontal_top(|ui| {
                let [r, g, b] = lightbar.color;
                let mut color = Color32::from_rgb(r, g, b);
                if color_picker::color_picker_color32(ui, &mut color, color_picker::Alpha::Opaque) {
                    lightbar.color = [color.r(), color.g(), color.b()];
                }
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.label("Brightness");
                    ui.add(egui::Slider::new(&mut lightbar.brightness, 0..=100).suffix("%"));
                    ui.label("Presets");
                    ui.horizontal_wrapped(|ui| {
                        for (name, rgb) in COLOR_PRESETS {
                            if widgets::swatch(ui, Color32::from_rgb(rgb[0], rgb[1], rgb[2]), name)
                            {
                                lightbar.color = rgb;
                                lightbar.brightness = lightbar.brightness.max(1);
                            }
                        }
                    });
                    ui.label("On the controller");
                    let (r, g, b) = lightbar.output_rgb();
                    color_picker::show_color(
                        ui,
                        Color32::from_rgb(r, g, b),
                        egui::vec2(64.0, 22.0),
                    );
                    if ui.button("Turn off").clicked() {
                        lightbar.brightness = 0;
                    }
                });
            });
        });

        section(ui, "Player LEDs", |ui| {
            let leds = &mut self.profile.player_leds;
            widgets::player_leds(ui, &mut leds.mask);
            ui.horizontal(|ui| {
                ui.label("Pattern:");
                if ui.selectable_label(leds.mask == 0, "Off").clicked() {
                    leds.mask = 0;
                }
                for (i, pattern) in PLAYER_PATTERNS.iter().enumerate() {
                    let label = format!("Player {}", i + 1);
                    if ui.selectable_label(leds.mask == *pattern, label).clicked() {
                        leds.mask = *pattern;
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Brightness:");
                ui.radio_value(&mut leds.brightness, PlayerLedBrightness::High, "High");
                ui.radio_value(&mut leds.brightness, PlayerLedBrightness::Medium, "Medium");
                ui.radio_value(&mut leds.brightness, PlayerLedBrightness::Low, "Low");
            });
        });

        section(ui, "Mute button LED", |ui| {
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.profile.mic_led, MicLed::Off, "Off");
                ui.radio_value(&mut self.profile.mic_led, MicLed::On, "On");
                ui.radio_value(&mut self.profile.mic_led, MicLed::Pulse, "Pulsing");
            });
            ui.weak("Pressing the mute button also toggles this LED (handled by the kernel).");
        });
    }

    fn triggers_page(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.weak("Trigger travel is split into zones 0 (released) to 9 (fully pressed). Press the triggers to see where you are.");
        ui.add_space(4.0);
        let mut copy: Option<bool> = None;
        ui.columns(2, |columns| {
            let pressures = snap.input.as_ref().map(|i| (i.l2, i.r2));
            let triggers = [
                (
                    "L2",
                    &mut self.profile.left_trigger,
                    pressures.map(|p| p.0),
                    "Copy to R2",
                ),
                (
                    "R2",
                    &mut self.profile.right_trigger,
                    pressures.map(|p| p.1),
                    "Copy to L2",
                ),
            ];
            for (i, (column, (name, effect, pressure, copy_label))) in
                columns.iter_mut().zip(triggers).enumerate()
            {
                egui::Frame::group(column.style()).show(column, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.heading(name);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(copy_label).clicked() {
                                copy = Some(i == 0);
                            }
                        });
                    });
                    widgets::trigger_zones(ui, effect, pressure);
                    trigger_editor(ui, name, effect);
                });
            }
        });
        match copy {
            Some(true) => self.profile.right_trigger = self.profile.left_trigger.clone(),
            Some(false) => self.profile.left_trigger = self.profile.right_trigger.clone(),
            None => {}
        }
        if ui.button("Turn both off").clicked() {
            self.profile.left_trigger = TriggerEffect::Off;
            self.profile.right_trigger = TriggerEffect::Off;
        }
    }

    fn vibration_page(&mut self, ui: &mut egui::Ui) {
        section(ui, "Strength", |ui| {
            let percent = |v: f64, _: RangeInclusive<usize>| format!("{}%", v * 12.5);
            let vibration = &mut self.profile.vibration;
            ui.add(
                egui::Slider::new(&mut vibration.rumble_strength, 1..=8)
                    .custom_formatter(percent)
                    .text("Rumble motors"),
            );
            ui.add(
                egui::Slider::new(&mut vibration.trigger_strength, 1..=8)
                    .custom_formatter(percent)
                    .text("Trigger effects"),
            );
            ui.weak("Applies to everything: games, Steam and the tests below.");
        });

        section(ui, "Test", |ui| {
            ui.add(egui::Slider::new(&mut self.test_rumble.0, 0..=255).text("Heavy motor (left)"));
            ui.add(egui::Slider::new(&mut self.test_rumble.1, 0..=255).text("Light motor (right)"));
            ui.horizontal(|ui| {
                let (left, right) = self.test_rumble;
                for (label, l, r) in [
                    ("Rumble", left, right),
                    ("Heavy only", left, 0),
                    ("Light only", 0, right),
                ] {
                    if ui.button(label).clicked() {
                        self.request(Request::Rumble {
                            left: l,
                            right: r,
                            duration: Duration::from_millis(600),
                        });
                    }
                }
            });
        });
    }

    fn audio_page(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let connected = match &snap.connection {
            Connection::Connected(info) => Some(info.clone()),
            _ => None,
        };
        let usb = connected
            .as_ref()
            .is_some_and(|info| info.transport == Transport::Usb);
        if !usb {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Audio settings only matter over USB: Linux does not support DualSense audio over Bluetooth.",
            );
        }

        section(ui, "Test", |ui| {
            let state = self.test_sound.lock().unwrap().clone();
            ui.horizontal(|ui| {
                let playing = matches!(state, Job::Running);
                let button = ui
                    .add_enabled(usb && !playing, egui::Button::new("Play test sound"))
                    .on_hover_text("Plays a short chime through this controller only");
                if button.clicked() {
                    if let Some(info) = connected.clone() {
                        spawn_job(&self.test_sound, ui.ctx(), move || {
                            sound::play_test_sound(&info)
                        });
                    }
                }
                match state {
                    Job::Running => {
                        ui.spinner();
                    }
                    Job::Finished(Err(e)) => {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    _ => {}
                }
            });
            ui.weak("Your default audio output stays as it is. Use it to hear what the settings below change.");
        });

        let audio = &mut self.profile.audio;
        section(ui, "Audio", |ui| {
            ui.checkbox(&mut audio.enabled, "Let ds5ctl control audio")
                .on_hover_text(
                    "When off, the kernel switches between speaker and headphones automatically.",
                );
            ui.add_enabled_ui(audio.enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Output:");
                    ui.radio_value(&mut audio.output, AudioOutput::Speaker, "Built-in speaker");
                    ui.radio_value(&mut audio.output, AudioOutput::Headphones, "Headphone jack");
                });
                ui.add(
                    egui::Slider::new(&mut audio.speaker_volume, 0..=SPEAKER_VOLUME_MAX)
                        .text("Speaker volume"),
                );
                ui.add(
                    egui::Slider::new(&mut audio.headphone_volume, 0..=HEADPHONE_VOLUME_MAX)
                        .text("Headphone volume"),
                );
                ui.add(
                    egui::Slider::new(&mut audio.mic_volume, 0..=MIC_VOLUME_MAX)
                        .text("Microphone gain"),
                );
                ui.checkbox(&mut audio.mic_muted, "Mute microphone");
            });
        });
        if let Some(input) = &snap.input {
            ui.label(format!(
                "Headphones: {}   Microphone: {}",
                if input.headphones_plugged {
                    "plugged in"
                } else {
                    "not plugged"
                },
                if input.mic_plugged {
                    "plugged in"
                } else {
                    "not plugged"
                },
            ));
        }
    }

    fn input_page(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let Some(input) = &snap.input else {
            ui.label("Connect a controller to see its input.");
            return;
        };
        ui.horizontal_top(|ui| {
            for (name, position, pressed) in [
                ("Left stick", input.left_stick, input.buttons.l3),
                ("Right stick", input.right_stick, input.buttons.r3),
            ] {
                ui.vertical(|ui| {
                    ui.label(name);
                    widgets::stick(ui, position, pressed);
                    ui.monospace(format!("{:3} {:3}", position.0, position.1));
                });
                ui.add_space(12.0);
            }
            ui.vertical(|ui| {
                for (name, value) in [("L2", input.l2), ("R2", input.r2)] {
                    ui.horizontal(|ui| {
                        ui.label(name);
                        widgets::meter(ui, value);
                        ui.monospace(format!("{value:3}"));
                    });
                }
            });
        });

        section(ui, "Buttons", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (name, pressed) in button_states(&input.buttons) {
                    let (bg, fg) = if pressed {
                        (
                            ui.visuals().selection.bg_fill,
                            ui.visuals().selection.stroke.color,
                        )
                    } else {
                        (
                            ui.visuals().extreme_bg_color,
                            ui.visuals().weak_text_color(),
                        )
                    };
                    ui.label(
                        RichText::new(format!(" {name} "))
                            .monospace()
                            .background_color(bg)
                            .color(fg),
                    );
                }
            });
        });

        section(ui, "Touchpad", |ui| {
            widgets::touchpad(ui, &input.touch, input.buttons.touchpad)
        });

        ui.weak("Gyroscope and accelerometer are on the Motion sensors page.");
    }

    fn motion_page(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let Some(motion) = &snap.motion else {
            ui.label("Connect a controller to see its motion sensors.");
            return;
        };
        let (r, g, b) = self.profile.lightbar.output_rgb();
        ui.horizontal_top(|ui| {
            let width = (ui.available_width() - 280.0).clamp(260.0, 640.0);
            model3d::controller(
                ui,
                motion.orientation,
                Color32::from_rgb(r, g, b),
                egui::vec2(width, width * 0.72),
                self.show_axes,
            );
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if ui
                        .button("Recenter")
                        .on_hover_text("Make the current direction straight ahead")
                        .clicked()
                    {
                        self.request(Request::Recenter);
                    }
                    ui.checkbox(&mut self.show_axes, "Show axes");
                });
                let [pitch, roll, yaw] = motion.angles_deg;
                value_grid(
                    ui,
                    "Orientation",
                    &[
                        ("Pitch", pitch, "°"),
                        ("Roll", roll, "°"),
                        ("Yaw", yaw, "°"),
                    ],
                );
                let [gx, gy, gz] = motion.gyro_dps;
                value_grid(
                    ui,
                    "Gyroscope",
                    &[
                        ("X (pitch)", gx, "°/s"),
                        ("Y (yaw)", gy, "°/s"),
                        ("Z (roll)", gz, "°/s"),
                    ],
                );
                let [ax, ay, az] = motion.accel_g;
                let total = (ax * ax + ay * ay + az * az).sqrt();
                value_grid(
                    ui,
                    "Accelerometer",
                    &[
                        ("X", ax, "g"),
                        ("Y", ay, "g"),
                        ("Z", az, "g"),
                        ("Total", total, "g"),
                    ],
                );
                if motion.still {
                    ui.colored_label(widgets::ONLINE, "Lying still: correcting gyro drift");
                } else {
                    ui.weak("Put it down for a moment to correct gyro drift.");
                }
            });
        });
        ui.weak("Pitch and roll follow gravity. Yaw has no compass to rely on and slowly drifts; press Recenter to reset it.");
    }

    fn info_page(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let Some(key) = self.selected.clone() else {
            return;
        };
        if let Some(controller) = self.config.controllers.get_mut(&key) {
            section(ui, "Name", |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut controller.name)
                        .hint_text("e.g. White, Player 2")
                        .desired_width(260.0),
                );
                ui.weak("Shown in the controller list; the command line accepts it too: ds5ctl -d <name>");
            });
        }

        let mut forget = false;
        section(ui, "Controller", |ui| {
            let Connection::Connected(info) = &snap.connection else {
                egui::Grid::new("info")
                    .num_columns(2)
                    .spacing([24.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("ID");
                        ui.monospace(&key);
                        ui.end_row();
                    });
                ui.label("Not connected. Its settings are kept and sent as soon as it connects.");
                forget = ui
                    .button("Forget this controller")
                    .on_hover_text("Delete its saved settings")
                    .clicked();
                return;
            };
            egui::Grid::new("info")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    let mut row = |key: &str, value: String| {
                        ui.label(key);
                        ui.monospace(value);
                        ui.end_row();
                    };
                    row("Model", info.model.to_string());
                    row("Connection", info.transport.to_string());
                    row(
                        "MAC address",
                        snap.mac.clone().unwrap_or_else(|| "unknown".into()),
                    );
                    row("Device node", info.hidraw.display().to_string());
                    row("Kernel device", info.instance.clone());
                    if let Some(fw) = &snap.firmware {
                        row("Firmware", format!("0x{:08x}", fw.firmware_version));
                        row("Update version", format!("0x{:04x}", fw.update_version));
                        row("Hardware", format!("0x{:08x}", fw.hardware_version));
                        row("Built", format!("{} {}", fw.build_date, fw.build_time));
                    }
                    if let Some(input) = &snap.input {
                        row(
                            "Battery",
                            battery_text(input.battery.percent, input.battery.state),
                        );
                    }
                });
        });
        if forget {
            self.config.controllers.remove(&key);
            if self.config.last_selected.as_deref() == Some(key.as_str()) {
                self.config.last_selected = None;
            }
            self.select(None);
            return;
        }

        section(ui, "ds5ctl", |ui| {
            if let Some(path) = Config::path() {
                ui.label(format!("Settings: {}", path.display()));
            }
            let installed = Path::new(udev::RULE_PATH).exists();
            ui.label(format!(
                "udev rule: {}",
                if installed {
                    udev::RULE_PATH
                } else {
                    "not installed (access may come from another rule, e.g. Steam's)"
                }
            ));
            ui.weak("Controllers forget their settings when they disconnect. While this window is open it re-applies them; run `ds5ctl daemon` (see README) to do that in the background.");
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let snap = self.worker.snapshot.lock().unwrap().clone();
        self.track_controllers(&snap);
        self.worker
            .live
            .store(self.page.shows_live_input(), Ordering::Relaxed);

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui, &snap);
            ui.add_space(2.0);
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(error) = snap.error.as_ref().or(self.notice.as_ref()) {
                    ui.colored_label(ui.visuals().warn_fg_color, error);
                    if self.notice.is_some() && ui.small_button("Dismiss").clicked() {
                        self.notice = None;
                    }
                } else if self.unsaved_since.is_some() {
                    ui.weak("Saving…");
                } else {
                    ui.weak("All changes are applied live and saved automatically.");
                }
            });
        });

        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(210.0)
            .show(ui, |ui| {
                ui.add_space(8.0);
                self.controller_list(ui, &snap);
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
                for page in Page::ALL {
                    let button = egui::Button::selectable(self.page == page, page.title())
                        .min_size(egui::vec2(ui.available_width(), 28.0));
                    if ui.add(button).clicked() {
                        self.page = page;
                    }
                }
            });

        // The pages edit a copy of the selected controller's profile, written
        // back by `sync_config`. Loaded after the list so a click there shows
        // the new controller right away.
        self.profile = match &self.selected {
            Some(key) => self.config.profile_for(key).clone(),
            None => Profile::default(),
        };

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    if let Connection::PermissionDenied(path) = &snap.connection {
                        self.permission_banner(ui, path);
                        ui.add_space(8.0);
                    }
                    if self.selected.is_none() {
                        ui.heading("No controller yet");
                        ui.label("Connect a DualSense with a USB cable, or pair it over Bluetooth: hold Create and PS until the lightbar flashes, then pair it in your Bluetooth settings.");
                        return;
                    }
                    if snap.connection == Connection::Offline {
                        let name = self.selected_name().unwrap_or("This controller");
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!("{name} is not connected. Changes are saved and sent when it connects."),
                        );
                    }
                    ui.heading(self.page.title());
                    ui.add_space(4.0);
                    match self.page {
                        Page::Lighting => self.lighting_page(ui),
                        Page::Triggers => self.triggers_page(ui, &snap),
                        Page::Vibration => self.vibration_page(ui),
                        Page::Audio => self.audio_page(ui, &snap),
                        Page::Input => self.input_page(ui, &snap),
                        Page::Motion => self.motion_page(ui, &snap),
                        Page::Info => self.info_page(ui, &snap),
                    }
                });
        });

        self.sync_config(ui.ctx());
        if let Some(screenshot) = &mut self.screenshot {
            screenshot.update(ui.ctx());
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.unsaved_since.is_some() || self.config != self.sent {
            self.save();
        }
    }
}

const COLOR_PRESETS: [(&str, [u8; 3]); 10] = [
    ("Blue", [0, 0, 255]),
    ("Cyan", [0, 200, 255]),
    ("Green", [0, 255, 40]),
    ("Yellow", [255, 200, 0]),
    ("Orange", [255, 80, 0]),
    ("Red", [255, 0, 0]),
    ("Pink", [255, 0, 140]),
    ("Purple", [128, 0, 255]),
    ("White", [255, 255, 255]),
    ("Warm white", [255, 150, 60]),
];

fn section(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).strong());
        add_contents(ui);
    });
    ui.add_space(6.0);
}

/// A titled block of signed readings with units, e.g. "+12.3 °/s".
fn value_grid(ui: &mut egui::Ui, title: &str, rows: &[(&str, f32, &str)]) {
    ui.add_space(4.0);
    ui.label(RichText::new(title).strong());
    egui::Grid::new(title)
        .num_columns(2)
        .spacing([16.0, 2.0])
        .show(ui, |ui| {
            for (name, value, unit) in rows {
                ui.label(*name);
                ui.monospace(format!("{value:+8.2} {unit}"));
                ui.end_row();
            }
        });
}

fn battery_text(percent: u8, state: ChargingState) -> String {
    match state {
        ChargingState::Discharging => format!("{percent}%"),
        ChargingState::Charging => format!("{percent}%, charging"),
        ChargingState::Full => "full".into(),
        ChargingState::Error => "error".into(),
    }
}

fn button_states(b: &Buttons) -> [(&'static str, bool); 19] {
    [
        ("↑", b.up),
        ("↓", b.down),
        ("←", b.left),
        ("→", b.right),
        ("Cross", b.cross),
        ("Circle", b.circle),
        ("Square", b.square),
        ("Triangle", b.triangle),
        ("L1", b.l1),
        ("R1", b.r1),
        ("L2", b.l2),
        ("R2", b.r2),
        ("L3", b.l3),
        ("R3", b.r3),
        ("Create", b.create),
        ("Options", b.options),
        ("PS", b.ps),
        ("Touchpad", b.touchpad),
        ("Mute", b.mute),
    ]
}

// ---- Trigger effect editor ----

const EFFECTS: [&str; 9] = [
    "Off",
    "Resistance",
    "Weapon",
    "Vibration",
    "Bow",
    "Galloping",
    "Machine",
    "Slope",
    "Custom zones",
];

fn effect_name(effect: &TriggerEffect) -> &'static str {
    match effect {
        TriggerEffect::Off => EFFECTS[0],
        TriggerEffect::Feedback { .. } => EFFECTS[1],
        TriggerEffect::Weapon { .. } => EFFECTS[2],
        TriggerEffect::Vibration { .. } => EFFECTS[3],
        TriggerEffect::Bow { .. } => EFFECTS[4],
        TriggerEffect::Galloping { .. } => EFFECTS[5],
        TriggerEffect::Machine { .. } => EFFECTS[6],
        TriggerEffect::Slope { .. } => EFFECTS[7],
        TriggerEffect::MultiFeedback { .. } => EFFECTS[8],
    }
}

fn default_effect(name: &str) -> TriggerEffect {
    match name {
        "Resistance" => TriggerEffect::Feedback {
            position: 3,
            strength: 5,
        },
        "Weapon" => TriggerEffect::Weapon {
            start: 2,
            end: 6,
            strength: 8,
        },
        "Vibration" => TriggerEffect::Vibration {
            position: 0,
            amplitude: 4,
            frequency: 40,
        },
        "Bow" => TriggerEffect::Bow {
            start: 1,
            end: 6,
            strength: 6,
            snap: 4,
        },
        "Galloping" => TriggerEffect::Galloping {
            start: 0,
            end: 9,
            first_foot: 2,
            second_foot: 5,
            frequency: 3,
        },
        "Machine" => TriggerEffect::Machine {
            start: 1,
            end: 9,
            amplitude_a: 3,
            amplitude_b: 7,
            frequency: 5,
            period: 3,
        },
        "Slope" => TriggerEffect::Slope {
            start: 0,
            end: 9,
            start_strength: 1,
            end_strength: 8,
        },
        "Custom zones" => TriggerEffect::MultiFeedback {
            strengths: [0, 1, 2, 3, 4, 5, 6, 7, 8, 8],
        },
        _ => TriggerEffect::Off,
    }
}

fn effect_help(effect: &TriggerEffect) -> &'static str {
    match effect {
        TriggerEffect::Off => "No resistance.",
        TriggerEffect::Feedback { .. } => "Constant resistance from the start zone to the end.",
        TriggerEffect::Weapon { .. } => "Resistance that breaks with a click, like a gun trigger.",
        TriggerEffect::Vibration { .. } => "The trigger buzzes once pressed past the start zone.",
        TriggerEffect::Bow { .. } => "Tension that snaps back when released, like drawing a bow.",
        TriggerEffect::Galloping { .. } => "A rhythmic two-beat pulse, like hooves.",
        TriggerEffect::Machine { .. } => "Vibration alternating between two strengths.",
        TriggerEffect::Slope { .. } => "Resistance that ramps up (or down) along the travel.",
        TriggerEffect::MultiFeedback { .. } => "Set the resistance of every zone yourself.",
    }
}

fn slider(ui: &mut egui::Ui, value: &mut u8, range: RangeInclusive<u8>, label: &str) {
    ui.add(egui::Slider::new(value, range).text(label));
}

fn trigger_editor(ui: &mut egui::Ui, id: &str, effect: &mut TriggerEffect) {
    // Leave room for the value box and the label so both columns fit.
    ui.spacing_mut().slider_width = (ui.available_width() - 170.0).clamp(60.0, 220.0);
    let current = effect_name(effect);
    egui::ComboBox::from_id_salt(id)
        .selected_text(current)
        .width(180.0)
        .show_ui(ui, |ui| {
            for name in EFFECTS {
                if ui.selectable_label(current == name, name).clicked() && current != name {
                    *effect = default_effect(name);
                }
            }
        });
    ui.weak(effect_help(effect));

    match effect {
        TriggerEffect::Off => {}
        TriggerEffect::Feedback { position, strength } => {
            slider(ui, position, 0..=9, "Start zone");
            slider(ui, strength, 1..=8, "Strength");
        }
        TriggerEffect::Weapon {
            start,
            end,
            strength,
        } => {
            slider(ui, start, 2..=7, "Start zone");
            *end = (*end).clamp(*start + 1, 8);
            slider(ui, end, *start + 1..=8, "Break zone");
            slider(ui, strength, 1..=8, "Strength");
        }
        TriggerEffect::Vibration {
            position,
            amplitude,
            frequency,
        } => {
            slider(ui, position, 0..=9, "Start zone");
            slider(ui, amplitude, 1..=8, "Amplitude");
            ui.add(
                egui::Slider::new(frequency, 1..=255)
                    .suffix(" Hz")
                    .text("Frequency"),
            );
        }
        TriggerEffect::Bow {
            start,
            end,
            strength,
            snap,
        } => {
            slider(ui, start, 0..=7, "Start zone");
            *end = (*end).clamp(*start + 1, 8);
            slider(ui, end, *start + 1..=8, "End zone");
            slider(ui, strength, 1..=8, "Tension");
            slider(ui, snap, 1..=8, "Snap back");
        }
        TriggerEffect::Galloping {
            start,
            end,
            first_foot,
            second_foot,
            frequency,
        } => {
            slider(ui, start, 0..=8, "Start zone");
            *end = (*end).clamp(*start + 1, 9);
            slider(ui, end, *start + 1..=9, "End zone");
            slider(ui, first_foot, 0..=6, "First beat");
            *second_foot = (*second_foot).clamp(*first_foot + 1, 7);
            slider(ui, second_foot, *first_foot + 1..=7, "Second beat");
            ui.add(
                egui::Slider::new(frequency, 1..=40)
                    .suffix(" Hz")
                    .text("Speed"),
            );
        }
        TriggerEffect::Machine {
            start,
            end,
            amplitude_a,
            amplitude_b,
            frequency,
            period,
        } => {
            slider(ui, start, 1..=8, "Start zone");
            *end = (*end).clamp(*start + 1, 9);
            slider(ui, end, *start + 1..=9, "End zone");
            slider(ui, amplitude_a, 0..=7, "Strength A");
            slider(ui, amplitude_b, 0..=7, "Strength B");
            ui.add(
                egui::Slider::new(frequency, 1..=255)
                    .suffix(" Hz")
                    .text("Frequency"),
            );
            slider(ui, period, 0..=50, "Switch every (×0.1 s)");
        }
        TriggerEffect::Slope {
            start,
            end,
            start_strength,
            end_strength,
        } => {
            slider(ui, start, 0..=8, "Start zone");
            *end = (*end).clamp(*start + 1, 9);
            slider(ui, end, *start + 1..=9, "End zone");
            slider(ui, start_strength, 1..=8, "Start strength");
            slider(ui, end_strength, 1..=8, "End strength");
        }
        TriggerEffect::MultiFeedback { strengths } => {
            ui.horizontal(|ui| {
                for (zone, strength) in strengths.iter_mut().enumerate() {
                    ui.vertical(|ui| {
                        ui.add(
                            egui::Slider::new(strength, 0..=8)
                                .vertical()
                                .show_value(false),
                        );
                        ui.small(format!("{zone}:{strength}"));
                    });
                }
            });
        }
    }
}
