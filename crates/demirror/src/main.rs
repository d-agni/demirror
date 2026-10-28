//! demirror: turn phones, tablets and other computers into extra monitors.
//!
//! One window with three pages: **Share** runs the server on this computer (other devices
//! extend its desktop), **Use as monitor** makes this computer's screen a monitor of another
//! one, **Settings**. What's set is remembered between runs.
//!
//! `demirror --share` starts sharing at once (for starting with the session);
//! `demirror --connect <address>` opens Use as monitor with the address filled in, and with
//! `--start` starts the monitor right away (a tablet kept as a second screen).
//! On Windows, `--install-driver` and `--uninstall-driver` (as administrator) add and remove
//! the display driver.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod connect;
#[cfg(windows)]
mod driver;
mod host;
mod net;
mod settings;
mod viewer;
mod widgets;

use eframe::egui::{self, Color32, RichText, vec2};

use crate::{connect::Connect, host::Host, host::ViewRequest, settings::Settings, viewer::Viewer};

fn main() -> eframe::Result {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    if let Some(code) = driver::run_command(&args) {
        std::process::exit(code);
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("demirror")
            .with_app_id("demirror")
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([640.0, 480.0])
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../../../packaging/icons/demirror.png")).unwrap_or_default()),
        ..Default::default()
    };
    let share = args.iter().any(|a| a == "--share");
    let connect_to = args.iter().position(|a| a == "--connect").and_then(|i| args.get(i + 1)).cloned();
    let start = args.iter().any(|a| a == "--start");
    eframe::run_native(
        "demirror",
        options,
        Box::new(move |cc| {
            widgets::apply_theme(&cc.egui_ctx);
            let settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, settings::KEY)).unwrap_or_default();
            let mut app = App::new(&cc.egui_ctx, settings);
            if share || app.settings.share_on_open {
                app.start_sharing();
            }
            if let Some(address) = connect_to {
                app.page = Page::Connect;
                app.settings.address = address;
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
    Settings,
}

struct App {
    page: Page,
    settings: Settings,
    host: Host,
    /// The running server's `Settings::server_key`.
    shared_with: Option<(u16, Option<String>, bool)>,
    connect: Connect,
    viewer: Option<Viewer>,
    /// `--start`: start the monitor once this screen's size is known.
    start_when_ready: bool,
    note: Option<String>,
}

impl App {
    fn new(ctx: &egui::Context, settings: Settings) -> App {
        App {
            page: Page::Host,
            settings,
            host: Host::new(),
            shared_with: None,
            connect: Connect::new(ctx),
            viewer: None,
            start_when_ready: false,
            note: None,
        }
    }

    fn start_sharing(&mut self) {
        self.host.start(&self.settings);
        self.shared_with = self.host.running().then(|| self.settings.server_key());
    }

    fn open_viewer(&mut self, ctx: &egui::Context, (target, id, own): ViewRequest) {
        match Viewer::open(target.clone(), id, own, self.settings.preset.max_side(), ctx) {
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

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("◧").size(26.0).color(widgets::ACCENT));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new("demirror").size(19.0).strong());
                ui.label(RichText::new("virtual monitors").small().weak());
            });
        });
        ui.add_space(22.0);
        let sharing = self.host.running();
        let pages = [
            (Page::Host, "📡", "Share this computer"),
            (Page::Connect, "🖵", "Use as monitor"),
            (Page::Settings, "⚙", "Settings"),
        ];
        for (page, icon, label) in pages {
            let selected = self.page == page;
            let text = RichText::new(format!("{icon}   {label}")).size(15.0);
            let button = egui::Button::new(if selected { text.strong().color(widgets::ACCENT) } else { text })
                .fill(if selected { widgets::ACCENT.linear_multiply(0.14) } else { Color32::TRANSPARENT })
                .corner_radius(8)
                .min_size(vec2(ui.available_width(), 38.0));
            if ui.add(button).clicked() {
                self.page = page;
            }
        }

        // The sharing status, at the bottom.
        ui.add_space((ui.available_height() - 84.0).max(16.0));
        {
            egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .corner_radius(10)
                .inner_margin(10)
                .show(ui, |ui| ui.vertical(|ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        widgets::status_dot(ui, if sharing { widgets::OK } else { widgets::IDLE });
                        ui.label(RichText::new(if sharing { "Sharing" } else { "Not sharing" }).strong());
                    });
                    if sharing {
                        let n = self.host.monitor_count();
                        let text = match n {
                            0 => "No monitors yet".to_string(),
                            1 => "1 monitor".to_string(),
                            n => format!("{n} monitors"),
                        };
                        ui.label(RichText::new(text).small().weak());
                    }
                }));
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

        // This screen in physical pixels: what a monitor started from here is sized to.
        let screen = ctx.input(|i| {
            let v = i.viewport();
            let (size, ppp) = (v.monitor_size?, v.native_pixels_per_point?);
            Some([(size.x * ppp).round() as u32, (size.y * ppp).round() as u32])
        });

        if let Some(screen) = screen.filter(|_| self.start_when_ready) {
            self.start_when_ready = false;
            self.connect.start(screen, &self.settings, &ctx);
        }

        egui::Panel::left("nav")
            .exact_size(220.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(ui.visuals().faint_bg_color).inner_margin(egui::Margin::symmetric(12, 0)))
            .show(ui, |ui| self.sidebar(ui));

        let mut view = None;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(ui.visuals().panel_fill)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                // A readable column, centred in a wide window.
                let width = ui.available_width().min(860.0) - 48.0;
                let side = ((ui.available_width() - width) / 2.0).max(24.0);
                ui.horizontal_top(|ui| {
                    ui.add_space(side);
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        ui.add_space(28.0);
                        match self.page {
                            Page::Host => self.host.ui(ui, &mut self.settings, &mut view),
                            Page::Connect => self.connect.ui(ui, screen, &mut self.settings, &mut view),
                            Page::Settings => {
                                let changes = settings::ui(ui, &mut self.settings, self.shared_with.as_ref(), &mut self.note);
                                if let Some(allow) = changes.allow_input {
                                    self.host.set_allow_input(allow);
                                }
                                if changes.restart_sharing {
                                    self.host.stop();
                                    self.start_sharing();
                                }
                            }
                        }
                        ui.add_space(28.0);
                    });
                });
            });
        });
        if !self.host.running() {
            self.shared_with = None;
        } else if self.shared_with.is_none() {
            self.shared_with = Some(self.settings.server_key());
        }
        if let Some(request) = view {
            self.open_viewer(&ctx, request);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, settings::KEY, &self.settings);
    }

    fn on_exit(&mut self) {
        if let Some(viewer) = self.viewer.take() {
            viewer.leave_now();
        }
        self.host.stop();
    }
}
