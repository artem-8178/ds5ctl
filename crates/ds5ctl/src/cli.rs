use std::io::{self, Write};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand, ValueEnum};
use dualsense::{
    enumerate, AudioOutput, ChargingState, DualSense, Error, LightbarFade, MicLed, OutputReport,
    PlayerLedBrightness, Transport, TriggerEffect, TriggerSide, HEADPHONE_VOLUME_MAX,
    MIC_VOLUME_MAX, PLAYER_PATTERNS, SPEAKER_VOLUME_MAX,
};

use crate::config::{controller_key, Config};
use crate::profile::strength_to_reduction;
use crate::{bluez, daemon, gui, sound, udev};

/// Configure DualSense controllers on Linux. Run without a command to open the settings window.
///
/// Commands that change a setting act on the controller immediately and do not
/// touch the saved profile, which the window, `apply` and `daemon` use.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Controller to use: /dev/hidrawN, its MAC address or its name. Defaults to the first one found.
    #[arg(short, long, global = true)]
    device: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the settings window (the default)
    Gui {
        /// Page to open on
        #[arg(long, value_enum)]
        page: Option<gui::Page>,
    },
    /// List connected controllers
    List,
    /// Show firmware, battery and connection details
    Info,
    /// Print live input (buttons, sticks, triggers, touchpad) until Ctrl+C
    Watch,
    /// Set the lightbar colour
    Lightbar {
        /// "#rrggbb", "r,g,b" or a name (red, green, blue, white, purple, ..., off)
        color: String,
        /// Fade the lightbar in or out instead of switching instantly
        #[arg(long, value_enum)]
        fade: Option<FadeArg>,
    },
    /// Set the five white player LEDs under the touchpad
    PlayerLeds {
        /// 0-5 for the PS5 player patterns, or five characters like "x.x.x" (x = on)
        pattern: String,
        #[arg(long, value_enum)]
        brightness: Option<BrightnessArg>,
    },
    /// Control the orange LED on the mute button
    MicLed {
        #[arg(value_enum)]
        mode: MicLedArg,
    },
    /// Set an adaptive trigger effect
    Trigger(TriggerArgs),
    /// Run the rumble motors for a moment
    Rumble {
        /// Heavy (left) motor strength, 0-255
        left: u8,
        /// Light (right) motor strength, 0-255
        right: u8,
        /// How long to rumble, in milliseconds
        #[arg(long, default_value_t = 500)]
        ms: u64,
    },
    /// Weaken the rumble motors and trigger effects, 1 (12.5%) to 8 (full power)
    Strength {
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u8).range(1..=8))]
        rumble: u8,
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u8).range(1..=8))]
        triggers: u8,
    },
    /// Audio settings (only matter over USB)
    Audio(AudioArgs),
    /// Play a short chime through the controller (USB only), leaving the default output alone
    TestSound,
    /// Switch off a Bluetooth controller
    PowerOff,
    /// Send the saved profile to the controller
    Apply,
    /// Stay running and apply the saved profile whenever a controller connects
    Daemon,
    /// Install the udev rule that gives the logged-in user access (asks for the admin password)
    InstallUdev,
    /// Name the controller, to tell several apart (shown in the window, usable with -d)
    Rename { name: String },
}

#[derive(Args)]
#[group(required = true, multiple = true)]
struct AudioArgs {
    /// Route audio to the speaker or the headphone jack
    #[arg(long, value_enum)]
    output: Option<OutputArg>,
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=SPEAKER_VOLUME_MAX as i64))]
    speaker_volume: Option<u8>,
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=HEADPHONE_VOLUME_MAX as i64))]
    headphone_volume: Option<u8>,
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=MIC_VOLUME_MAX as i64))]
    mic_volume: Option<u8>,
    /// Mute the microphone in hardware
    #[arg(long, value_enum)]
    mic_mute: Option<Switch>,
}

#[derive(Args)]
struct TriggerArgs {
    #[arg(value_enum)]
    side: SideArg,
    #[command(subcommand)]
    effect: EffectCmd,
}

/// Trigger travel is divided into zones 0 (released) to 9 (fully pressed).
#[derive(Subcommand)]
enum EffectCmd {
    /// No resistance
    Off,
    /// Constant resistance from a zone to the end of travel
    Feedback {
        /// Zone where resistance starts, 0-9
        position: u8,
        /// 1-8
        strength: u8,
    },
    /// Resistance that gives way with a click, like a gun trigger
    Weapon {
        /// 2-7
        start: u8,
        /// start+1..8
        end: u8,
        /// 1-8
        strength: u8,
    },
    /// Vibration from a zone to the end of travel
    Vibration {
        /// 0-9
        position: u8,
        /// 1-8
        amplitude: u8,
        /// Hz, 1-255
        frequency: u8,
    },
    /// Resistance that snaps back on release, like drawing a bow
    Bow {
        /// 0-8
        start: u8,
        /// start+1..8
        end: u8,
        /// 1-8
        strength: u8,
        /// Snap-back force, 1-8
        snap: u8,
    },
    /// Rhythmic two-step pulse, like a galloping horse
    Galloping {
        /// 0-8
        start: u8,
        /// start+1..9
        end: u8,
        /// 0-6
        first_foot: u8,
        /// first_foot+1..7
        second_foot: u8,
        /// Hz, 1-255
        frequency: u8,
    },
    /// Vibration alternating between two amplitudes
    Machine {
        /// 1-8
        start: u8,
        /// start+1..9
        end: u8,
        /// 0-7
        amplitude_a: u8,
        /// 0-7
        amplitude_b: u8,
        /// Hz, 1-255
        frequency: u8,
        /// Time between amplitude switches, tenths of a second
        period: u8,
    },
    /// Resistance ramping linearly between two zones
    Slope {
        /// 0-8
        start: u8,
        /// start+1..9
        end: u8,
        /// 1-8
        start_strength: u8,
        /// 1-8
        end_strength: u8,
    },
    /// Individual resistance for each of the 10 zones
    Zones {
        /// Ten values 0-8, zone 0 first
        #[arg(num_args = 10, required = true)]
        strengths: Vec<u8>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum SideArg {
    Left,
    Right,
    Both,
}

#[derive(Clone, Copy, ValueEnum)]
enum FadeArg {
    In,
    Out,
}

#[derive(Clone, Copy, ValueEnum)]
enum BrightnessArg {
    High,
    Medium,
    Low,
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputArg {
    Speaker,
    Headphones,
}

#[derive(Clone, Copy, ValueEnum)]
enum Switch {
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
enum MicLedArg {
    Off,
    On,
    Pulse,
}

impl EffectCmd {
    fn to_effect(&self) -> TriggerEffect {
        match *self {
            EffectCmd::Off => TriggerEffect::Off,
            EffectCmd::Feedback { position, strength } => {
                TriggerEffect::Feedback { position, strength }
            }
            EffectCmd::Weapon {
                start,
                end,
                strength,
            } => TriggerEffect::Weapon {
                start,
                end,
                strength,
            },
            EffectCmd::Vibration {
                position,
                amplitude,
                frequency,
            } => TriggerEffect::Vibration {
                position,
                amplitude,
                frequency,
            },
            EffectCmd::Bow {
                start,
                end,
                strength,
                snap,
            } => TriggerEffect::Bow {
                start,
                end,
                strength,
                snap,
            },
            EffectCmd::Galloping {
                start,
                end,
                first_foot,
                second_foot,
                frequency,
            } => TriggerEffect::Galloping {
                start,
                end,
                first_foot,
                second_foot,
                frequency,
            },
            EffectCmd::Machine {
                start,
                end,
                amplitude_a,
                amplitude_b,
                frequency,
                period,
            } => TriggerEffect::Machine {
                start,
                end,
                amplitude_a,
                amplitude_b,
                frequency,
                period,
            },
            EffectCmd::Slope {
                start,
                end,
                start_strength,
                end_strength,
            } => TriggerEffect::Slope {
                start,
                end,
                start_strength,
                end_strength,
            },
            EffectCmd::Zones { ref strengths } => {
                let mut zones = [0u8; 10];
                zones.copy_from_slice(strengths);
                TriggerEffect::MultiFeedback { strengths: zones }
            }
        }
    }
}

fn parse_color(s: &str) -> Result<(u8, u8, u8), String> {
    let named = match s.to_ascii_lowercase().as_str() {
        "off" | "black" => Some((0, 0, 0)),
        "white" => Some((255, 255, 255)),
        "red" => Some((255, 0, 0)),
        "green" => Some((0, 255, 0)),
        "blue" => Some((0, 0, 255)),
        "yellow" => Some((255, 200, 0)),
        "orange" => Some((255, 80, 0)),
        "cyan" => Some((0, 255, 255)),
        "magenta" | "pink" => Some((255, 0, 255)),
        "purple" => Some((128, 0, 255)),
        _ => None,
    };
    if let Some(rgb) = named {
        return Ok(rgb);
    }

    if s.contains(',') {
        let parts: Vec<&str> = s.split(',').map(str::trim).collect();
        if let [r, g, b] = parts[..] {
            let p = |v: &str| v.parse::<u8>().map_err(|_| format!("'{v}' is not 0-255"));
            return Ok((p(r)?, p(g)?, p(b)?));
        }
        return Err(format!("'{s}' should be r,g,b"));
    }

    let hex = s.trim_start_matches('#');
    if hex.len() == 6 {
        if let Ok(v) = u32::from_str_radix(hex, 16) {
            return Ok(((v >> 16) as u8, (v >> 8) as u8, v as u8));
        }
    }
    Err(format!("unknown colour '{s}'"))
}

fn parse_player_pattern(s: &str) -> Result<u8, String> {
    match s {
        "0" | "off" => return Ok(0),
        "1" | "2" | "3" | "4" | "5" => return Ok(PLAYER_PATTERNS[s.parse::<usize>().unwrap() - 1]),
        _ => {}
    }
    if s.chars().count() != 5 {
        return Err(format!("'{s}' should be 0-5 or five characters like x.x.x"));
    }
    s.chars().enumerate().try_fold(0u8, |mask, (i, c)| match c {
        'x' | 'X' | '1' | '#' => Ok(mask | (1 << i)),
        '.' | '-' | '0' | '_' => Ok(mask),
        _ => Err(format!("unexpected '{c}' in LED pattern '{s}'")),
    })
}

fn open_device(selector: Option<&str>) -> Result<DualSense, Error> {
    let Some(selector) = selector else {
        return DualSense::open_first();
    };
    let wanted = selector.to_lowercase();
    let config = Config::load().unwrap_or_default();
    let named = config.key_by_name(selector);
    let info = enumerate()?
        .into_iter()
        .find(|d| {
            d.hidraw.to_str() == Some(selector)
                || d.mac.as_deref() == Some(wanted.as_str())
                || named == Some(controller_key(d).as_str())
        })
        .ok_or_else(|| {
            Error::InvalidParameter(format!("no connected controller matches \"{selector}\""))
        })?;
    DualSense::open(info)
}

fn print_info(ds: &mut DualSense) -> Result<(), Error> {
    let info = ds.info().clone();
    println!("Model:       {} ({})", info.model, info.transport);
    println!("Device:      {}", info.hidraw.display());
    let mac = ds.mac_address().ok().or(info.mac);
    println!("MAC:         {}", mac.as_deref().unwrap_or("unknown"));

    let fw = ds.firmware_info()?;
    println!(
        "Firmware:    0x{:08x} (update 0x{:04x}), built {} {}",
        fw.firmware_version, fw.update_version, fw.build_date, fw.build_time
    );
    println!("Hardware:    0x{:08x}", fw.hardware_version);

    let state = ds.read_input(Duration::from_secs(2))?;
    let charging = match state.battery.state {
        ChargingState::Discharging => "discharging",
        ChargingState::Charging => "charging",
        ChargingState::Full => "full",
        ChargingState::Error => "error",
    };
    println!("Battery:     {}% ({charging})", state.battery.percent);
    let yes_no = |b: bool| if b { "plugged" } else { "no" };
    println!("Headphones:  {}", yes_no(state.headphones_plugged));
    println!("Microphone:  {}", yes_no(state.mic_plugged));
    Ok(())
}

fn watch(ds: &mut DualSense) -> Result<(), Error> {
    let mut last_print = Instant::now() - Duration::from_secs(1);
    let mut out = io::stdout();
    loop {
        let s = ds.read_input(Duration::from_secs(2))?;
        if last_print.elapsed() < Duration::from_millis(50) {
            continue;
        }
        last_print = Instant::now();
        let touch: Vec<String> = s
            .touch
            .iter()
            .flatten()
            .map(|(x, y)| format!("{x},{y}"))
            .collect();
        let _ = write!(
            out,
            "\r\x1b[KL({:3},{:3}) R({:3},{:3}) L2 {:3} R2 {:3} | touch [{}] | {} ",
            s.left_stick.0,
            s.left_stick.1,
            s.right_stick.0,
            s.right_stick.1,
            s.l2,
            s.r2,
            touch.join(" "),
            s.buttons.pressed().join(" "),
        );
        let _ = out.flush();
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let Some(command) = cli.command else {
        return gui::run(None, cli.device);
    };
    match command {
        Command::Gui { page } => gui::run(page, cli.device),
        Command::Daemon => daemon::run(),
        Command::InstallUdev => {
            udev::install()?;
            println!("Installed {}", udev::RULE_PATH);
            Ok(())
        }
        Command::Apply => {
            let ds = open_device(cli.device.as_deref()).map_err(error_text)?;
            let name = daemon::apply(ds.info())?;
            println!("Applied the profile of \"{name}\"");
            Ok(())
        }
        Command::TestSound => {
            let ds = open_device(cli.device.as_deref()).map_err(error_text)?;
            sound::play_test_sound(ds.info())
        }
        Command::Rename { name } => {
            let ds = open_device(cli.device.as_deref()).map_err(error_text)?;
            let mut config = Config::load()?;
            config.remember(ds.info());
            let key = controller_key(ds.info());
            if let Some(controller) = config.controllers.get_mut(&key) {
                controller.name = name.trim().to_string();
            }
            config
                .save()
                .map_err(|e| format!("cannot save the settings: {e}"))?;
            println!("{key} is now called \"{}\"", name.trim());
            Ok(())
        }
        Command::PowerOff => {
            let ds = open_device(cli.device.as_deref()).map_err(error_text)?;
            let info = ds.info();
            if info.transport != Transport::Bluetooth {
                return Err("only Bluetooth controllers can be switched off".into());
            }
            let mac = ds
                .mac_address()
                .ok()
                .or_else(|| info.mac.clone())
                .ok_or("controller MAC address is unknown")?;
            bluez::disconnect(&mac)
        }
        command => run_device_command(cli.device.as_deref(), command).map_err(error_text),
    }
}

fn error_text(e: Error) -> String {
    match e {
        Error::PermissionDenied(_) => format!("{e}; run `ds5ctl install-udev` once to fix this"),
        e => e.to_string(),
    }
}

fn run_device_command(device: Option<&str>, command: Command) -> Result<(), Error> {
    if let Command::List = command {
        let devices = enumerate()?;
        if devices.is_empty() {
            println!("No DualSense controllers found.");
        }
        let config = Config::load().unwrap_or_default();
        for d in devices {
            let battery = d
                .sysfs_battery()
                .map(|b| format!("  {}%", b.percent))
                .unwrap_or_default();
            println!(
                "{}  {:<18}  {} ({})  {}{battery}",
                d.hidraw.display(),
                config.name_of(&d),
                d.model,
                d.transport,
                d.mac.as_deref().unwrap_or("")
            );
        }
        return Ok(());
    }

    let mut ds = open_device(device)?;
    let mut report = OutputReport::new();
    match command {
        Command::List
        | Command::Gui { .. }
        | Command::Daemon
        | Command::Rename { .. }
        | Command::TestSound
        | Command::InstallUdev
        | Command::Apply
        | Command::PowerOff => {
            unreachable!("handled in run()")
        }
        Command::Info => return print_info(&mut ds),
        Command::Watch => return watch(&mut ds),

        Command::Lightbar { color, fade } => {
            let (r, g, b) = parse_color(&color).map_err(Error::InvalidParameter)?;
            report.lightbar(r, g, b);
            if let Some(fade) = fade {
                report.lightbar_fade(match fade {
                    FadeArg::In => LightbarFade::In,
                    FadeArg::Out => LightbarFade::Out,
                });
            }
        }

        Command::PlayerLeds {
            pattern,
            brightness,
        } => {
            let mask = parse_player_pattern(&pattern).map_err(Error::InvalidParameter)?;
            report.player_leds(mask, true);
            if let Some(b) = brightness {
                report.player_led_brightness(match b {
                    BrightnessArg::High => PlayerLedBrightness::High,
                    BrightnessArg::Medium => PlayerLedBrightness::Medium,
                    BrightnessArg::Low => PlayerLedBrightness::Low,
                });
            }
        }

        Command::MicLed { mode } => {
            report.mic_led(match mode {
                MicLedArg::Off => MicLed::Off,
                MicLedArg::On => MicLed::On,
                MicLedArg::Pulse => MicLed::Pulse,
            });
        }

        Command::Trigger(args) => {
            let effect = args.effect.to_effect();
            let sides: &[TriggerSide] = match args.side {
                SideArg::Left => &[TriggerSide::Left],
                SideArg::Right => &[TriggerSide::Right],
                SideArg::Both => &[TriggerSide::Left, TriggerSide::Right],
            };
            for &side in sides {
                report.trigger(side, &effect)?;
            }
        }

        Command::Rumble { left, right, ms } => {
            report.rumble(left, right);
            ds.send(&report)?;
            thread::sleep(Duration::from_millis(ms));
            report = OutputReport::new();
            report.rumble(0, 0);
        }

        Command::Strength { rumble, triggers } => {
            report.motor_power_reduction(
                strength_to_reduction(rumble),
                strength_to_reduction(triggers),
            );
        }

        Command::Audio(args) => {
            if let Some(output) = args.output {
                report.audio_output(match output {
                    OutputArg::Speaker => AudioOutput::Speaker,
                    OutputArg::Headphones => AudioOutput::Headphones,
                });
            }
            if let Some(v) = args.speaker_volume {
                report.speaker_volume(v);
            }
            if let Some(v) = args.headphone_volume {
                report.headphone_volume(v);
            }
            if let Some(v) = args.mic_volume {
                report.mic_volume(v);
            }
            if let Some(mute) = args.mic_mute {
                report.mic_mute(matches!(mute, Switch::On));
            }
        }
    }
    ds.send(&report)
}

pub fn main() -> ExitCode {
    // Exit quietly when piped into `head` and friends instead of panicking.
    // SAFETY: restoring the default disposition of a signal is always sound.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ds5ctl: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(parse_color("#ff8000"), Ok((255, 128, 0)));
        assert_eq!(parse_color("00ff00"), Ok((0, 255, 0)));
        assert_eq!(parse_color("10, 20,30"), Ok((10, 20, 30)));
        assert_eq!(parse_color("Purple"), Ok((128, 0, 255)));
        assert!(parse_color("300,0,0").is_err());
        assert!(parse_color("nope").is_err());
    }

    #[test]
    fn player_patterns() {
        assert_eq!(parse_player_pattern("off"), Ok(0));
        assert_eq!(parse_player_pattern("1"), Ok(0b00100));
        assert_eq!(parse_player_pattern("x...x"), Ok(0b10001));
        assert_eq!(parse_player_pattern("xx..."), Ok(0b00011));
        assert!(parse_player_pattern("xx").is_err());
    }

    #[test]
    fn cli_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
