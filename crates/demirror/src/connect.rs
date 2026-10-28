//! The Use as monitor page: this computer's screen becomes a monitor of another one.

use std::{
    sync::{Arc, Mutex, mpsc},
    thread,
};

use anyhow::{Result, bail};
use eframe::egui::{self, RichText};
use vmon_proto::{MonitorInfo, MonitorSpec, SourceSpec};

use crate::{
    host::ViewRequest,
    net::{self, FoundHost, Target},
    settings::{self, Preset, Settings},
    widgets,
};

type Starting = mpsc::Receiver<Result<(Target, MonitorInfo)>>;

pub struct Connect {
    hosts: Arc<Mutex<Vec<FoundHost>>>,
    error: Option<String>,
    /// The address a monitor is being started on, and the answer to come.
    starting: Option<(String, Starting)>,
    /// A found host that wants a token: its address, while the token is typed.
    token_for: Option<String>,
}

impl Connect {
    pub fn new(ctx: &egui::Context) -> Connect {
        let ctx = ctx.clone();
        Connect { hosts: net::discover(move || ctx.request_repaint()), error: None, starting: None, token_for: None }
    }

    /// `screen`: this screen's size in pixels. `view`: set to the monitor to show once started.
    pub fn ui(&mut self, ui: &mut egui::Ui, screen: Option<[u32; 2]>, settings: &mut Settings, view: &mut Option<ViewRequest>) {
        self.poll(view);
        widgets::page_title(ui, "Use as a monitor", "This screen becomes an extra monitor for another computer that's sharing.");
        if let Some(e) = &self.error
            && widgets::banner(ui, e, ui.visuals().error_fg_color)
        {
            self.error = None;
        }

        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                widgets::section(ui, "Computers sharing nearby");
                if self.starting.is_none() {
                    ui.spinner().on_hover_text("Still looking");
                }
            });
            ui.add_space(6.0);
            let hosts = self.hosts.lock().unwrap().clone();
            if hosts.is_empty() {
                ui.label(RichText::new("None found yet. On the other computer, open demirror and press Start sharing, or enter its address below.").weak());
            }
            for host in hosts {
                self.host_row(ui, &host, screen, settings);
                ui.add_space(6.0);
            }
        });
        ui.add_space(12.0);

        widgets::card(ui, |ui| {
            widgets::section(ui, "Picture");
            ui.add_space(4.0);
            widgets::segmented(ui, &mut settings.preset, &Preset::ALL.map(|p| (p, p.label())));
            ui.label(RichText::new(settings.preset.detail()).small().weak());
            ui.add_space(6.0);
            match screen {
                Some(size) => {
                    let [w, h] = settings::fit(size, settings.preset.max_side());
                    ui.label(format!("This screen is {} × {}: the monitor will be {w} × {h}, and follows the window's size.", size[0], size[1]));
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Measuring this screen…");
                    });
                }
            }
        });
        ui.add_space(12.0);

        let none_found = self.hosts.lock().unwrap().is_empty();
        widgets::card(ui, |ui| {
            egui::CollapsingHeader::new(RichText::new("Connect by address").strong())
                .default_open(none_found)
                .show(ui, |ui| {
                    ui.label(RichText::new("The address the other computer shows, like 192.168.1.5:7878.").small().weak());
                    ui.add_space(4.0);
                    egui::Grid::new("connect-to").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("Address");
                        ui.add(egui::TextEdit::singleline(&mut settings.address).hint_text("192.168.1.5:7878").desired_width(280.0));
                        ui.end_row();
                        ui.label("Token");
                        ui.add(
                            egui::TextEdit::singleline(&mut settings.host_token)
                                .hint_text("only if it asks for one")
                                .password(true)
                                .desired_width(280.0),
                        );
                        ui.end_row();
                    });
                    ui.add_space(8.0);
                    let address = settings.address.clone();
                    self.start_button(ui, &address, screen, settings);
                });
        });
    }

    fn host_row(&mut self, ui: &mut egui::Ui, host: &FoundHost, screen: Option<[u32; 2]>, settings: &mut Settings) {
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("🖥").size(26.0));
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new(&host.name).strong().size(15.5));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&host.address).small().weak());
                            if host.token_required {
                                widgets::pill(ui, "🔒 token", widgets::ACCENT);
                            }
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if host.token_required && self.token_for.as_deref() != Some(&host.address) {
                            if ui.button("Start monitor…").clicked() {
                                self.token_for = Some(host.address.clone());
                            }
                        } else if !host.token_required {
                            self.start_button_for(ui, &host.address, "", screen, settings);
                        }
                    });
                });
                if host.token_required && self.token_for.as_deref() == Some(&host.address) {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Token");
                        ui.add(egui::TextEdit::singleline(&mut settings.host_token).password(true).desired_width(180.0));
                        let token = settings.host_token.clone();
                        self.start_button_for(ui, &host.address, &token, screen, settings);
                    });
                }
            });
    }

    fn start_button(&mut self, ui: &mut egui::Ui, address: &str, screen: Option<[u32; 2]>, settings: &Settings) {
        let token = settings.host_token.clone();
        ui.horizontal(|ui| self.start_button_for(ui, address, &token, screen, settings));
    }

    fn start_button_for(&mut self, ui: &mut egui::Ui, address: &str, token: &str, screen: Option<[u32; 2]>, settings: &Settings) {
        let this_one = self.starting.as_ref().is_some_and(|(a, _)| a == address);
        if this_one {
            ui.spinner();
            ui.label("Starting…");
            return;
        }
        let enabled = self.starting.is_none() && screen.is_some() && !address.trim().is_empty();
        if widgets::primary_button(ui, "Start monitor", enabled).clicked() {
            let size = settings::fit(screen.unwrap_or([1920, 1080]), settings.preset.max_side());
            self.start_at(address, token, size, settings.preset, ui.ctx());
        }
    }

    /// Starts a monitor on the address and token in `settings` (`--connect … --start`).
    pub fn start(&mut self, screen: [u32; 2], settings: &Settings, ctx: &egui::Context) {
        let size = settings::fit(screen, settings.preset.max_side());
        self.start_at(&settings.address, &settings.host_token, size, settings.preset, ctx);
    }

    fn start_at(&mut self, address: &str, token: &str, [width, height]: [u32; 2], preset: Preset, ctx: &egui::Context) {
        self.error = None;
        let (address, token, (fps, quality)) = (address.to_string(), token.to_string(), preset.fps_quality());
        let (done, starting) = mpsc::channel();
        self.starting = Some((address.clone(), starting));
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = (|| {
                let target = Target::parse(&address, &token)?;
                let sources = target.sources()?;
                if !sources.virtual_available {
                    bail!("That computer can't add a display: {}", sources.virtual_detail);
                }
                let name = format!("{}'s screen", vmon_server::host_name());
                let spec = MonitorSpec { name: Some(name), width, height, fps, quality, source: SourceSpec::Virtual };
                Ok((target.clone(), target.create(&spec)?))
            })();
            let _ = done.send(result);
            ctx.request_repaint();
        });
    }

    fn poll(&mut self, view: &mut Option<ViewRequest>) {
        let Some((_, starting)) = &self.starting else { return };
        match starting.try_recv() {
            Ok(Ok((target, info))) => {
                self.starting = None;
                self.token_for = None;
                *view = Some((target, info.id, true));
            }
            Ok(Err(e)) => {
                self.starting = None;
                self.error = Some(format!("{e:#}"));
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.starting = None,
        }
    }

    pub fn show_error(&mut self, error: String) {
        self.error = Some(error);
    }
}
