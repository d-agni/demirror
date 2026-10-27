//! Virtual display devices through the kernel's VKMS driver and its configfs
//! interface (Linux 6.17+). Each device is a DRM card with one connected
//! connector: to the desktop it looks like a newly plugged-in monitor.

use std::{
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use std::os::unix::fs::PermissionsExt;
use vmon_proto::vdisplay::MODE_HINT_DIR;

const CONFIGFS: &str = "/sys/kernel/config";
const VKMS_DIR: &str = "/sys/kernel/config/vkms";
/// Every device this daemon creates is named `demirror-<n>`.
pub const PREFIX: &str = "demirror-";

/// Loads VKMS without its default device and checks configfs support.
pub fn ensure_loaded() -> Result<()> {
    if Path::new(VKMS_DIR).exists() {
        return Ok(());
    }
    if fs::read_dir(CONFIGFS)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true)
    {
        // Usually mounted by systemd; mount it ourselves otherwise.
        let _ = Command::new("mount")
            .args(["-t", "configfs", "none", CONFIGFS])
            .status();
    }
    let status = Command::new("modprobe")
        .args(["vkms", "create_default_dev=0"])
        .status()
        .context("running modprobe")?;
    if !status.success() {
        bail!("`modprobe vkms` failed: is CONFIG_DRM_VKMS enabled in this kernel?");
    }
    if !Path::new(VKMS_DIR).exists() {
        bail!("VKMS is loaded but has no configfs interface (needs Linux 6.17 or newer)");
    }
    Ok(())
}

/// Removes displays left behind by a previous run that did not shut down cleanly.
pub fn remove_stale() {
    let Ok(entries) = fs::read_dir(VKMS_DIR) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(PREFIX) {
            eprintln!("removing stale virtual display {name}");
            drop(VkmsDisplay {
                name,
                dir: entry.path(),
                card: PathBuf::new(),
            });
        }
    }
}

/// A virtual display, unplugged and deleted on drop.
pub struct VkmsDisplay {
    pub name: String,
    dir: PathBuf,
    /// `/dev/dri/cardN` of the display.
    pub card: PathBuf,
}

impl VkmsDisplay {
    /// `size`: the size it's viewed at, written as the desktop's mode hint before the
    /// display is plugged in (`write_mode_hint`).
    pub fn create(name: &str, size: Option<(u32, u32)>) -> Result<Self> {
        if let Some((w, h)) = size {
            write_mode_hint(name, w, h)?;
        }
        let dir = Path::new(VKMS_DIR).join(name);
        let before = vkms_cards();
        fs::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
        // From here on, drop cleans up whatever was created.
        let mut display = Self {
            name: name.into(),
            dir,
            card: PathBuf::new(),
        };
        let d = &display.dir;

        // One pipeline: primary plane -> CRTC -> encoder -> connector.
        for sub in [
            "planes/plane0",
            "crtcs/crtc0",
            "encoders/encoder0",
            "connectors/connector0",
        ] {
            fs::create_dir(d.join(sub)).with_context(|| format!("creating {sub}"))?;
        }
        write(d, "planes/plane0/type", "1")?; // DRM_PLANE_TYPE_PRIMARY
        symlink(
            d.join("crtcs/crtc0"),
            d.join("planes/plane0/possible_crtcs/crtc0"),
        )?;
        symlink(
            d.join("crtcs/crtc0"),
            d.join("encoders/encoder0/possible_crtcs/crtc0"),
        )?;
        symlink(
            d.join("encoders/encoder0"),
            d.join("connectors/connector0/possible_encoders/encoder0"),
        )?;
        // Connected (the default on most kernels, explicit for the rest).
        let _ = write(d, "connectors/connector0/status", "1");
        write(d, "enabled", "1")?;

        display.card = wait_for_card(name, &before)?;
        Ok(display)
    }
}

impl Drop for VkmsDisplay {
    fn drop(&mut self) {
        let d = &self.dir;
        let _ = write(d, "enabled", "0");
        let _ = fs::remove_file(Path::new(MODE_HINT_DIR).join(&self.name));
        for link in [
            "connectors/connector0/possible_encoders/encoder0",
            "encoders/encoder0/possible_crtcs/crtc0",
            "planes/plane0/possible_crtcs/crtc0",
        ] {
            let _ = fs::remove_file(d.join(link));
        }
        for sub in [
            "connectors/connector0",
            "encoders/encoder0",
            "crtcs/crtc0",
            "planes/plane0",
        ] {
            let _ = fs::remove_dir(d.join(sub));
        }
        if let Err(e) = fs::remove_dir(d) {
            eprintln!("could not remove {}: {e}", d.display());
        }
    }
}

/// Tells the desktop the size a display is viewed at (`vdisplay::MODE_HINT_DIR`). Replaced
/// in one step (a rename), so a desktop never reads half a file.
pub fn write_mode_hint(name: &str, width: u32, height: u32) -> Result<()> {
    fs::create_dir_all(MODE_HINT_DIR)?;
    fs::set_permissions(MODE_HINT_DIR, fs::Permissions::from_mode(0o755))?;
    let path = Path::new(MODE_HINT_DIR).join(name);
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, format!("{width}x{height}\n"))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
    fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
}

fn write(dir: &Path, attr: &str, value: &str) -> Result<()> {
    fs::write(dir.join(attr), value).with_context(|| format!("writing {value} to {attr}"))
}

/// DRM card nodes (`cardN`) driven by VKMS, with the device name behind each.
fn vkms_cards() -> Vec<(PathBuf, String)> {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let node = e.file_name().to_string_lossy().into_owned();
            let is_card = node
                .strip_prefix("card")
                .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()));
            if !is_card {
                return None;
            }
            // VKMS devices sit on the platform bus on older kernels and on the
            // faux bus (driver `faux_driver`) since Linux 6.15.
            let driver = fs::read_link(e.path().join("device/driver")).ok()?;
            if !matches!(driver.file_name()?.to_str()?, "vkms" | "faux_driver") {
                return None;
            }
            let device = fs::read_link(e.path().join("device")).ok()?;
            Some((
                Path::new("/dev/dri").join(&node),
                device.file_name()?.to_string_lossy().into_owned(),
            ))
        })
        .collect()
}

fn wait_for_card(name: &str, before: &[(PathBuf, String)]) -> Result<PathBuf> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let cards = vkms_cards();
        // The device is named after the configfs directory; fall back to "the new one".
        let found = cards
            .iter()
            .find(|(_, dev)| dev == name || dev.starts_with(&format!("{name}.")));
        let found = found.or_else(|| cards.iter().find(|c| !before.contains(c)));
        if let Some((card, _)) = found
            && card.exists()
        {
            return Ok(card.clone());
        }
        thread::sleep(Duration::from_millis(50));
    }
    bail!("VKMS device {name} was enabled but no DRM card appeared")
}
