//! Wire types shared by the demirror server and its clients.

use serde::{Deserialize, Serialize};

pub mod input;

pub const DEFAULT_PORT: u16 = 7878;

/// mDNS service type hosts advertise themselves as, for viewers to find them on the network.
/// TXT records: `token` (`required` or `none`), `version`.
pub const MDNS_SERVICE: &str = "_demirror._tcp.local.";

/// Where a virtual monitor gets its pixels from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SourceSpec {
    /// Animated test pattern rendered by the server. Works on every OS.
    Synthetic,
    /// A screen of the host. `output` is the name reported by `/api/sources`
    /// (`HDMI-A-1` on Wayland, `\\.\DISPLAY2` on Windows); `None` picks the
    /// first one, or the one created by the server's `--create-output-cmd`.
    Screen { output: Option<String> },
    /// A new display plugged into the host OS for this monitor, so the desktop
    /// extends onto it like a real secondary monitor. Removed when the monitor closes.
    Virtual,
}

/// Request body for `POST /api/monitors`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSpec {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// JPEG quality, 1-100.
    #[serde(default = "default_quality")]
    pub quality: u8,
    #[serde(default = "default_source")]
    pub source: SourceSpec,
    /// Where a new virtual display goes next to the host's main screen, where the host
    /// lets demirror choose (`SourcesInfo::can_place`).
    #[serde(default)]
    pub placement: Placement,
    /// A new virtual display becomes the host's main display while it's on: programs open
    /// on it, and its taskbar is the main one. Followed where `SourcesInfo::can_place`.
    #[serde(default)]
    pub main_display: bool,
}

/// Where a new virtual display goes, next to the host's main screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    #[default]
    Right,
    Left,
    Above,
    Below,
}

fn default_width() -> u32 {
    1280
}
fn default_height() -> u32 {
    720
}
fn default_fps() -> u32 {
    30
}
fn default_quality() -> u8 {
    75
}
fn default_source() -> SourceSpec {
    SourceSpec::Synthetic
}

impl Default for MonitorSpec {
    fn default() -> Self {
        Self {
            name: None,
            width: default_width(),
            height: default_height(),
            fps: default_fps(),
            quality: default_quality(),
            source: default_source(),
            placement: Placement::default(),
            main_display: false,
        }
    }
}

/// Body of `POST /api/monitors/{id}/size`: the viewing device's screen changed (rotated,
/// window resized). A virtual display asks the desktop for the closest mode it offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// A live monitor as reported by the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    /// Size of the frames actually streamed (a screen source streams its own size).
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub quality: u8,
    pub source: SourceSpec,
    /// Number of clients currently viewing.
    pub viewers: usize,
    /// Viewers may control the host through this monitor (pointer, keys).
    #[serde(default)]
    pub input: bool,
    /// Fingers on the viewer are the host's touch (`InputEvent::Touch`), not a mouse.
    #[serde(default)]
    pub touch: bool,
}

/// A screen the server can capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputInfo {
    pub name: String,
    pub description: String,
    pub width: u32,
    pub height: u32,
}

/// Response of `GET /api/sources`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcesInfo {
    /// Screen backend of this host (`wlr`, `windows`), if any.
    pub screen_backend: Option<String>,
    pub outputs: Vec<OutputInfo>,
    /// The server creates a new OS display per screen monitor (`--create-output-cmd`).
    pub can_create_outputs: bool,
    /// `SourceSpec::Virtual` can be used on this host.
    pub virtual_available: bool,
    /// Which virtual display driver is used, or why none is available.
    pub virtual_detail: String,
    /// `MonitorSpec::placement` and `main_display` are followed (Windows; on Linux the
    /// desktop arranges).
    #[serde(default)]
    pub can_place: bool,
}

/// Protocol of the Linux virtual display daemon (`vmon-vdisplayd`).
///
/// JSON lines over a Unix socket. Each connection owns at most one display,
/// which is unplugged when the connection closes, so a crashed server never
/// leaves displays behind.
pub mod vdisplay {
    use serde::{Deserialize, Serialize};

    pub const SOCKET_PATH: &str = "/run/demirror/vdisplay.sock";

    /// Mode hints for desktops: `<dir>/<device name>` holds `<w>x<h>`, the size the
    /// display is viewed at. VKMS can't advertise a preferred mode itself, so a desktop that
    /// reads this (kompact does) picks the closest mode the device offers. Written before the
    /// device is enabled; rewritten when the viewer's size changes.
    pub const MODE_HINT_DIR: &str = "/run/vkms-mode";

    /// Input hints for desktops: `<dir>/<display name>` lists (one a line) the input devices
    /// that point at that display: their absolute positions are positions on it.
    pub const INPUT_HINT_DIR: &str = "/run/vkms-input";

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "op", rename_all = "snake_case")]
    pub enum Request {
        /// Check the daemon is up.
        Hello,
        /// Plug in a virtual display. The size is the preferred mode; the
        /// desktop may still pick another one, like with a real monitor.
        Create { width: u32, height: u32 },
        /// Copy the display's current image into the frame file.
        Capture,
        /// The viewer's size changed: update the display's mode hint.
        Resize { width: u32, height: u32 },
        /// Input for a display this user created (on any connection: the server sends input
        /// on its own one, so it never waits behind a capture). Not answered.
        Input { display: String, event: crate::input::InputEvent },
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Response {
        Hello {
            version: String,
            driver: String,
        },
        Created {
            /// Name of the display device, e.g. `demirror-1`.
            display: String,
            /// DRM device node of the display, e.g. `/dev/dri/card1`.
            card: String,
            /// File (on tmpfs, owned by the client) that `Capture` writes BGRA pixels to.
            frame_path: String,
        },
        Captured(Frame),
        Resized,
        Error {
            message: String,
        },
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    pub struct Frame {
        /// The desktop is showing something on the display. When false, it
        /// has not picked up the display (yet) and no pixels were written.
        pub active: bool,
        pub width: u32,
        pub height: u32,
        /// Bytes per row in the frame file.
        pub stride: u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_defaults_fill_missing_fields() {
        let spec: MonitorSpec = serde_json::from_str(r#"{"width":800}"#).unwrap();
        assert_eq!((spec.width, spec.height, spec.fps), (800, 720, 30));
        assert_eq!(spec.source, SourceSpec::Synthetic);
        assert_eq!(spec.placement, Placement::Right);
        assert!(!spec.main_display);
        let spec: MonitorSpec = serde_json::from_str(r#"{"placement":"above","main_display":true}"#).unwrap();
        assert_eq!(spec.placement, Placement::Above);
        assert!(spec.main_display);
    }

    #[test]
    fn source_spec_is_tagged() {
        let json = serde_json::to_string(&SourceSpec::Screen {
            output: Some("HDMI-A-1".into()),
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"screen","output":"HDMI-A-1"}"#);
    }
}
