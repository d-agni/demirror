//! The Share page: run demirror's server on this computer, show how to reach it, manage its
//! monitors.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText, TextureHandle, TextureOptions, vec2};
use vmon_proto::{MonitorInfo, MonitorSpec, SourceSpec};
use vmon_server::{Config, OutputCommands, Server};

use crate::{
    net::{self, Target},
    settings::Settings,
    widgets,
};

/// A monitor to show in the app: where, which, and whether it was started from here.
pub type ViewRequest = (Target, u32, bool);

pub struct Host {
    server: Option<Server>,
    error: Option<String>,
    /// Whether this computer can add virtual displays, and the driver or why not.
    support: Arc<Mutex<Option<(bool, String)>>>,
    support_checked: Option<Instant>,
    thumbs: HashMap<u32, TextureHandle>,
    thumbs_at: Option<Instant>,
    copied_at: Option<Instant>,
    /// Installing the display driver (Windows): `None` while it runs, then why it failed.
    installing: Arc<Mutex<Option<Option<String>>>>,
}

impl Host {
    pub fn new() -> Host {
        Host {
            server: None,
            error: None,
            support: Arc::default(),
            support_checked: None,
            thumbs: HashMap::new(),
            thumbs_at: None,
            copied_at: None,
            installing: Arc::default(),
        }
    }

    pub fn running(&self) -> bool {
        self.server.is_some()
    }

    pub fn start(&mut self, settings: &Settings) {
        let config = Config {
            bind: [0, 0, 0, 0].into(),
            port: settings.port,
            token: settings.require_token.then(|| settings.token.trim().to_string()).filter(|t| !t.is_empty()),
            commands: OutputCommands::default(),
            advertise: settings.announce,
            allow_input: settings.allow_input,
        };
        match Server::start(config) {
            Ok(server) => {
                self.server = Some(server);
                self.error = None;
            }
            Err(e) => self.error = Some(format!("Couldn't start sharing: {e:#}")),
        }
    }

    pub fn stop(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop();
        }
        self.thumbs.clear();
    }

    /// Devices may (or may not) control this computer from now on.
    pub fn set_allow_input(&self, allow: bool) {
        if let Some(server) = &self.server {
            server.manager.set_allow_input(allow);
        }
    }

    pub fn monitor_count(&self) -> usize {
        self.server.as_ref().map_or(0, |s| s.manager.list().len())
    }

    /// `view`: set to a monitor of this host to show in the app.
    pub fn ui(&mut self, ui: &mut egui::Ui, settings: &mut Settings, view: &mut Option<ViewRequest>) {
        self.check_support(ui.ctx());
        widgets::page_title(ui, "Share this computer", "Phones, tablets and other computers become extra monitors for this desktop.");
        if let Some(e) = &self.error
            && widgets::banner(ui, e, ui.visuals().error_fg_color)
        {
            self.error = None;
        }

        widgets::card(ui, |ui| self.sharing_ui(ui, settings));
        ui.add_space(12.0);
        self.status_row(ui, settings);

        if let Some(server) = &self.server {
            // Monitors come and go from other devices: look again every second.
            ui.ctx().request_repaint_after(Duration::from_secs(1));
            ui.add_space(18.0);
            let manager = server.manager.clone();
            let local = Target { base: format!("http://127.0.0.1:{}", server.port), token: server.token.clone() };
            self.monitors_ui(ui, &manager, &local, view);
        }
    }

    fn sharing_ui(&mut self, ui: &mut egui::Ui, settings: &mut Settings) {
        let Some(server) = &self.server else {
            ui.horizontal(|ui| {
                ui.label(RichText::new("🖥").size(40.0));
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    widgets::section(ui, "Not sharing");
                    ui.label(RichText::new("Start sharing, then open the address it shows on the other device. No app is needed there; a browser will do.").weak());
                });
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let ready = !matches!(&*self.support.lock().unwrap(), Some((false, _)));
                if widgets::primary_button(ui, "Start sharing", true).clicked() {
                    self.start(settings);
                }
                if !ready {
                    ui.label(RichText::new("Monitors would only mirror: virtual displays aren't set up (see below).").small().weak());
                }
            });
            return;
        };

        let url = server.share_url();
        let mut stop = false;
        ui.horizontal_top(|ui| {
            widgets::qr_code(ui, &url, 176.0);
            ui.add_space(18.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    widgets::status_dot(ui, widgets::OK);
                    widgets::section(ui, "Sharing");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        stop = widgets::quiet_button(ui, "Stop sharing").clicked();
                    });
                });
                ui.add_space(6.0);
                widgets::step(ui, 1, "Put the other device on the same Wi-Fi or network.");
                widgets::step(ui, 2, "Scan the code with its camera, or open this address:");
                ui.horizontal(|ui| {
                    ui.add_space(32.0);
                    egui::Frame::new()
                        .fill(ui.visuals().extreme_bg_color)
                        .corner_radius(8)
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| ui.label(RichText::new(&url).monospace().size(15.0).strong()));
                    let copied = self.copied_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2));
                    if ui.button(if copied { "Copied ✔" } else { "Copy" }).clicked() {
                        ui.ctx().copy_text(url.clone());
                        self.copied_at = Some(Instant::now());
                        ui.ctx().request_repaint_after(Duration::from_secs(2));
                    }
                });
                widgets::step(ui, 3, "Tap Start monitor. It's sized to fit that screen.");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(32.0);
                    ui.label(RichText::new("Another computer with demirror finds this one on its Use as monitor page.").small().weak());
                });
                if let Some(token) = &server.token {
                    ui.horizontal(|ui| {
                        ui.add_space(32.0);
                        ui.label(RichText::new(format!("Devices need the token {token} (it's in the code and the address).")).small().weak());
                    });
                }
            });
        });
        if stop {
            self.stop();
        }
    }

    /// Chips under the sharing card: virtual displays, control, discovery.
    fn status_row(&mut self, ui: &mut egui::Ui, settings: &mut Settings) {
        let support = self.support.lock().unwrap().clone();
        widgets::card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                match &support {
                    None => {
                        ui.spinner();
                        ui.label("Checking virtual displays…");
                    }
                    Some((true, detail)) => {
                        widgets::pill(ui, "✔ Virtual displays ready", widgets::OK).on_hover_text(detail);
                    }
                    Some((false, detail)) => {
                        widgets::pill(ui, "⚠ Virtual displays unavailable", widgets::WARN).on_hover_text(detail);
                    }
                }
                let (text, color) = if settings.announce { ("Found on the network", widgets::ACCENT) } else { ("Hidden from the network", widgets::IDLE) };
                widgets::pill(ui, text, color).on_hover_text("Change in Settings");
                if settings.require_token {
                    widgets::pill(ui, "🔒 Token needed", widgets::ACCENT);
                }
            });
            if let Some((false, detail)) = &support {
                ui.add_space(6.0);
                ui.label(RichText::new(detail).small().weak());
                if cfg!(target_os = "linux") {
                    ui.label("Install the display service once (it plugs monitors into the kernel):");
                    ui.label(RichText::new("sudo packaging/linux/install-linux.sh").monospace());
                } else if cfg!(windows) {
                    self.driver_ui(ui);
                } else {
                    ui.label("On this system demirror can mirror screens; extending the desktop needs a virtual display driver.");
                }
            }
            ui.add_space(8.0);
            let control = widgets::switch(
                ui,
                &mut settings.allow_input,
                "Let devices control this computer",
                "Touch, mouse and keyboard on a monitor work on this desktop.",
            );
            if control.changed() {
                self.set_allow_input(settings.allow_input);
            }
        });
    }

    /// Installing the display driver, on Windows.
    fn driver_ui(&mut self, ui: &mut egui::Ui) {
        #[cfg(windows)]
        {
            if !crate::driver::available() {
                ui.label("This copy of demirror came without its display driver; reinstall it from the release to extend the desktop.");
                return;
            }
            ui.label("Install demirror's display driver once, and devices extend this desktop like real monitors.");
            let state = self.installing.lock().unwrap().clone();
            ui.horizontal(|ui| match state {
                Some(None) => {
                    ui.spinner();
                    ui.label("Installing… (Windows asks for permission)");
                }
                _ => {
                    if widgets::primary_button(ui, "Install display driver", true).clicked() {
                        *self.installing.lock().unwrap() = Some(None);
                        let (installing, ctx) = (self.installing.clone(), ui.ctx().clone());
                        thread::spawn(move || {
                            let result = crate::driver::install_elevated();
                            *installing.lock().unwrap() = match result {
                                Ok(()) => None,
                                Err(e) => Some(Some(format!("{e:#}"))),
                            };
                            ctx.request_repaint();
                        });
                        // Look again once it's done.
                        self.support_checked = None;
                    }
                }
            });
            if let Some(Some(error)) = state {
                ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
            }
        }
        #[cfg(not(windows))]
        let _ = ui;
    }

    fn check_support(&mut self, ctx: &egui::Context) {
        if matches!(*self.installing.lock().unwrap(), Some(None)) {
            // Installing: check again when it's done.
            self.support_checked = None;
            return;
        }
        if self.support_checked.is_some_and(|t| t.elapsed() < Duration::from_secs(10)) {
            return;
        }
        self.support_checked = Some(Instant::now());
        let (support, ctx) = (self.support.clone(), ctx.clone());
        thread::spawn(move || {
            *support.lock().unwrap() = Some(vmon_server::source::virtual_support());
            ctx.request_repaint();
        });
    }

    fn monitors_ui(
        &mut self,
        ui: &mut egui::Ui,
        manager: &vmon_server::MonitorManager,
        local: &Target,
        view: &mut Option<ViewRequest>,
    ) {
        let monitors = manager.list();
        self.refresh_thumbs(ui.ctx(), manager, &monitors);
        ui.horizontal(|ui| {
            widgets::section(ui, "Monitors");
            widgets::pill(ui, monitors.len().to_string(), widgets::ACCENT);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let test = widgets::quiet_button(ui, "＋ Test monitor").on_hover_text("An animated pattern, to try a device");
                if test.clicked() {
                    let spec = MonitorSpec { name: Some("Test pattern".into()), ..Default::default() };
                    if let Err(e) = manager.create(spec) {
                        self.error = Some(format!("{e:#}"));
                    }
                }
            });
        });
        ui.add_space(8.0);
        if monitors.is_empty() {
            widgets::card(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new("📱  💻").size(30.0));
                    ui.label(RichText::new("No monitors yet").strong());
                    ui.label(RichText::new("They appear here when a device starts one, and go when it disconnects.").weak());
                    ui.add_space(10.0);
                });
            });
            return;
        }
        let mut close = None;
        let width = 240.0;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(12.0, 12.0);
            for m in &monitors {
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                    .corner_radius(12)
                    .inner_margin(10)
                    .show(ui, |ui| ui.vertical(|ui| {
                        ui.set_width(width);
                        let height = width * 9.0 / 16.0;
                        let (rect, _) = ui.allocate_exact_size(vec2(width, height), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 8.0, Color32::from_gray(12));
                        match self.thumbs.get(&m.id) {
                            Some(texture) => {
                                let size = texture.size_vec2();
                                let scale = (rect.width() / size.x).min(rect.height() / size.y);
                                let shown = egui::Rect::from_center_size(rect.center(), size * scale);
                                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                                ui.painter().image(texture.id(), shown, uv, Color32::WHITE);
                            }
                            None => {
                                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "No picture yet", egui::FontId::proportional(13.0), Color32::GRAY);
                            }
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            widgets::status_dot(ui, if m.viewers > 0 { widgets::OK } else { widgets::IDLE });
                            ui.add(egui::Label::new(RichText::new(&m.name).strong()).truncate());
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            widgets::pill(ui, format!("{} × {}", m.width, m.height), widgets::IDLE);
                            widgets::pill(ui, kind(m), widgets::IDLE);
                            let viewers = match m.viewers {
                                0 => "not viewed".to_string(),
                                1 => "1 viewer".to_string(),
                                n => format!("{n} viewers"),
                            };
                            widgets::pill(ui, viewers, if m.viewers > 0 { widgets::OK } else { widgets::IDLE });
                        });
                        ui.horizontal(|ui| {
                            if ui.button("View here").on_hover_text("Show it in this window").clicked() {
                                *view = Some((local.clone(), m.id, false));
                            }
                            if widgets::quiet_button(ui, "Close").on_hover_text("Unplug it; its windows move back").clicked() {
                                close = Some(m.id);
                            }
                        });
                    }));
            }
        });
        if let Some(id) = close {
            manager.close(id);
            self.thumbs.remove(&id);
        }
    }

    /// A still of each monitor, once a second (its latest frame; nothing extra is captured).
    fn refresh_thumbs(&mut self, ctx: &egui::Context, manager: &vmon_server::MonitorManager, monitors: &[MonitorInfo]) {
        if self.thumbs_at.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.thumbs_at = Some(Instant::now());
        self.thumbs.retain(|id, _| monitors.iter().any(|m| m.id == *id));
        for m in monitors {
            let Some((_, rx)) = manager.subscribe(m.id) else { continue };
            let jpeg = rx.borrow().clone();
            drop(rx);
            let Some(image) = jpeg.and_then(|j| net::decode(&j).ok()) else { continue };
            match self.thumbs.get_mut(&m.id) {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => {
                    self.thumbs.insert(m.id, ctx.load_texture(format!("thumb-{}", m.id), image, TextureOptions::LINEAR));
                }
            }
        }
    }
}

fn kind(m: &MonitorInfo) -> String {
    match &m.source {
        SourceSpec::Virtual => "extends the desktop".to_string(),
        SourceSpec::Synthetic => "test pattern".to_string(),
        SourceSpec::Screen { output } => format!("mirrors {}", output.as_deref().unwrap_or("a screen")),
    }
}
