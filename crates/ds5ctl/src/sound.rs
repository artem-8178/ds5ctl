//! A short chime played through a controller's own sound card, without
//! changing the default audio output.

use std::f32::consts::TAU;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use dualsense::DeviceInfo;

const RATE: u32 = 48_000;

pub fn play_test_sound(device: &DeviceInfo) -> Result<(), String> {
    let card = device
        .sound_card()
        .ok_or("this controller has no sound card here; connect it with a USB cable")?;
    let path = temp_file();
    fs::write(&path, chime_wav()).map_err(|e| format!("cannot write {}: {e}", path.display()))?;

    // Through PipeWire when it runs, so apps already playing on the
    // controller keep working; straight to ALSA otherwise.
    let result = match pipewire_sink(card) {
        Some(sink) => run(Command::new("pw-play").arg("--target").arg(sink).arg(&path)),
        None => run(Command::new("aplay")
            .arg("-q")
            .arg("-D")
            .arg(format!("plughw:{card},0"))
            .arg(&path)),
    };
    let _ = fs::remove_file(&path);
    result
}

fn temp_file() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("ds5ctl-chime-{}.wav", std::process::id()))
}

fn run(command: &mut Command) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Name of the PipeWire sink that plays on ALSA card `card`.
fn pipewire_sink(card: u32) -> Option<String> {
    let output = Command::new("pw-dump")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let objects: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
    objects.iter().find_map(|object| {
        let props = object.pointer("/info/props")?;
        let number = |key: &str| {
            let value = props.get(key)?;
            value.as_u64().or_else(|| value.as_str()?.parse().ok())
        };
        let on_card =
            number("alsa.card").or_else(|| number("api.alsa.pcm.card")) == Some(u64::from(card));
        let is_sink = props.get("media.class")?.as_str()? == "Audio/Sink";
        if on_card && is_sink {
            props.get("node.name")?.as_str().map(str::to_string)
        } else {
            None
        }
    })
}

/// Two bell-like notes (E5, then A5), 16-bit stereo WAV.
fn chime_wav() -> Vec<u8> {
    const NOTES: [(f32, f32); 2] = [(659.25, 0.0), (880.0, 0.16)];
    const LENGTH: f32 = 0.9;
    const FADE_OUT: f32 = 0.05;
    let frames = (RATE as f32 * LENGTH) as u32;

    let mut data = Vec::with_capacity(frames as usize * 4);
    for i in 0..frames {
        let t = i as f32 / RATE as f32;
        let mut value = 0.0;
        for (freq, start) in NOTES {
            if t >= start {
                let dt = t - start;
                // 5 ms attack, then an exponential ring-out.
                let envelope = (dt / 0.005).min(1.0) * (-dt * 5.0).exp();
                value += envelope * ((TAU * freq * dt).sin() + 0.3 * (TAU * 2.0 * freq * dt).sin());
            }
        }
        value *= ((LENGTH - t) / FADE_OUT).min(1.0);
        let sample = (value * 0.35 * f32::from(i16::MAX)) as i16;
        for _ in 0..2 {
            data.extend_from_slice(&sample.to_le_bytes());
        }
    }

    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&2u16.to_le_bytes()); // channels
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 4).to_le_bytes()); // bytes per second
    wav.extend_from_slice(&4u16.to_le_bytes()); // bytes per frame
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chime_is_a_valid_wav_without_clipping() {
        let wav = chime_wav();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
        assert_eq!(wav.len(), 44 + data_len);
        assert_eq!(
            u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize,
            wav.len() - 8
        );

        let samples: Vec<i16> = wav[44..]
            .chunks(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 10_000 && peak < 32_000, "peak {peak}");
        // Starts and ends silent, so there is no click.
        assert!(samples[0].unsigned_abs() < 100 && samples.last().unwrap().unsigned_abs() < 100);
    }
}
