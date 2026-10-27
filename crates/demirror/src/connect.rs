//! The Connect page: use this computer's screen as a monitor of another one.

use std::{
    sync::{Arc, Mutex, mpsc},
    thread,
};

use anyhow::{Result, bail};
use eframe::egui::{self, RichText, vec2};
use vmon_proto::{MonitorInfo, MonitorSpec, SourceSpec};

use crate::{
    net::{self, FoundHost, Target},
    widgets,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Preset {
    Smooth,
    Balanced,
    Sharp,
    Saver,
}

impl Preset {
    const ALL: [Preset; 4] = [Preset::Smooth, Preset::Balanced, Preset::Sharp, Preset::Saver];

    fn label(self) -> &'static str {
        match self {
            Preset::Smooth => "Smooth (60 fps)",
            Preset::Balanced => "Balanced (30 fps)",
            Preset::Sharp => "Sharp (best quality)",
            Preset::Saver => "Data saver (15 fps)",
        }
    }

    fn fps_quality(self) -> (u32, u8) {
        match self {
            Preset::Smooth => (60, 70),
            Preset::Balanced => (30, 75),
            Preset::Sharp => (30, 90),
            Preset::Saver => (15, 55),
        }
    }
}

pub struct Connect {
    hosts: Arc<Mutex<Vec<FoundHost>>>,
    address: String,
    token: String,
    preset: Preset,
    error: Option<String>,
    starting: Option<mpsc::Receiver<Result<(Target, MonitorInfo)>>>,
}

impl Connect {
    pub fn new(ctx: &egui::Context) -> Connect {
        let ctx = ctx.clone();
        Connect {
            hosts: net::discover(move || ctx.request_repaint()),
            address: String::new(),
            token: String::new(),
            preset: Preset::Balanced,
            error: None,
            starting: None,
        }
    }

    /// `screen`: this screen's size in pixels. `view`: set to the monitor to show once started.
    pub fn ui(&mut self, ui: &mut egui::Ui, screen: Option<[u32; 2]>, view: &mut Option<(Target, u32, bool)>) {
        self.poll(view);
        ui.heading("Use this computer as a monitor");
        ui.label("Extend another computer's desktop onto this screen.");
        ui.add_space(12.0);

        widgets::card(ui, |ui| {
            ui.label(RichText::new("This screen").weak());
            match screen {
                Some([w, h]) => {
                    ui.label(RichText::new(format!("{w} × {h}")).size(26.0).strong());
                    ui.label(RichText::new("Detected. The monitor follows if you resize the window or change the screen.").weak());
                }
                None => {
                    ui.label(RichText::new("Detecting…").size(26.0));
                }
            }
        });
        ui.add_space(12.0);

        widgets::card(ui, |ui| {
            ui.label(RichText::new("Computers sharing on this network").strong());
            ui.add_space(4.0);
            let hosts = self.hosts.lock().unwrap().clone();
            if hosts.is_empty() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Looking… (or type the address below)").weak());
                });
            }
            for host in hosts {
                ui.horizontal(|ui| {
                    let selected = Target::parse(&self.address, "").is_ok_and(|t| t.base.ends_with(&host.address));
                    if ui.selectable_label(selected, RichText::new(&host.name).strong()).clicked() {
                        self.address = host.address.clone();
                    }
                    ui.label(RichText::new(&host.address).weak());
                    if host.token_required {
                        ui.label(RichText::new("token needed").weak().small());
                    }
                });
            }
            ui.add_space(8.0);
            egui::Grid::new("connect-to").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Address");
                ui.add(egui::TextEdit::singleline(&mut self.address).hint_text("192.168.1.5 or host:7878").desired_width(260.0));
                ui.end_row();
                ui.label("Token");
                ui.add(egui::TextEdit::singleline(&mut self.token).hint_text("only if the host asks").password(true).desired_width(260.0));
                ui.end_row();
                ui.label("Quality");
                ui.horizontal_wrapped(|ui| {
                    for preset in Preset::ALL {
                        ui.selectable_value(&mut self.preset, preset, preset.label());
                    }
                });
                ui.end_row();
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let busy = self.starting.is_some();
                let button = egui::Button::new(RichText::new("Start monitor").strong()).min_size(vec2(160.0, 34.0));
                if ui.add_enabled(!busy && screen.is_some(), button).clicked() {
                    self.start(screen.unwrap_or([1920, 1080]), ui.ctx());
                }
                if busy {
                    ui.spinner();
                    ui.label("Asking the host for a monitor…");
                }
            });
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
        });
    }

    pub fn start(&mut self, [width, height]: [u32; 2], ctx: &egui::Context) {
        self.error = None;
        let (address, token, (fps, quality)) = (self.address.clone(), self.token.clone(), self.preset.fps_quality());
        let (done, starting) = mpsc::channel();
        self.starting = Some(starting);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = (|| {
                let target = Target::parse(&address, &token)?;
                let sources = target.sources()?;
                if !sources.virtual_available {
                    bail!("that computer can't add a display: {}", sources.virtual_detail);
                }
                let name = format!("{}'s screen", vmon_server::host_name());
                let spec = MonitorSpec { name: Some(name), width, height, fps, quality, source: SourceSpec::Virtual };
                Ok((target.clone(), target.create(&spec)?))
            })();
            let _ = done.send(result);
            ctx.request_repaint();
        });
    }

    fn poll(&mut self, view: &mut Option<(Target, u32, bool)>) {
        let Some(starting) = &self.starting else { return };
        match starting.try_recv() {
            Ok(Ok((target, info))) => {
                self.starting = None;
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

    pub fn set_address(&mut self, address: String) {
        self.address = address;
    }

    pub fn show_error(&mut self, error: String) {
        self.error = Some(error);
    }
}
