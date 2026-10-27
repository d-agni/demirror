//! demirror: turn phones, tablets and other computers into extra monitors.
//!
//! One window with two pages: **Host** runs the server on this computer (other devices
//! extend its desktop), **Connect** uses this computer's screen as a monitor of another one.
//!
//! `demirror --share` starts sharing at once (for starting with the session);
//! `demirror --connect <address>` opens Connect with the address filled in, and with
//! `--start` starts the monitor right away (a tablet kept as a second screen).

#![cfg_attr(windows, windows_subsystem = "windows")]

mod connect;
mod host;
mod net;
mod viewer;
mod widgets;

use eframe::egui::{self, RichText};

use crate::{connect::Connect, host::Host, net::Target, viewer::Viewer};

fn main() -> eframe::Result {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("demirror")
            .with_app_id("demirror")
            .with_inner_size([900.0, 700.0])
            .with_min_inner_size([560.0, 460.0]),
        ..Default::default()
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let share = args.iter().any(|a| a == "--share");
    let connect_to = args.iter().position(|a| a == "--connect").and_then(|i| args.get(i + 1)).cloned();
    let start = args.iter().any(|a| a == "--start");
    eframe::run_native(
        "demirror",
        options,
        Box::new(move |cc| {
            let mut app = App::new(&cc.egui_ctx);
            if share {
                app.host.start();
            }
            if let Some(address) = connect_to {
                app.page = Page::Connect;
                app.connect.set_address(address);
                app.start_when_ready = start;
            }
            Ok(Box::new(app))
        }),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Host,
    Connect,
}

struct App {
    page: Page,
    host: Host,
    connect: Connect,
    viewer: Option<Viewer>,
    /// `--start`: start the monitor once this screen's size is known.
    start_when_ready: bool,
}

impl App {
    fn new(ctx: &egui::Context) -> App {
        App { page: Page::Host, host: Host::new(), connect: Connect::new(ctx), viewer: None, start_when_ready: false }
    }

    fn open_viewer(&mut self, ctx: &egui::Context, (target, id, own): (Target, u32, bool)) {
        match Viewer::open(target.clone(), id, own, ctx) {
            Ok(viewer) => {
                self.viewer = Some(viewer);
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
            Err(e) => {
                if own {
                    let _ = target.close(id);
                }
                self.page = Page::Connect;
                self.connect.show_error(format!("{e:#}"));
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if let Some(viewer) = &mut self.viewer {
            let mut left = false;
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                left = matches!(viewer.ui(ui), viewer::Action::Leave);
            });
            if left && let Some(viewer) = self.viewer.take() {
                viewer.leave();
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
            return;
        }

        // This screen in physical pixels: what a monitor started from here asks for.
        let screen = ctx.input(|i| {
            let v = i.viewport();
            let (size, ppp) = (v.monitor_size?, v.native_pixels_per_point?);
            Some([(size.x * ppp).round() as u32, (size.y * ppp).round() as u32])
        });

        if let Some(screen) = screen.filter(|_| self.start_when_ready) {
            self.start_when_ready = false;
            self.connect.start(screen, &ctx);
        }

        egui::Panel::top("pages").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("demirror").size(20.0).strong());
                ui.add_space(16.0);
                let sharing = if self.host.running() { "Host  ●" } else { "Host" };
                ui.selectable_value(&mut self.page, Page::Host, RichText::new(sharing).size(15.0));
                ui.selectable_value(&mut self.page, Page::Connect, RichText::new("Connect").size(15.0));
            });
            ui.add_space(6.0);
        });

        let mut view = None;
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.add_space(8.0);
                match self.page {
                    Page::Host => self.host.ui(ui, &mut view),
                    Page::Connect => self.connect.ui(ui, screen, &mut view),
                }
            });
        });
        if let Some(request) = view {
            self.open_viewer(&ctx, request);
        }
    }

    fn on_exit(&mut self) {
        if let Some(viewer) = self.viewer.take() {
            viewer.leave_now();
        }
        self.host.stop();
    }
}
