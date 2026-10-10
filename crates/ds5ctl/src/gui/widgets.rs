//! Custom painted widgets for the controller views.

use eframe::egui::{
    self, Color32, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2,
};

use dualsense::TriggerEffect;

/// Connection indicator colours.
pub const ONLINE: Color32 = Color32::from_rgb(60, 180, 90);
pub const OFFLINE: Color32 = Color32::from_rgb(120, 120, 120);
pub const FAULT: Color32 = Color32::from_rgb(220, 70, 60);

fn accent(ui: &egui::Ui) -> Color32 {
    ui.visuals().selection.bg_fill
}

fn track(ui: &egui::Ui) -> Color32 {
    ui.visuals().extreme_bg_color
}

fn outline(ui: &egui::Ui) -> Stroke {
    ui.visuals().widgets.noninteractive.bg_stroke
}

/// Analog stick position inside its circular range.
pub fn stick(ui: &mut egui::Ui, (x, y): (u8, u8), pressed: bool) {
    let size = 96.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let painter = ui.painter_at(rect);
    let center = rect.center();
    let radius = size / 2.0 - 2.0;
    painter.circle(center, radius, track(ui), outline(ui));
    painter.line_segment(
        [center - Vec2::X * radius, center + Vec2::X * radius],
        outline(ui),
    );
    painter.line_segment(
        [center - Vec2::Y * radius, center + Vec2::Y * radius],
        outline(ui),
    );
    let offset = Vec2::new(f32::from(x) - 128.0, f32::from(y) - 128.0) / 128.0 * (radius - 8.0);
    let fill = if pressed {
        ui.visuals().strong_text_color()
    } else {
        accent(ui)
    };
    painter.circle_filled(center + offset, 8.0, fill);
}

/// Strength (0-1) the effect applies in each of the 10 trigger zones, and
/// whether it vibrates rather than resists.
fn zone_levels(effect: &TriggerEffect) -> ([f32; 10], bool) {
    let mut levels = [0.0f32; 10];
    let mut span = |from: u8, to: u8, level: f32| {
        for zone in levels
            .iter_mut()
            .take(usize::from(to) + 1)
            .skip(usize::from(from))
        {
            *zone = level;
        }
    };
    let vibrates = match *effect {
        TriggerEffect::Off => false,
        TriggerEffect::Feedback { position, strength } => {
            span(position, 9, f32::from(strength) / 8.0);
            false
        }
        TriggerEffect::Weapon {
            start,
            end,
            strength,
        } => {
            span(start, end, f32::from(strength) / 8.0);
            false
        }
        TriggerEffect::Bow {
            start,
            end,
            strength,
            ..
        } => {
            span(start, end, f32::from(strength) / 8.0);
            false
        }
        TriggerEffect::Vibration {
            position,
            amplitude,
            ..
        } => {
            span(position, 9, f32::from(amplitude) / 8.0);
            true
        }
        TriggerEffect::Galloping { start, end, .. } => {
            span(start, end, 0.6);
            true
        }
        TriggerEffect::Machine {
            start,
            end,
            amplitude_a,
            amplitude_b,
            ..
        } => {
            span(
                start,
                end,
                f32::from(amplitude_a.max(amplitude_b) + 1) / 8.0,
            );
            true
        }
        TriggerEffect::MultiFeedback { strengths } => {
            for (zone, s) in levels.iter_mut().zip(strengths) {
                *zone = f32::from(s) / 8.0;
            }
            false
        }
        TriggerEffect::Slope {
            start,
            end,
            start_strength,
            end_strength,
        } => {
            for zone in start..10 {
                let t = f32::from(zone.min(end) - start) / f32::from(end - start);
                levels[usize::from(zone)] = (f32::from(start_strength)
                    + t * (f32::from(end_strength) - f32::from(start_strength)))
                    / 8.0;
            }
            false
        }
    };
    (levels, vibrates)
}

/// Trigger travel split into the 10 effect zones, shaded by how strongly the
/// effect acts there, with the live pressure drawn on top.
pub fn trigger_zones(ui: &mut egui::Ui, effect: &TriggerEffect, pressure: Option<u8>) {
    let width = ui.available_width().min(360.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 44.0), Sense::hover());
    let painter = ui.painter_at(rect.expand(1.0));
    let (levels, vibrates) = zone_levels(effect);
    let effect_color = if vibrates {
        Color32::from_rgb(230, 140, 40)
    } else {
        accent(ui)
    };

    let zone_width = rect.width() / 10.0;
    let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 28.0));
    painter.rect_filled(bar, CornerRadius::same(4), track(ui));
    for (i, level) in levels.iter().enumerate() {
        if *level <= 0.0 {
            continue;
        }
        let x = bar.left() + zone_width * i as f32;
        let height = bar.height() * level.clamp(0.1, 1.0);
        let zone = Rect::from_min_max(
            Pos2::new(x + 1.0, bar.bottom() - height),
            Pos2::new(x + zone_width - 1.0, bar.bottom()),
        );
        painter.rect_filled(
            zone,
            CornerRadius::same(2),
            effect_color.gamma_multiply(0.75),
        );
    }
    if let Some(p) = pressure {
        let x = bar.left() + bar.width() * f32::from(p) / 255.0;
        painter.line_segment(
            [
                Pos2::new(x, bar.top() - 2.0),
                Pos2::new(x, bar.bottom() + 2.0),
            ],
            Stroke::new(2.5, ui.visuals().strong_text_color()),
        );
    }
    painter.rect_stroke(bar, CornerRadius::same(4), outline(ui), StrokeKind::Inside);

    let font = egui::FontId::proportional(11.0);
    let color = ui.visuals().weak_text_color();
    for i in 0..10 {
        let center = Pos2::new(
            bar.left() + zone_width * (i as f32 + 0.5),
            bar.bottom() + 9.0,
        );
        painter.text(center, egui::Align2::CENTER_CENTER, i, font.clone(), color);
    }
}

/// Simple horizontal level meter for the analog triggers.
pub fn meter(ui: &mut egui::Ui, value: u8) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(140.0, 14.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(3), track(ui));
    let fill = Rect::from_min_size(
        rect.min,
        Vec2::new(rect.width() * f32::from(value) / 255.0, rect.height()),
    );
    painter.rect_filled(fill, CornerRadius::same(3), accent(ui));
}

/// The touchpad surface with up to two contact points.
pub fn touchpad(ui: &mut egui::Ui, touches: &[Option<(u16, u16)>; 2], pressed: bool) {
    let width = ui.available_width().min(320.0);
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(width, width * 1080.0 / 1920.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let fill = if pressed {
        accent(ui).gamma_multiply(0.35)
    } else {
        track(ui)
    };
    painter.rect_filled(rect, CornerRadius::same(8), fill);
    painter.rect_stroke(rect, CornerRadius::same(8), outline(ui), StrokeKind::Inside);
    for (x, y) in touches.iter().flatten() {
        let pos = rect.min
            + Vec2::new(
                f32::from(*x) / 1920.0 * rect.width(),
                f32::from(*y) / 1080.0 * rect.height(),
            );
        painter.circle_filled(pos, 9.0, accent(ui));
    }
}

/// The five player LEDs as clickable pills. Returns true when toggled.
pub fn player_leds(ui: &mut egui::Ui, mask: &mut u8) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        for i in 0..5 {
            let on = *mask & (1 << i) != 0;
            let (rect, response) = ui.allocate_exact_size(Vec2::new(30.0, 12.0), Sense::click());
            let fill = if on { Color32::WHITE } else { track(ui) };
            ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
            ui.painter()
                .rect_stroke(rect, CornerRadius::same(6), outline(ui), StrokeKind::Inside);
            if response.on_hover_text(format!("LED {}", i + 1)).clicked() {
                *mask ^= 1 << i;
                changed = true;
            }
        }
    });
    changed
}

/// Small filled circle used as a connection indicator.
pub fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

/// One clickable entry of the controller list: status dot, name, details.
pub fn controller_row(
    ui: &mut egui::Ui,
    name: &str,
    detail: &str,
    online: bool,
    selected: bool,
) -> egui::Response {
    // Reserve the background so it can be painted under the text once the
    // row's size is known.
    let background = ui.painter().add(egui::Shape::Noop);
    let response = ui
        .scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                status_dot(ui, if online { ONLINE } else { OFFLINE });
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add_space(3.0);
                    ui.label(RichText::new(name).strong());
                    ui.label(RichText::new(detail).small().weak());
                    ui.add_space(3.0);
                });
            });
        })
        .response;
    let fill = if selected {
        ui.visuals().selection.bg_fill.gamma_multiply(0.55)
    } else if response.hovered() {
        ui.visuals().widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().set(
        background,
        egui::epaint::RectShape::filled(response.rect, CornerRadius::same(4), fill),
    );
    response
}

/// A coloured square button used for colour presets.
pub fn swatch(ui: &mut egui::Ui, color: Color32, name: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    ui.painter().rect_filled(rect, CornerRadius::same(4), color);
    let stroke = if response.hovered() {
        ui.visuals().widgets.hovered.fg_stroke
    } else {
        outline(ui)
    };
    ui.painter()
        .rect_stroke(rect, CornerRadius::same(4), stroke, StrokeKind::Inside);
    response.on_hover_text(name).clicked()
}
