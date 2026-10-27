//! The Host page: run demirror's server on this computer, show how to reach it, manage its
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
    widgets,
};

pub struct Host {
    server: Option<Server>,
    port: u16,
    require_token: bool,
    token: String,
    announce: bool,
    error: Option<String>,
    /// Whether this computer can add virtual displays, and the driver or why not.
    support: Arc<Mutex<Option<(bool, String)>>>,
    support_checked: Option<Instant>,
    thumbs: HashMap<u32, TextureHandle>,
    thumbs_at: Option<Instant>,
}

impl Host {
    pub fn new() -> Host {
        Host {
            server: None,
            port: vmon_proto::DEFAULT_PORT,
            require_token: false,
            token: net::new_token(),
            announce: true,
            error: None,
            support: Arc::default(),
            support_checked: None,
            thumbs: HashMap::new(),
            thumbs_at: None,
        }
    }

    pub fn running(&self) -> bool {
        self.server.is_some()
    }

    pub fn start(&mut self) {
        let config = Config {
            bind: [0, 0, 0, 0].into(),
            port: self.port,
            token: self.require_token.then(|| self.token.trim().to_string()).filter(|t| !t.is_empty()),
            commands: OutputCommands::default(),
            advertise: self.announce,
        };
        match Server::start(config) {
            Ok(server) => {
                self.server = Some(server);
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    pub fn stop(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop();
        }
        self.thumbs.clear();
    }

    /// `view`: set to a monitor of this host to show in the app.
    pub fn ui(&mut self, ui: &mut egui::Ui, view: &mut Option<(Target, u32, bool)>) {
        self.check_support(ui.ctx());
        ui.heading("Share this computer's desktop");
        ui.label("Other devices on your network become extra monitors for this computer.");
        ui.add_space(12.0);

        widgets::card(ui, |ui| self.server_ui(ui));
        ui.add_space(12.0);
        widgets::card(ui, |ui| self.support_ui(ui));
        if let Some(server) = &self.server {
            // Monitors come and go from other devices: look again every second.
            ui.ctx().request_repaint_after(Duration::from_secs(1));
            ui.add_space(12.0);
            let manager = server.manager.clone();
            let local = Target { base: format!("http://127.0.0.1:{}", server.port), token: server.token.clone() };
            widgets::card(ui, |ui| self.monitors_ui(ui, &manager, &local, view));
        }
    }

    fn server_ui(&mut self, ui: &mut egui::Ui) {
        match &self.server {
            None => {
                ui.horizontal(|ui| {
                    widgets::status_dot(ui, Color32::GRAY);
                    ui.label(RichText::new("Not sharing").strong());
                });
                ui.add_space(8.0);
                egui::Grid::new("host-settings").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label("Port");
                    ui.add(egui::DragValue::new(&mut self.port).range(1024..=65535));
                    ui.end_row();
                    ui.label("Token");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.require_token, "Ask devices for a token");
                        ui.add_enabled(self.require_token, egui::TextEdit::singleline(&mut self.token).desired_width(150.0));
                        if ui.add_enabled(self.require_token, egui::Button::new("New")).clicked() {
                            self.token = net::new_token();
                        }
                    });
                    ui.end_row();
                    ui.label("Network");
                    ui.checkbox(&mut self.announce, "Let devices find this computer");
                    ui.end_row();
                });
                ui.add_space(8.0);
                if ui.add(egui::Button::new(RichText::new("Start sharing").strong()).min_size(vec2(160.0, 34.0))).clicked() {
                    self.start();
                }
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            }
            Some(server) => {
                let url = server.share_url();
                let mut stop = false;
                ui.horizontal(|ui| {
                    widgets::status_dot(ui, widgets::OK);
                    ui.label(RichText::new(format!("Sharing on port {}", server.port)).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        stop = ui.button("Stop sharing").clicked();
                    });
                });
                ui.add_space(8.0);
                ui.horizontal_top(|ui| {
                    widgets::qr_code(ui, &url, 150.0);
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.label("On a phone or tablet, scan the code or open:");
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&url).monospace().size(15.0));
                            if ui.small_button("Copy").clicked() {
                                ui.ctx().copy_text(url.clone());
                            }
                        });
                        ui.add_space(4.0);
                        ui.label("Then tap Start monitor. Its resolution is detected from that screen.");
                        ui.label("Another computer with demirror finds this one under Connect.");
                        ui.add_space(4.0);
                        ui.hyperlink_to("Open the dashboard in a browser", &url);
                    });
                });
                if stop {
                    self.stop();
                }
            }
        }
    }

    fn check_support(&mut self, ctx: &egui::Context) {
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

    fn support_ui(&mut self, ui: &mut egui::Ui) {
        let support = self.support.lock().unwrap().clone();
        ui.horizontal(|ui| match &support {
            None => {
                ui.spinner();
                ui.label("Checking virtual displays…");
            }
            Some((true, detail)) => {
                widgets::status_dot(ui, widgets::OK);
                ui.label(RichText::new("Virtual displays ready").strong());
                ui.label(RichText::new(detail).weak());
            }
            Some((false, detail)) => {
                widgets::status_dot(ui, widgets::WARN);
                ui.label(RichText::new("Virtual displays unavailable").strong());
                ui.label(RichText::new(detail).weak());
            }
        });
        if let Some((false, _)) = &support {
            ui.add_space(4.0);
            if cfg!(target_os = "linux") {
                ui.label("Install the display service once (it plugs monitors into the kernel):");
                ui.label(RichText::new("sudo packaging/linux/install-linux.sh").monospace());
            } else {
                ui.label("On this system demirror can mirror screens; extending the desktop needs a virtual display driver.");
            }
        }
    }

    fn monitors_ui(
        &mut self,
        ui: &mut egui::Ui,
        manager: &vmon_server::MonitorManager,
        local: &Target,
        view: &mut Option<(Target, u32, bool)>,
    ) {
        let monitors = manager.list();
        self.refresh_thumbs(ui.ctx(), manager, &monitors);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Monitors").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Add test monitor").on_hover_text("An animated pattern, to try a device").clicked() {
                    let spec = MonitorSpec { name: Some("Test pattern".into()), ..Default::default() };
                    if let Err(e) = manager.create(spec) {
                        self.error = Some(format!("{e:#}"));
                    }
                }
            });
        });
        ui.add_space(6.0);
        if monitors.is_empty() {
            ui.label(RichText::new("None yet. Start one from a phone, tablet or another computer.").weak());
            return;
        }
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for m in &monitors {
                widgets::card(ui, |ui| {
                    ui.set_width(220.0);
                    let (rect, _) = ui.allocate_exact_size(vec2(220.0, 138.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 6.0, Color32::BLACK);
                    if let Some(texture) = self.thumbs.get(&m.id) {
                        let size = texture.size_vec2();
                        let scale = (rect.width() / size.x).min(rect.height() / size.y);
                        let shown = egui::Rect::from_center_size(rect.center(), size * scale);
                        ui.painter().image(texture.id(), shown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    ui.label(RichText::new(&m.name).strong());
                    ui.label(RichText::new(describe(m)).weak().small());
                    ui.horizontal(|ui| {
                        if ui.button("View here").clicked() {
                            *view = Some((local.clone(), m.id, false));
                        }
                        if ui.button("Close").clicked() {
                            close = Some(m.id);
                        }
                    });
                });
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

fn describe(m: &MonitorInfo) -> String {
    let kind = match &m.source {
        SourceSpec::Virtual => "virtual display".to_string(),
        SourceSpec::Synthetic => "test pattern".to_string(),
        SourceSpec::Screen { output } => format!("mirror {}", output.as_deref().unwrap_or("")),
    };
    let viewers = if m.viewers == 1 { "1 viewer".to_string() } else { format!("{} viewers", m.viewers) };
    format!("{} × {} · {kind} · {viewers}", m.width, m.height)
}
