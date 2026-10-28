//! Virtual displays on Windows, through demirror's display driver (packaging/windows/driver).
//!
//! The driver plugs in a monitor of the asked size, Windows extends the desktop onto it like
//! onto any new monitor, and it's captured with GDI like any screen. The monitor lives as
//! long as this source: it's pinged every second, and the driver unplugs monitors that stop
//! being pinged (demirror quit or crashed).

use std::{
    mem, ptr,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use vmon_proto::input::InputEvent;
use windows_sys::{
    Win32::{
        Devices::{DeviceAndDriverInstallation::*, Display::*},
        Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE},
        Graphics::Gdi::*,
        Storage::FileSystem::*,
        System::IO::DeviceIoControl,
    },
    core::GUID,
};

use super::windows::{GdiSource, dpi_aware, monitors};

// protocol.h, the driver's side of these.
const INTERFACE: GUID = GUID::from_u128(0xb3fbedff_97db_4045_81af_e2cf872da1da);
const IOCTL_VERSION: u32 = 0x222000;
const IOCTL_ADD: u32 = 0x222004;
const IOCTL_REMOVE: u32 = 0x222008;
const IOCTL_PING: u32 = 0x22200C;
const IOCTL_RESIZE: u32 = 0x222010;

const NOT_INSTALLED: &str = "the demirror display driver isn't installed (Share page: Install display driver)";

/// An open handle to the driver.
struct Driver(HANDLE);

// Synchronous DeviceIoControl calls on one handle may come from any thread.
unsafe impl Send for Driver {}
unsafe impl Sync for Driver {}

impl Driver {
    fn open() -> Result<Driver> {
        let path = interface_path()?.context(NOT_INSTALLED)?;
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let e = std::io::Error::last_os_error();
            bail!("can't open the demirror display driver: {e}");
        }
        Ok(Driver(handle))
    }

    fn ioctl(&self, code: u32, input: &[u32]) -> Result<Option<u32>> {
        let mut output = 0u32;
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.0,
                code,
                input.as_ptr().cast(),
                mem::size_of_val(input) as u32,
                (&mut output as *mut u32).cast(),
                4,
                &mut returned,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok((returned == 4).then_some(output))
    }

    fn version(&self) -> Result<u32> {
        self.ioctl(IOCTL_VERSION, &[])?.context("no version from the display driver")
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// The driver's device interface, as a null-terminated path, if the driver is running.
fn interface_path() -> Result<Option<Vec<u16>>> {
    let mut len = 0u32;
    let flags = CM_GET_DEVICE_INTERFACE_LIST_PRESENT;
    if unsafe { CM_Get_Device_Interface_List_SizeW(&mut len, &INTERFACE, ptr::null(), flags) } != CR_SUCCESS {
        bail!("can't list display drivers");
    }
    let mut list = vec![0u16; len as usize];
    if unsafe { CM_Get_Device_Interface_ListW(&INTERFACE, ptr::null(), list.as_mut_ptr(), len, flags) } != CR_SUCCESS {
        bail!("can't list display drivers");
    }
    // A list of strings; the first is ours.
    let end = list.iter().position(|&c| c == 0).unwrap_or(0);
    Ok((end > 0).then(|| list[..=end].to_vec()))
}

/// Whether virtual displays can be created, and the driver or the reason why not.
pub fn support() -> (bool, String) {
    match Driver::open().and_then(|d| d.version()) {
        Ok(version) => (true, format!("demirror display driver (protocol {version})")),
        Err(e) => (false, format!("{e:#}")),
    }
}

pub struct VirtualSource {
    driver: Arc<Driver>,
    id: u32,
    output: String,
    screen: GdiSource,
    /// Dropped to stop the pings.
    _pinging: mpsc::Sender<()>,
}

impl VirtualSource {
    pub fn open(width: u32, height: u32) -> Result<Self> {
        dpi_aware();
        let driver = Arc::new(Driver::open()?);
        let names = || monitors().into_iter().map(|(name, _)| name).collect::<Vec<_>>();
        let before = names();
        let id = driver
            .ioctl(IOCTL_ADD, &[width, height])
            .context("the display driver didn't add a monitor")?
            .context("no monitor id from the display driver")?;
        let pinging = ping(driver.clone(), id);

        // Windows extends the desktop onto a new monitor by itself, unless it's set to show
        // only one screen or to duplicate: then ask it to extend.
        let started = Instant::now();
        let mut extended = false;
        let output = loop {
            if let Some(new) = names().into_iter().find(|n| !before.contains(n)) {
                break new;
            }
            if !extended && started.elapsed() > Duration::from_secs(3) {
                extended = true;
                unsafe { SetDisplayConfig(0, ptr::null(), 0, ptr::null(), SDC_TOPOLOGY_EXTEND | SDC_APPLY) };
            }
            if started.elapsed() > Duration::from_secs(10) {
                let _ = driver.ioctl(IOCTL_REMOVE, &[id]);
                bail!("the monitor was added but Windows didn't extend the desktop onto it");
            }
            thread::sleep(Duration::from_millis(200));
        };
        tracing::info!("virtual display {output} plugged in");
        // Windows picks the monitor's only mode; make sure, if it remembered another.
        set_mode(&output, width, height);
        let screen = GdiSource::open(Some(&output))?;
        Ok(Self { driver, id, output, screen, _pinging: pinging })
    }
}

/// Pings monitor `id` every second until the returned sender is dropped.
fn ping(driver: Arc<Driver>, id: u32) -> mpsc::Sender<()> {
    let (stop, stopped) = mpsc::channel::<()>();
    thread::spawn(move || {
        while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(Duration::from_secs(1)) {
            if let Err(e) = driver.ioctl(IOCTL_PING, &[id]) {
                tracing::warn!("virtual display {id} is gone: {e}");
                return;
            }
        }
    });
    stop
}

/// Switches `output` to `width` x `height`, if it isn't already.
fn set_mode(output: &str, width: u32, height: u32) {
    let name: Vec<u16> = output.encode_utf16().chain([0]).collect();
    unsafe {
        let mut mode: DEVMODEW = mem::zeroed();
        mode.dmSize = mem::size_of::<DEVMODEW>() as u16;
        if EnumDisplaySettingsW(name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode) != 0
            && (mode.dmPelsWidth, mode.dmPelsHeight) == (width, height)
        {
            return;
        }
        let mut mode: DEVMODEW = mem::zeroed();
        mode.dmSize = mem::size_of::<DEVMODEW>() as u16;
        mode.dmPelsWidth = width;
        mode.dmPelsHeight = height;
        mode.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
        let result = ChangeDisplaySettingsExW(name.as_ptr(), &mode, ptr::null_mut(), CDS_UPDATEREGISTRY, ptr::null());
        if result != DISP_CHANGE_SUCCESSFUL {
            tracing::warn!("{output} didn't switch to {width}x{height} ({result})");
        }
    }
}

impl super::Source for VirtualSource {
    fn capture(&mut self, out: &mut Vec<u8>) -> Result<(u32, u32)> {
        self.screen.capture(out)
    }

    /// The monitor is offered in the new size only, and switched to it.
    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.driver.ioctl(IOCTL_RESIZE, &[self.id, width, height]).context("the display driver didn't resize")?;
        set_mode(&self.output, width, height);
        Ok(())
    }

    fn input(&mut self) -> Option<mpsc::Sender<InputEvent>> {
        super::windows::input(self.output.clone())
    }
}

impl Drop for VirtualSource {
    fn drop(&mut self) {
        if let Err(e) = self.driver.ioctl(IOCTL_REMOVE, &[self.id]) {
            tracing::warn!("removing virtual display {}: {e}", self.output);
        }
    }
}
