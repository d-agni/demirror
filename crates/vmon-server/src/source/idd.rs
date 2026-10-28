//! Virtual displays on Windows, through demirror's display driver (packaging/windows/driver).
//!
//! The driver plugs in a monitor of the asked size, Windows extends the desktop onto it like
//! onto any new monitor, and the driver shares the frames Windows draws for it (a screen
//! read with GDI can stay black for such a monitor; it's only the fallback). The monitor lives as
//! long as this source: it's pinged every second, and the driver unplugs monitors that stop
//! being pinged (demirror quit or crashed).

use std::{
    mem, ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering, fence},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use vmon_proto::{Placement, input::InputEvent};
use windows_sys::{
    Win32::{
        Devices::{DeviceAndDriverInstallation::*, Display::*},
        Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, POINTL},
        Graphics::Gdi::*,
        Storage::FileSystem::*,
        System::{IO::DeviceIoControl, Memory::*},
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
// IOCTL_RESIZE (0x222010) isn't used: Windows turns a monitor off when its modes change, so
// resizing replaces the monitor instead.

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
    placement: Placement,
    /// The virtual display is the main display while it's on.
    main: bool,
    /// The main display before this one was plugged in: what it's placed beside, and what's
    /// made the main display again when it goes.
    previous_main: Option<String>,
    plugged: Plugged,
    /// The display's name for input, which follows the display when it's replaced.
    input_output: Arc<Mutex<String>>,
    /// Whether a frame came from the driver yet; until then the screen is read with GDI.
    from_driver: bool,
    last: (u32, u32),
}

impl VirtualSource {
    pub fn open(width: u32, height: u32, placement: Placement, main: bool) -> Result<Self> {
        dpi_aware();
        let driver = Arc::new(Driver::open()?);
        let previous_main = main_display();
        let plugged = Plugged::new(&driver, width, height)?;
        let input_output = Arc::new(Mutex::new(plugged.output.clone()));
        let source = Self {
            driver,
            placement,
            main,
            previous_main,
            plugged,
            input_output,
            from_driver: false,
            last: (width, height),
        };
        source.place(width, height);
        Ok(source)
    }

    /// Puts the display beside the previous main one, and makes it the main one if asked.
    /// Otherwise the previous one stays main: Windows remembers a main display per set of
    /// monitors, and would make this one main again if it once was.
    fn place(&self, width: u32, height: u32) {
        let output = &self.plugged.output;
        arrange(output, width, height, self.placement, self.previous_main.as_deref());
        if self.main {
            make_main(output);
        } else if let Some(previous) = &self.previous_main
            && main_display().as_ref() != Some(previous)
        {
            make_main(previous);
        }
    }
}

/// The main display's name: the one at the desktop's origin.
fn main_display() -> Option<String> {
    monitors().into_iter().find(|(_, r)| r.left == 0 && r.top == 0).map(|(name, _)| name)
}

/// Makes `output` the main display.
fn make_main(output: &str) {
    match unsafe { set_main(output) } {
        Ok(()) => tracing::info!("{output} is the main display"),
        Err(e) => tracing::warn!("{output} can't be made the main display: {e:#}"),
    }
}

/// The main display is the one at the desktop's origin: every display moves by as much as
/// `output` does to get there, so the arrangement stays. (As Windows' Settings do it;
/// ChangeDisplaySettingsEx's CDS_SET_PRIMARY fails for some displays.)
unsafe fn set_main(output: &str) -> Result<()> {
    unsafe {
        let flags = QDC_ONLY_ACTIVE_PATHS;
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count) != 0 {
            bail!("can't read the display arrangement");
        }
        let mut paths = vec![mem::zeroed::<DISPLAYCONFIG_PATH_INFO>(); path_count as usize];
        let mut modes = vec![mem::zeroed::<DISPLAYCONFIG_MODE_INFO>(); mode_count as usize];
        let read = QueryDisplayConfig(
            flags,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            ptr::null_mut(),
        );
        if read != 0 {
            bail!("can't read the display arrangement ({read})");
        }
        paths.truncate(path_count as usize);
        modes.truncate(mode_count as usize);

        let origin = paths
            .iter()
            .find(|path| {
                let mut name: DISPLAYCONFIG_SOURCE_DEVICE_NAME = mem::zeroed();
                name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
                name.header.size = mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
                name.header.adapterId = path.sourceInfo.adapterId;
                name.header.id = path.sourceInfo.id;
                let gdi = &name.viewGdiDeviceName;
                DisplayConfigGetDeviceInfo(&mut name.header) == 0
                    && String::from_utf16_lossy(&gdi[..gdi.iter().position(|&c| c == 0).unwrap_or(gdi.len())]) == output
            })
            .and_then(|path| modes.get(path.sourceInfo.Anonymous.modeInfoIdx as usize))
            .filter(|mode| mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE)
            .map(|mode| mode.Anonymous.sourceMode.position)
            .with_context(|| format!("{output} isn't on the desktop"))?;
        for mode in &mut modes {
            if mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
                let position = &mut mode.Anonymous.sourceMode.position;
                position.x -= origin.x;
                position.y -= origin.y;
            }
        }
        let applied = SetDisplayConfig(
            path_count,
            paths.as_ptr(),
            mode_count,
            modes.as_ptr(),
            SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_SAVE_TO_DATABASE | SDC_ALLOW_CHANGES,
        );
        if applied != 0 {
            bail!("Windows didn't take the new arrangement ({applied})");
        }
        Ok(())
    }
}

/// A monitor the driver plugged in, and Windows put on the desktop.
struct Plugged {
    id: u32,
    output: String,
    /// The frames the driver copies out, if it could share them.
    frames: Option<SharedFrames>,
    screen: GdiSource,
    /// Dropped to stop the pings.
    _pinging: mpsc::Sender<()>,
}

impl Plugged {
    fn new(driver: &Arc<Driver>, width: u32, height: u32) -> Result<Plugged> {
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
        tracing::info!("virtual display {output} plugged in at {width}x{height}");
        let frames = SharedFrames::open(id);
        if frames.is_none() {
            tracing::warn!("the display driver doesn't share {output}'s frames; reading the screen instead");
        }
        let screen = match GdiSource::open(Some(&output)) {
            Ok(screen) => screen,
            Err(e) => {
                let _ = driver.ioctl(IOCTL_REMOVE, &[id]);
                return Err(e);
            }
        };
        Ok(Plugged { id, output, frames, screen, _pinging: pinging })
    }

    fn unplug(self, driver: &Driver) {
        drop(self._pinging);
        if let Err(e) = driver.ioctl(IOCTL_REMOVE, &[self.id]) {
            tracing::warn!("removing virtual display {}: {e}", self.output);
        }
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

/// Sets `output` to `width` x `height` (Windows picks the monitor's only mode, unless it
/// remembered another) and puts it at `placement` beside `beside` (or the main display).
fn arrange(output: &str, width: u32, height: u32, placement: Placement, beside: Option<&str>) {
    let all = monitors();
    let main = match beside.and_then(|b| all.iter().find(|(n, _)| n == b)) {
        Some((_, r)) => Some(*r),
        None => all.iter().map(|(_, r)| *r).find(|r| r.left == 0 && r.top == 0),
    };
    let (w, h) = (width as i32, height as i32);
    let position = main.map(|m| match placement {
        Placement::Right => (m.right, m.top),
        Placement::Left => (m.left - w, m.top),
        Placement::Above => (m.left, m.top - h),
        Placement::Below => (m.left, m.bottom),
    });
    let name: Vec<u16> = output.encode_utf16().chain([0]).collect();
    unsafe {
        let mut mode: DEVMODEW = mem::zeroed();
        mode.dmSize = mem::size_of::<DEVMODEW>() as u16;
        mode.dmPelsWidth = width;
        mode.dmPelsHeight = height;
        mode.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
        if let Some((x, y)) = position {
            mode.Anonymous1.Anonymous2.dmPosition = POINTL { x, y };
            mode.dmFields |= DM_POSITION;
        }
        let flags = CDS_UPDATEREGISTRY | CDS_NORESET;
        let result = ChangeDisplaySettingsExW(name.as_ptr(), &mode, ptr::null_mut(), flags, ptr::null());
        if result != DISP_CHANGE_SUCCESSFUL {
            tracing::warn!("{output} can't be {width}x{height} at {position:?} ({result})");
            return;
        }
        // Applies what was set above.
        ChangeDisplaySettingsExW(ptr::null(), ptr::null(), ptr::null_mut(), 0, ptr::null());
    }
}

/// A monitor's frames, as the driver copies them out (protocol.h, DEMIRROR_FRAME).
struct SharedFrames {
    mapping: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

const FRAME_PIXELS: usize = 64;

impl SharedFrames {
    fn open(id: u32) -> Option<SharedFrames> {
        let name: Vec<u16> = format!(r"Global\demirror-idd-frame-{id}").encode_utf16().chain([0]).collect();
        unsafe {
            let mapping = OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr());
            if mapping.is_null() {
                return None;
            }
            let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
            if view.Value.is_null() {
                CloseHandle(mapping);
                return None;
            }
            Some(SharedFrames { mapping, view })
        }
    }

    /// Copies the latest frame into `out` and returns its size; `None` before the first.
    fn read(&self, out: &mut Vec<u8>) -> Option<(u32, u32)> {
        let base = self.view.Value as *const u8;
        // Odd while the driver writes a frame: copy, and keep it if nothing changed meanwhile.
        let sequence = unsafe { &*(base as *const AtomicU64) };
        for _ in 0..100 {
            let before = sequence.load(Ordering::Acquire);
            if before & 1 == 1 {
                thread::yield_now();
                continue;
            }
            let (frames, width, height, capacity) = unsafe {
                (
                    ptr::read_volatile(base.add(8) as *const u64),
                    ptr::read_volatile(base.add(16) as *const u32),
                    ptr::read_volatile(base.add(20) as *const u32),
                    ptr::read_volatile(base.add(24) as *const u32),
                )
            };
            let len = width as usize * height as usize * 4;
            if frames == 0 || len == 0 || len > capacity as usize {
                return None;
            }
            out.resize(len, 0);
            unsafe { ptr::copy_nonoverlapping(base.add(FRAME_PIXELS), out.as_mut_ptr(), len) };
            fence(Ordering::Acquire);
            if sequence.load(Ordering::Acquire) == before {
                return Some((width, height));
            }
        }
        None
    }
}

impl Drop for SharedFrames {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.view);
            CloseHandle(self.mapping);
        }
    }
}

impl super::Source for VirtualSource {
    fn capture(&mut self, out: &mut Vec<u8>) -> Result<(u32, u32)> {
        let plugged = &mut self.plugged;
        let mut frame = plugged.frames.as_ref().and_then(|f| f.read(out));
        // Windows draws a new monitor's first frame within moments; the screen read with GDI
        // meanwhile is black.
        if frame.is_none() && !self.from_driver && plugged.frames.is_some() {
            let started = Instant::now();
            while frame.is_none() && started.elapsed() < Duration::from_secs(2) {
                thread::sleep(Duration::from_millis(20));
                frame = plugged.frames.as_ref().and_then(|f| f.read(out));
            }
        }
        match frame {
            Some(size) => {
                if !self.from_driver {
                    tracing::info!("frames for {} come from the display driver", plugged.output);
                    self.from_driver = true;
                }
                self.last = size;
            }
            // Between frames being written: the last one stays.
            None if self.from_driver => out.resize(self.last.0 as usize * self.last.1 as usize * 4, 0),
            None => self.last = plugged.screen.capture(out)?,
        }
        Ok(self.last)
    }

    /// A monitor of the new size replaces this one. Windows only reads a monitor's modes
    /// when it's plugged in: changing them afterwards makes it turn the monitor off.
    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let plugged = Plugged::new(&self.driver, width, height)?;
        *self.input_output.lock().unwrap() = plugged.output.clone();
        mem::replace(&mut self.plugged, plugged).unplug(&self.driver);
        // Placed once the old one is gone: while both were there, Windows moved it aside.
        self.place(width, height);
        self.from_driver = false;
        Ok(())
    }

    fn input(&mut self) -> Option<mpsc::Sender<InputEvent>> {
        super::windows::input(self.input_output.clone())
    }
}

impl Drop for VirtualSource {
    fn drop(&mut self) {
        // The main display goes back first, so Windows doesn't pick one.
        if self.main
            && let Some(previous) = &self.previous_main
            && main_display().as_ref() != Some(previous)
        {
            make_main(previous);
        }
        // No ping after the remove.
        self.plugged._pinging = mpsc::channel().0;
        if let Err(e) = self.driver.ioctl(IOCTL_REMOVE, &[self.plugged.id]) {
            tracing::warn!("removing virtual display {}: {e}", self.plugged.output);
        }
    }
}
