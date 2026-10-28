//! Pixel sources that back a virtual monitor.

#[cfg(windows)]
mod idd;
mod synthetic;
#[cfg(target_os = "linux")]
mod vdisplay;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
mod wlr;

use std::sync::mpsc;

use anyhow::Result;
use vmon_proto::{OutputInfo, input::InputEvent};

pub use synthetic::Synthetic;

/// Produces frames for one monitor. Created and used on the monitor's own
/// capture thread, so implementations need not be `Send`.
pub trait Source {
    /// Captures one frame into `out` as tightly packed BGRA (alpha ignored)
    /// and returns its size.
    fn capture(&mut self, out: &mut Vec<u8>) -> Result<(u32, u32)>;

    /// The viewer's screen is now `width` x `height`. Sources that can follow it do;
    /// the rest keep their size and the viewer scales.
    fn resize(&mut self, _width: u32, _height: u32) -> Result<()> {
        Ok(())
    }

    /// Where viewers' input for this source goes, if it takes input. Called once, when the
    /// source is open; the events are delivered without waiting for captures.
    fn input(&mut self) -> Option<mpsc::Sender<InputEvent>> {
        None
    }
}

/// Name of the screen backend compiled for this OS, if any.
pub fn screen_backend() -> Option<&'static str> {
    if cfg!(target_os = "linux") {
        Some("wlr")
    } else if cfg!(windows) {
        Some("windows")
    } else {
        None
    }
}

/// Lists the host screens that can be captured.
pub fn list_outputs() -> Result<Vec<OutputInfo>> {
    #[cfg(target_os = "linux")]
    return wlr::list_outputs();
    #[cfg(windows)]
    return windows::list_outputs();
    #[allow(unreachable_code)]
    Ok(Vec::new())
}

/// Opens a capture of the named host screen (or the first one).
pub fn open_screen(output: Option<&str>) -> Result<Box<dyn Source>> {
    #[cfg(target_os = "linux")]
    return Ok(Box::new(wlr::WlrSource::open(output)?));
    #[cfg(windows)]
    return Ok(Box::new(windows::GdiSource::open(output)?));
    #[allow(unreachable_code)]
    {
        let _ = output;
        anyhow::bail!("screen capture is not supported on this OS yet")
    }
}

/// Whether `SourceSpec::Virtual` works here: the driver in use, or why not.
pub fn virtual_support() -> (bool, String) {
    #[cfg(target_os = "linux")]
    return vdisplay::support();
    #[cfg(windows)]
    return idd::support();
    #[allow(unreachable_code)]
    (
        false,
        "virtual displays are not supported on this OS yet".into(),
    )
}

/// Plugs a new virtual display into the OS and captures it.
pub fn open_virtual(width: u32, height: u32) -> Result<Box<dyn Source>> {
    #[cfg(target_os = "linux")]
    return Ok(Box::new(vdisplay::VirtualSource::open(width, height)?));
    #[cfg(windows)]
    return Ok(Box::new(idd::VirtualSource::open(width, height)?));
    #[allow(unreachable_code)]
    {
        let _ = (width, height);
        anyhow::bail!("virtual displays are not supported on this OS yet")
    }
}
