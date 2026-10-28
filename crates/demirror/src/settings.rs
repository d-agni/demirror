//! What the app remembers between runs, the quality presets, and starting with the session.

use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preset {
    Smooth,
    Balanced,
    Sharp,
    Saver,
}

impl Preset {
    pub const ALL: [Preset; 4] = [Preset::Smooth, Preset::Balanced, Preset::Sharp, Preset::Saver];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Smooth => "Smooth",
            Preset::Balanced => "Balanced",
            Preset::Sharp => "Sharp",
            Preset::Saver => "Data saver",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Preset::Smooth => "60 fps, up to 1600 pixels wide. For moving things around.",
            Preset::Balanced => "30 fps, up to 1920 pixels wide. Good for most uses.",
            Preset::Sharp => "30 fps, up to 2560 pixels wide, best picture. Needs a strong network.",
            Preset::Saver => "15 fps, up to 1280 pixels wide. For slow or busy networks.",
        }
    }

    pub fn fps_quality(self) -> (u32, u8) {
        match self {
            Preset::Smooth => (60, 70),
            Preset::Balanced => (30, 75),
            Preset::Sharp => (30, 90),
            Preset::Saver => (15, 55),
        }
    }

    /// The longest side a monitor gets (the same as the web dashboard's presets): a big
    /// screen gets a display it can draw and send smoothly, the same shape as the screen.
    pub fn max_side(self) -> u32 {
        match self {
            Preset::Smooth => 1600,
            Preset::Balanced => 1920,
            Preset::Sharp => 2560,
            Preset::Saver => 1280,
        }
    }
}

/// A monitor's size for a screen of `size` pixels: the same shape, the longest side at most
/// `max_side`, the width a multiple of 8 (rounded down).
pub fn fit([w, h]: [u32; 2], max_side: u32) -> [u32; 2] {
    let scale = (max_side as f64 / w.max(h).max(1) as f64).min(1.0);
    let width = ((w as f64 * scale / 8.0).floor() as u32 * 8).max(8);
    let height = ((h as f64 * scale / 2.0).round() as u32 * 2).max(2);
    [width, height]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub port: u16,
    pub require_token: bool,
    pub token: String,
    pub announce: bool,
    pub allow_input: bool,
    /// Start sharing when the app opens.
    pub share_on_open: bool,
    pub address: String,
    pub host_token: String,
    pub preset: Preset,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            port: vmon_proto::DEFAULT_PORT,
            require_token: false,
            token: crate::net::new_token(),
            announce: true,
            allow_input: true,
            share_on_open: false,
            address: String::new(),
            host_token: String::new(),
            preset: Preset::Balanced,
        }
    }
}

impl Settings {
    /// What the server was started with that a restart would change: port, token, announced.
    pub fn server_key(&self) -> (u16, Option<String>, bool) {
        let token = self.require_token.then(|| self.token.trim().to_string()).filter(|t| !t.is_empty());
        (self.port, token, self.announce)
    }
}

pub const KEY: &str = "demirror.settings";

/// Starting demirror (sharing) when the user logs in.
pub mod autostart {
    use anyhow::Result;

    pub fn supported() -> bool {
        cfg!(any(target_os = "linux", windows))
    }

    #[cfg(target_os = "linux")]
    fn entry() -> Option<std::path::PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))?;
        Some(config.join("autostart").join("demirror.desktop"))
    }

    #[cfg(target_os = "linux")]
    pub fn enabled() -> bool {
        entry().is_some_and(|p| p.exists())
    }

    #[cfg(target_os = "linux")]
    pub fn set(on: bool) -> Result<()> {
        use anyhow::Context;
        let path = entry().context("no home directory")?;
        if !on {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            };
        }
        let exe = std::env::current_exe()?;
        std::fs::create_dir_all(path.parent().context("autostart folder")?)?;
        let desktop = format!(
            "[Desktop Entry]\nType=Application\nName=demirror\nComment=Share this computer's desktop with other devices\nExec=\"{}\" --share\nIcon=video-display\nX-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        std::fs::write(&path, desktop).with_context(|| format!("writing {}", path.display()))
    }

    #[cfg(windows)]
    const RUN: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

    #[cfg(windows)]
    pub fn enabled() -> bool {
        std::process::Command::new("reg")
            .args(["query", RUN, "/v", "demirror"])
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[cfg(windows)]
    pub fn set(on: bool) -> Result<()> {
        let exe = std::env::current_exe()?;
        let value = format!("\"{}\" --share", exe.display());
        let args: Vec<&str> = if on {
            vec!["add", RUN, "/v", "demirror", "/d", &value, "/f"]
        } else {
            vec!["delete", RUN, "/v", "demirror", "/f"]
        };
        let status = std::process::Command::new("reg").args(&args).output()?.status;
        anyhow::ensure!(status.success() || !on, "Windows didn't take the change");
        Ok(())
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    pub fn enabled() -> bool {
        false
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    pub fn set(_on: bool) -> Result<()> {
        anyhow::bail!("not on this system")
    }
}

/// What the Settings page asks of the app.
#[derive(Default)]
pub struct Changes {
    /// The server's settings changed while sharing: restart it to apply them.
    pub restart_sharing: bool,
    pub allow_input: Option<bool>,
}

/// The Settings page. `sharing_with`: the running server's `server_key` (a change to those
/// applies when it restarts).
pub fn ui(ui: &mut egui::Ui, settings: &mut Settings, sharing_with: Option<&(u16, Option<String>, bool)>, note: &mut Option<String>) -> Changes {
    let mut changes = Changes::default();
    crate::widgets::page_title(ui, "Settings", "How this computer shares, and how demirror starts.");
    if let Some(text) = note.as_deref()
        && crate::widgets::banner(ui, text, crate::widgets::ACCENT)
    {
        *note = None;
    }

    crate::widgets::card(ui, |ui| {
        crate::widgets::section(ui, "Sharing");
        ui.add_space(6.0);
        crate::widgets::switch(ui, &mut settings.announce, "Let devices find this computer", "Other demirror apps list it without typing an address.");
        ui.add_space(4.0);
        let control = crate::widgets::switch(
            ui,
            &mut settings.allow_input,
            "Let devices control this computer",
            "Touch, mouse and keyboard on a monitor work on this desktop.",
        );
        if control.changed() {
            changes.allow_input = Some(settings.allow_input);
        }
        ui.add_space(4.0);
        crate::widgets::switch(ui, &mut settings.require_token, "Ask devices for a token", "Only devices given the code or address (it includes the token) can connect.");
        if settings.require_token {
            ui.horizontal(|ui| {
                ui.add_space(48.0);
                ui.add(egui::TextEdit::singleline(&mut settings.token).desired_width(180.0).font(egui::TextStyle::Monospace));
                if ui.button("New token").clicked() {
                    settings.token = crate::net::new_token();
                }
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(48.0);
            ui.label("Port");
            ui.add(egui::DragValue::new(&mut settings.port).range(1024..=65535));
            ui.label(egui::RichText::new(format!("default {}", vmon_proto::DEFAULT_PORT)).small().weak());
        });
        if sharing_with.is_some_and(|key| *key != settings.server_key()) {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("These apply when sharing starts again.").weak());
                if ui.button("Restart sharing now").clicked() {
                    changes.restart_sharing = true;
                }
            });
        }
    });
    ui.add_space(12.0);

    crate::widgets::card(ui, |ui| {
        crate::widgets::section(ui, "Starting");
        ui.add_space(6.0);
        crate::widgets::switch(ui, &mut settings.share_on_open, "Share when demirror opens", "Start sharing straight away, without pressing Start sharing.");
        if autostart::supported() {
            ui.add_space(4.0);
            let mut on = autostart::enabled();
            let response = crate::widgets::switch(ui, &mut on, "Open demirror when I log in", "It starts sharing in the background, ready for your devices.");
            if response.changed() {
                *note = Some(match autostart::set(on) {
                    Ok(()) if on => "demirror will open and share when you log in.".to_string(),
                    Ok(()) => "demirror won't open when you log in.".to_string(),
                    Err(e) => format!("Couldn't change that: {e:#}"),
                });
            }
        }
    });
    ui.add_space(12.0);

    crate::widgets::card(ui, |ui| {
        crate::widgets::section(ui, "About");
        ui.label(format!("demirror {}", env!("CARGO_PKG_VERSION")));
        ui.label(egui::RichText::new("Virtual monitors for phones, tablets and other computers.").weak());
    });

    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitors_keep_the_screens_shape() {
        assert_eq!(fit([1080, 2400], 1920), [864, 1920], "a phone held upright");
        assert_eq!(fit([2400, 1080], 1920), [1920, 864]);
        assert_eq!(fit([1920, 1080], 1920), [1920, 1080], "small enough already");
        assert_eq!(fit([1366, 768], 2560), [1360, 768], "never larger, width a multiple of 8");
        assert_eq!(fit([2560, 1600], Preset::Saver.max_side()), [1280, 800]);
    }

    #[test]
    fn old_settings_still_load() {
        let s: Settings = serde_json::from_str(r#"{"port": 9000}"#).unwrap();
        assert_eq!((s.port, s.preset, s.allow_input), (9000, Preset::Balanced, true));
    }
}
