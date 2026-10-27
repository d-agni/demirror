//! Small pieces of UI shared by the pages.

use eframe::egui::{self, Color32, Sense, vec2};

pub const OK: Color32 = Color32::from_rgb(60, 190, 110);
pub const WARN: Color32 = Color32::from_rgb(235, 165, 60);

/// A rounded panel around a group of controls.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(10)
        .inner_margin(14)
        .show(ui, add)
        .inner
}

pub fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

/// `text` as a QR code `size` points wide, dark on white (scannable in either theme).
pub fn qr_code(ui: &mut egui::Ui, text: &str, size: f32) {
    let Ok(code) = qrcode::QrCode::new(text.as_bytes()) else { return };
    let modules = code.width();
    let colors = code.to_colors();
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 6.0, Color32::WHITE);
    // A quiet zone of two modules around the code.
    let cell = size / (modules + 4) as f32;
    let origin = rect.min + vec2(cell * 2.0, cell * 2.0);
    for (i, color) in colors.iter().enumerate() {
        if *color == qrcode::Color::Dark {
            let (x, y) = ((i % modules) as f32, (i / modules) as f32);
            let min = origin + vec2(x * cell, y * cell);
            painter.rect_filled(egui::Rect::from_min_size(min, vec2(cell + 0.5, cell + 0.5)), 0.0, Color32::BLACK);
        }
    }
}
