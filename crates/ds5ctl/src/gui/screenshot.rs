//! `DS5CTL_SCREENSHOT=out.ppm ds5ctl gui`: saves the window once it has
//! settled and quits. Used to make README images.

use std::time::{Duration, Instant};

use eframe::egui;

const SETTLE: Duration = Duration::from_millis(1500);

pub struct Screenshot {
    path: String,
    started: Instant,
    requested: bool,
}

impl Screenshot {
    pub fn from_env() -> Option<Self> {
        let path = std::env::var("DS5CTL_SCREENSHOT").ok()?;
        Some(Screenshot {
            path,
            started: Instant::now(),
            requested: false,
        })
    }

    pub fn update(&mut self, ctx: &egui::Context) {
        if !self.requested {
            if self.started.elapsed() >= SETTLE {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                self.requested = true;
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            return;
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let mut ppm = format!("P6\n{} {}\n255\n", image.size[0], image.size[1]).into_bytes();
            for pixel in &image.pixels {
                ppm.extend_from_slice(&[pixel.r(), pixel.g(), pixel.b()]);
            }
            if let Err(e) = std::fs::write(&self.path, ppm) {
                eprintln!("ds5ctl: cannot write {}: {e}", self.path);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
