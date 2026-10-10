# ds5ctl

Settings app for Sony DualSense controllers on Linux: lightbar, player LEDs,
mute LED, adaptive triggers, vibration strength, audio, battery and firmware
info. Works over USB and Bluetooth, ships as a single binary with no
desktop-specific dependencies (no Qt, no GTK), and talks to the controller
directly through `/dev/hidraw*`.

Run `ds5ctl` for the settings window, or use the subcommands from scripts.

## Build

Needs Rust 1.95 or newer.

```sh
cargo build --release
./target/release/ds5ctl
```

The binary only links against glibc. Graphics libraries (Wayland or X11,
OpenGL) are loaded at runtime, so it runs on any desktop.

## Install

```sh
install -Dm755 target/release/ds5ctl ~/.local/bin/ds5ctl
install -Dm644 dist/ds5ctl.desktop ~/.local/share/applications/ds5ctl.desktop
ds5ctl install-udev
```

`install-udev` is the only step that needs root: it asks for the admin
password once and installs `dist/70-ds5ctl.rules`, which gives the logged-in
user access to DualSense hidraw nodes. The settings window offers the same
button when it cannot open the controller.

### Keep settings across reconnects

The controller forgets everything when it disconnects. The daemon re-applies
the saved profile every time a controller connects:

```sh
install -Dm644 dist/ds5ctl.service ~/.config/systemd/user/ds5ctl.service
systemctl --user enable --now ds5ctl.service
```

## Settings window

Every change is sent to the selected controller immediately and saved to
`~/.config/ds5ctl/config.json`.

### Several controllers

Each controller keeps its own settings, identified by its MAC address, so it
gets them back over USB or Bluetooth and after re-pairing. The list on the
left shows every connected controller with its battery, plus the ones
configured before that are offline now; offline ones can still be edited.

- **Identify** blinks the lightbar and rumbles, to tell identical controllers apart.
- Give each one a name on the *Controller info* page (also `ds5ctl rename`),
  then use it on the command line: `ds5ctl -d White lightbar red`.
- While the window is open, controllers that connect get their own profile
  even when they are not the selected one; the daemon does the same in the
  background.
- A controller seen for the first time starts from the default profile.

- **Lighting**: lightbar colour and brightness, player LEDs, mute button LED
- **Adaptive triggers**: nine effects per trigger with a live view of the
  trigger position over the effect zones
- **Vibration**: rumble and trigger effect strength, motor test
- **Audio** (USB only): a test sound played through the controller without
  changing your default output, speaker/headphone routing and volumes, mic
  gain and mute
- **Input test**: sticks, buttons, triggers and touchpad
- **Motion sensors**: a 3D controller that follows the real one, plus
  calibrated gyroscope (°/s), accelerometer (g) and pitch/roll/yaw. Pitch
  and roll are locked to gravity; yaw has no compass and slowly drifts, so
  there is a *Recenter* button. Gyro drift is re-learned whenever the
  controller lies still.
- **Controller info**: firmware, MAC address, battery; power off (Bluetooth)

## Command line

Setting commands act on the controller right away and do not change the
saved profile.

```sh
ds5ctl list                              # connected controllers
ds5ctl info                              # firmware, MAC, battery, headset jack
ds5ctl watch                             # live buttons, sticks, triggers, touchpad

ds5ctl lightbar '#8000ff'                # also r,g,b or a name; "off" turns it off
ds5ctl player-leds 2 --brightness low    # PS5 player pattern 0-5, or "x.x.x"
ds5ctl mic-led pulse                     # off | on | pulse

ds5ctl trigger right weapon 2 6 8        # gun-like click
ds5ctl trigger left feedback 3 6         # resistance from zone 3, strength 6
ds5ctl trigger both vibration 0 4 40     # buzz at 40 Hz
ds5ctl trigger both off

ds5ctl rumble 200 120 --ms 400
ds5ctl strength --rumble 4 --triggers 8  # 1 (12.5%) to 8 (full)
ds5ctl audio --output headphones --headphone-volume 90 --mic-mute on
ds5ctl test-sound                        # chime through the controller (USB)

ds5ctl power-off                         # Bluetooth only
ds5ctl rename "White"                    # name the controller
ds5ctl apply                             # send its saved profile
ds5ctl daemon                            # apply the profile on every connection
ds5ctl -d White gui --page triggers      # open the window on a controller and page
```

Trigger travel is split into zones 0 (released) to 9 (fully pressed). Run
`ds5ctl trigger left <effect> --help` for each effect's ranges. Effects: `off`,
`feedback`, `weapon`, `vibration`, `bow`, `galloping`, `machine`, `slope`,
`zones`.

With several controllers, pick one with `-d <name>`, `-d <MAC>` or
`-d /dev/hidrawN`; without `-d` commands use the first one found.

## Notes

- Steam Input also drives the controller while Steam runs and may override
  the lightbar and LEDs.
- Linux has no DualSense audio over Bluetooth, so the audio settings only do
  something over USB.

## Layout

- `crates/dualsense`: protocol library (device discovery, input parsing,
  output report and trigger effect encoding, Bluetooth CRC, motion sensor
  calibration and orientation tracking)
- `crates/ds5ctl`: the app: CLI, settings window (egui), per-controller
  config, daemon, BlueZ power off
- `dist/`: udev rule, desktop entry, systemd user unit

`DS5CTL_SCREENSHOT=out.ppm ds5ctl gui --page <page>` saves a picture of the
window and exits; handy for documentation.

## Credits

Report layouts follow the Linux `hid-playstation` driver, Nielk1's DualSense
reverse-engineering notes and trigger effect generator, and
[dualsensectl](https://github.com/nowrep/dualsensectl).
