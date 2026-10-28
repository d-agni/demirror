//! The look of the app (theme) and small pieces of UI shared by the pages.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Response, RichText, Sense, Stroke, TextStyle, Theme,
    Ui, Vec2, vec2,
};

pub const ACCENT: Color32 = Color32::from_rgb(64, 156, 255);
pub const OK: Color32 = Color32::from_rgb(52, 199, 120);
pub const WARN: Color32 = Color32::from_rgb(240, 170, 60);
pub const IDLE: Color32 = Color32::from_rgb(140, 146, 158);

/// demirror's theme: calm surfaces, one accent, roomier controls. Follows the system's
/// light or dark mode.
pub fn apply_theme(ctx: &egui::Context) {
    for theme in [Theme::Dark, Theme::Light] {
        ctx.style_mut_of(theme, |style| {
            let dark = theme == Theme::Dark;
            style.text_styles = [
                (TextStyle::Heading, FontId::new(24.0, FontFamily::Proportional)),
                (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
                (TextStyle::Button, FontId::new(14.5, FontFamily::Proportional)),
                (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
                (TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace)),
            ]
            .into();
            let s = &mut style.spacing;
            s.item_spacing = vec2(10.0, 8.0);
            s.button_padding = vec2(14.0, 7.0);
            s.interact_size.y = 30.0;
            s.window_margin = Margin::same(16);

            let v = &mut style.visuals;
            let (panel, card, field, line) = if dark {
                (rgb(0x14161b), rgb(0x1c1f26), rgb(0x0f1115), rgb(0x2a2e37))
            } else {
                (rgb(0xf4f5f8), rgb(0xffffff), rgb(0xf0f2f5), rgb(0xdfe2e8))
            };
            v.panel_fill = panel;
            v.window_fill = card;
            v.faint_bg_color = card;
            v.extreme_bg_color = field;
            v.selection.bg_fill = ACCENT.linear_multiply(if dark { 0.45 } else { 0.3 });
            v.selection.stroke = Stroke::new(1.0, if dark { Color32::WHITE } else { rgb(0x0b3d75) });
            v.hyperlink_color = ACCENT;
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, line);
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
                w.corner_radius = CornerRadius::same(8);
            }
            v.widgets.inactive.weak_bg_fill = if dark { rgb(0x272b34) } else { rgb(0xe8ebf0) };
            v.widgets.hovered.weak_bg_fill = if dark { rgb(0x323743) } else { rgb(0xdde1e8) };
            v.window_corner_radius = CornerRadius::same(12);
        });
    }
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// A rounded panel around a group of controls.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(12)
        .inner_margin(18)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A page's title and what it's for.
pub fn page_title(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).heading().strong());
    ui.label(RichText::new(subtitle).weak());
    ui.add_space(14.0);
}

/// A small heading inside a card.
pub fn section(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(16.0).strong());
    ui.add_space(2.0);
}

pub fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
    if color == OK {
        ui.painter().circle_stroke(rect.center(), 6.0, Stroke::new(2.0, color.linear_multiply(0.35)));
    }
}

/// A rounded label in `color`: a status, a count, a size.
pub fn pill(ui: &mut Ui, text: impl Into<String>, color: Color32) -> Response {
    egui::Frame::new()
        .fill(color.linear_multiply(0.16))
        .corner_radius(20)
        .inner_margin(Margin::symmetric(9, 3))
        .show(ui, |ui| ui.label(RichText::new(text).small().strong().color(color)))
        .response
}

/// The main action of a card: filled with the accent colour.
pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let button = egui::Button::new(RichText::new(text).strong().size(15.5).color(Color32::WHITE))
        .fill(if enabled { ACCENT } else { ACCENT.linear_multiply(0.4) })
        .corner_radius(10)
        .min_size(vec2(180.0, 40.0));
    ui.add_enabled(enabled, button)
}

/// A destructive or secondary action, outlined.
pub fn quiet_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(egui::Button::new(text).fill(Color32::TRANSPARENT).stroke(ui.visuals().widgets.noninteractive.bg_stroke))
}

/// An on/off switch with a label and, under it, what it does.
pub fn switch(ui: &mut Ui, on: &mut bool, label: &str, detail: &str) -> Response {
    ui.horizontal(|ui| {
        let size = vec2(38.0, 22.0);
        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
        if response.clicked() {
            *on = !*on;
            response.mark_changed();
        }
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let off_fill = ui.visuals().widgets.inactive.weak_bg_fill;
        let fill = lerp_color(off_fill, ACCENT, t);
        let painter = ui.painter();
        painter.rect_filled(rect, 11.0, fill);
        let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
        painter.circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE);
        ui.add_space(4.0);
        let text = ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let l = ui.add(egui::Label::new(label).sense(Sense::click()));
            if !detail.is_empty() {
                ui.label(RichText::new(detail).small().weak());
            }
            l
        });
        if text.inner.clicked() {
            *on = !*on;
            response.mark_changed();
        }
        response
    })
    .inner
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// A row of choices, one selected (`options`: value and label).
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(10)
        .inner_margin(3)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (option, label) in options {
                    let selected = *value == *option;
                    let text = RichText::new(*label);
                    let button = egui::Button::new(if selected { text.strong().color(Color32::WHITE) } else { text })
                        .fill(if selected { ACCENT } else { Color32::TRANSPARENT })
                        .corner_radius(8);
                    if ui.add(button).clicked() && !selected {
                        *value = *option;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// A short numbered step of instructions.
pub fn step(ui: &mut Ui, n: u32, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 11.0, ACCENT.linear_multiply(0.2));
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, n.to_string(), FontId::proportional(12.5), ACCENT);
        ui.label(text);
    });
}

/// A message across the top of a page, `color` for its kind; returns whether it was dismissed.
pub fn banner(ui: &mut Ui, text: &str, color: Color32) -> bool {
    let mut dismissed = false;
    egui::Frame::new()
        .fill(color.linear_multiply(0.14))
        .stroke(Stroke::new(1.0, color.linear_multiply(0.5)))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(text).color(ui.visuals().strong_text_color())).wrap());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    dismissed = ui.small_button("✕").clicked();
                });
            });
        });
    ui.add_space(10.0);
    dismissed
}

/// `text` as a QR code `size` points wide, dark on white (scannable in either theme).
pub fn qr_code(ui: &mut Ui, text: &str, size: f32) {
    let Ok(code) = qrcode::QrCode::new(text.as_bytes()) else { return };
    let modules = code.width();
    let colors = code.to_colors();
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 10.0, Color32::WHITE);
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
