//! Showing a monitor: full screen, scaled to fit, following this screen's size when the
//! monitor is a virtual display.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use eframe::egui::{self, Color32, Rect, TextureHandle, TextureOptions, pos2, vec2};
use tungstenite::{Bytes, Message, stream::MaybeTlsStream};
use vmon_proto::{
    MonitorInfo, SourceSpec,
    input::{Button, ClientMessage, InputEvent},
};

use crate::net::{self, Target};

/// How long the viewer's size has to stay put before the host is told (a window being
/// dragged bigger, a phone turning).
const SIZE_SETTLE: Duration = Duration::from_millis(800);
/// The toolbar hides after this long without the pointer moving.
const TOOLBAR_FOR: Duration = Duration::from_secs(3);
const TOOLBAR_HEIGHT: f32 = 44.0;

pub enum Action {
    Stay,
    Leave,
}

pub struct Viewer {
    target: Target,
    pub info: MonitorInfo,
    /// Started from here: closed when the viewer leaves.
    own: bool,
    latest: Arc<Mutex<Option<Bytes>>>,
    ended: Arc<Mutex<Option<String>>>,
    /// Input for the host, sent by the stream's thread.
    input: mpsc::Sender<InputEvent>,
    /// Keys held down here (released if the viewer leaves meanwhile).
    held: HashSet<String>,
    /// Where the picture was drawn last, for turning pointer positions into its fractions.
    shown: Rect,
    texture: Option<TextureHandle>,
    frame_size: [usize; 2],
    frames: u32,
    fps: u32,
    since: Instant,
    /// Size last told to the host, and a new one waiting to settle.
    sent_size: Option<[u32; 2]>,
    new_size: Option<([u32; 2], Instant)>,
    /// The longest side the monitor may have (the quality preset's).
    max_side: u32,
    active_at: Instant,
}

impl Viewer {
    pub fn open(target: Target, id: u32, own: bool, max_side: u32, ctx: &egui::Context) -> Result<Viewer> {
        let (info, mut socket) = target.connect(id)?;
        // Short reads, so the same thread also sends input without waiting for a frame.
        if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream.set_read_timeout(Some(Duration::from_millis(8)))?;
        }
        let latest: Arc<Mutex<Option<Bytes>>> = Arc::default();
        let ended: Arc<Mutex<Option<String>>> = Arc::default();
        let (input, outgoing) = mpsc::channel::<InputEvent>();
        {
            let (latest, ended, ctx) = (latest.clone(), ended.clone(), ctx.clone());
            thread::spawn(move || {
                let reason = loop {
                    for event in outgoing.try_iter() {
                        let Ok(text) = serde_json::to_string(&ClientMessage { input: event }) else { continue };
                        if let Err(e) = socket.send(Message::Text(text.into())) {
                            tracing::warn!("sending input: {e}");
                        }
                    }
                    match socket.read() {
                        Ok(Message::Binary(jpeg)) => {
                            *latest.lock().unwrap() = Some(jpeg);
                            ctx.request_repaint();
                        }
                        Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => {
                            break "The monitor was closed.".to_string();
                        }
                        Ok(_) => {}
                        Err(tungstenite::Error::Io(e))
                            if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                        Err(e) => break format!("Connection lost: {e}"),
                    }
                };
                *ended.lock().unwrap() = Some(reason);
                ctx.request_repaint();
            });
        }
        Ok(Viewer {
            target,
            info,
            own,
            latest,
            ended,
            input,
            held: HashSet::new(),
            shown: Rect::NOTHING,
            texture: None,
            frame_size: [0, 0],
            frames: 0,
            fps: 0,
            since: Instant::now(),
            sent_size: None,
            new_size: None,
            max_side,
            active_at: Instant::now(),
        })
    }

    /// Ends viewing; a monitor started from here is closed (on a thread, the window doesn't wait).
    pub fn leave(mut self) {
        self.release_keys();
        if self.own {
            let (target, id) = (self.target.clone(), self.info.id);
            thread::spawn(move || {
                if let Err(e) = target.close(id) {
                    tracing::warn!("closing monitor {id}: {e:#}");
                }
            });
        }
    }

    /// `leave`, but waits for it (the app is quitting).
    pub fn leave_now(self) {
        if self.own {
            let _ = self.target.close(self.info.id);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Action {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        self.take_frame(&ctx);
        if now.duration_since(self.since) >= Duration::from_secs(1) {
            (self.fps, self.frames, self.since) = (self.frames, 0, now);
        }
        ctx.request_repaint_after(Duration::from_millis(500));

        let rect = ui.max_rect();
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, Color32::BLACK);
        if let Some(texture) = &self.texture {
            let [w, h] = self.frame_size;
            let scale = (rect.width() / w as f32).min(rect.height() / h as f32);
            let shown = Rect::from_center_size(rect.center(), vec2(w as f32 * scale, h as f32 * scale));
            self.shown = shown;
            painter.image(texture.id(), shown, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }

        self.follow_size(&ctx, rect, now);

        let response = ui.interact(rect, ui.scope_id().with("monitor"), egui::Sense::click());
        let (moved, escape) = ctx.input(|i| (i.pointer.delta() != egui::Vec2::ZERO, i.key_pressed(egui::Key::Escape)));
        if moved || response.clicked() {
            self.active_at = now;
        }
        // With control, clicks and Esc belong to the host; leave with the toolbar.
        let control = self.info.input;
        let toolbar_shown = now.duration_since(self.active_at) < TOOLBAR_FOR;
        if control {
            let toolbar = toolbar_shown.then(|| Rect::from_min_size(rect.min, vec2(rect.width(), TOOLBAR_HEIGHT)));
            self.forward_input(&ctx, toolbar);
            if response.hovered() && toolbar.is_none_or(|t| !ctx.pointer_hover_pos().is_some_and(|p| t.contains(p))) {
                ctx.set_cursor_icon(egui::CursorIcon::None);
            }
        } else {
            if response.double_clicked() {
                toggle_fullscreen(&ctx);
            }
            if escape {
                return Action::Leave;
            }
        }

        let ended = self.ended.lock().unwrap().clone();
        if let Some(reason) = ended {
            return self.ended_ui(ui, rect, &reason);
        }
        if self.texture.is_none() {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Waiting for the first frame…", egui::FontId::proportional(18.0), Color32::GRAY);
        }
        if now.duration_since(self.active_at) < TOOLBAR_FOR {
            ctx.request_repaint_after(TOOLBAR_FOR);
            return self.toolbar(ui, rect);
        }
        Action::Stay
    }

    fn take_frame(&mut self, ctx: &egui::Context) {
        let Some(jpeg) = self.latest.lock().unwrap().take() else { return };
        match net::decode(&jpeg) {
            Ok(image) => {
                self.frame_size = image.size;
                match &mut self.texture {
                    Some(texture) => texture.set(image, TextureOptions::LINEAR),
                    None => self.texture = Some(ctx.load_texture("monitor", image, TextureOptions::LINEAR)),
                }
                self.frames += 1;
            }
            Err(e) => tracing::warn!("bad frame: {e:#}"),
        }
    }

    /// A virtual display is asked to match this window (its shape; its pixels, up to the
    /// preset's longest side).
    fn follow_size(&mut self, ctx: &egui::Context, rect: Rect, now: Instant) {
        if self.info.source != SourceSpec::Virtual {
            return;
        }
        let ppp = ctx.pixels_per_point();
        let size = crate::settings::fit([(rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32], self.max_side);
        if size[0] < 16 || size[1] < 16 || Some(size) == self.sent_size {
            self.new_size = None;
            return;
        }
        match self.new_size {
            Some((pending, since)) if pending == size => {
                if now.duration_since(since) >= SIZE_SETTLE {
                    self.sent_size = Some(size);
                    self.new_size = None;
                    let (target, id) = (self.target.clone(), self.info.id);
                    thread::spawn(move || {
                        if let Err(e) = target.resize(id, size[0], size[1]) {
                            tracing::warn!("telling the host our size: {e:#}");
                        }
                    });
                } else {
                    ctx.request_repaint_after(SIZE_SETTLE);
                }
            }
            _ => {
                self.new_size = Some((size, now));
                ctx.request_repaint_after(SIZE_SETTLE);
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, rect: Rect) -> Action {
        let mut action = Action::Stay;
        let bar = Rect::from_min_size(rect.min, vec2(rect.width(), TOOLBAR_HEIGHT));
        ui.painter().rect_filled(bar, 0.0, Color32::from_black_alpha(180));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(14.0, 8.0))));
        child.horizontal_centered(|ui| {
            ui.visuals_mut().override_text_color = Some(Color32::from_gray(230));
            let [w, h] = self.frame_size;
            ui.label(egui::RichText::new(&self.info.name).strong());
            ui.label(format!("{w} × {h} · {} fps", self.fps));
            if self.info.input {
                ui.label(egui::RichText::new("· controlling").color(Color32::from_rgb(120, 200, 140)));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let leave = if self.own { "Disconnect" } else { "Leave" };
                if ui.button(leave).on_hover_text("Esc").clicked() {
                    action = Action::Leave;
                }
                let fullscreen = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
                if ui.button(if fullscreen { "Window" } else { "Full screen" }).on_hover_text("Double-click").clicked() {
                    toggle_fullscreen(ui.ctx());
                }
            });
        });
        action
    }

    fn ended_ui(&mut self, ui: &mut egui::Ui, rect: Rect, reason: &str) -> Action {
        let mut action = Action::Stay;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.vertical_centered(|ui| {
            ui.add_space(rect.height() / 2.0 - 40.0);
            ui.label(egui::RichText::new(reason).size(18.0).color(Color32::GRAY));
            ui.add_space(12.0);
            if ui.button("Back").clicked() {
                action = Action::Leave;
            }
        });
        action
    }
}

impl Viewer {
    /// This window's pointer, buttons, wheel and keys, for the host. Not what happens on the
    /// toolbar while it's shown.
    fn forward_input(&mut self, ctx: &egui::Context, toolbar: Option<Rect>) {
        if self.shown.width() <= 0.0 {
            return;
        }
        let on_toolbar = |p: egui::Pos2| toolbar.is_some_and(|t| t.contains(p));
        let events = ctx.input(|i| i.events.clone());
        for event in events {
            let input = match event {
                egui::Event::PointerMoved(p) if !on_toolbar(p) => Some(self.move_to(p)),
                egui::Event::PointerButton { pos, button, pressed, .. } if !on_toolbar(pos) || !pressed => {
                    let button = match button {
                        egui::PointerButton::Primary => Button::Left,
                        egui::PointerButton::Secondary => Button::Right,
                        egui::PointerButton::Middle => Button::Middle,
                        _ => continue,
                    };
                    let _ = self.input.send(self.move_to(pos));
                    Some(InputEvent::Button { button, down: pressed })
                }
                egui::Event::MouseWheel { unit, delta, .. } => {
                    // To 1/120 notch units; egui's delta is where the content moves.
                    let per = match unit {
                        egui::MouseWheelUnit::Point => 1.2,
                        egui::MouseWheelUnit::Line => 120.0,
                        egui::MouseWheelUnit::Page => 1200.0,
                    };
                    let (dx, dy) = ((-delta.x * per).round() as i32, (-delta.y * per).round() as i32);
                    (dx != 0 || dy != 0).then_some(InputEvent::Wheel { dx, dy })
                }
                egui::Event::Key { key, physical_key, pressed, repeat: false, .. } => {
                    key_code(physical_key.unwrap_or(key)).map(|code| {
                        if pressed {
                            self.held.insert(code.clone());
                        } else {
                            self.held.remove(&code);
                        }
                        InputEvent::Key { code, down: pressed }
                    })
                }
                egui::Event::WindowFocused(false) => {
                    self.release_keys();
                    None
                }
                _ => None,
            };
            if let Some(input) = input {
                let _ = self.input.send(input);
            }
        }
    }

    fn move_to(&self, p: egui::Pos2) -> InputEvent {
        let x = ((p.x - self.shown.min.x) / self.shown.width()).clamp(0.0, 1.0);
        let y = ((p.y - self.shown.min.y) / self.shown.height()).clamp(0.0, 1.0);
        InputEvent::Move { x, y }
    }

    fn release_keys(&mut self) {
        for code in self.held.drain() {
            let _ = self.input.send(InputEvent::Key { code, down: false });
        }
    }
}

/// An egui key as the web's `KeyboardEvent.code` (the physical key), as the host expects.
fn key_code(key: egui::Key) -> Option<String> {
    let name = format!("{key:?}");
    let code = match name.as_str() {
        n if n.len() == 1 && n.as_bytes()[0].is_ascii_uppercase() => format!("Key{n}"),
        n if n.starts_with("Num") && n.len() == 4 => format!("Digit{}", &n[3..]),
        "OpenBracket" => "BracketLeft".into(),
        "CloseBracket" => "BracketRight".into(),
        "Backtick" => "Backquote".into(),
        "Equals" => "Equal".into(),
        "SuperLeft" => "MetaLeft".into(),
        "SuperRight" => "MetaRight".into(),
        "Menu" => "ContextMenu".into(),
        n => n.to_string(),
    };
    vmon_proto::input::evdev_key(&code).map(|_| code)
}

fn toggle_fullscreen(ctx: &egui::Context) {
    let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_for_the_host() {
        assert_eq!(key_code(egui::Key::A).as_deref(), Some("KeyA"));
        assert_eq!(key_code(egui::Key::Num7).as_deref(), Some("Digit7"));
        assert_eq!(key_code(egui::Key::F5).as_deref(), Some("F5"));
        assert_eq!(key_code(egui::Key::Enter).as_deref(), Some("Enter"));
        assert_eq!(key_code(egui::Key::OpenBracket).as_deref(), Some("BracketLeft"));
        assert_eq!(key_code(egui::Key::SuperLeft).as_deref(), Some("MetaLeft"));
        assert_eq!(key_code(egui::Key::ShiftLeft).as_deref(), Some("ShiftLeft"));
        assert_eq!(key_code(egui::Key::Copy), None);
    }
}
