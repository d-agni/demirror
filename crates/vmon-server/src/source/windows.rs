//! Windows screen capture with GDI, pure Rust through `windows-sys`.
//!
//! Any display Windows knows about can be streamed, including one added by a
//! virtual display driver, which is how a monitor extends the desktop on Windows.

use std::{
    mem, ptr,
    sync::{Once, mpsc},
    thread,
};

use anyhow::{Result, anyhow, bail};
use vmon_proto::{
    OutputInfo,
    input::{Button, InputEvent, pc_scancode},
};
use windows_sys::Win32::{
    Foundation::{LPARAM, RECT, TRUE},
    Graphics::Gdi::*,
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

/// Without this Windows reports scaled (not physical) monitor sizes.
pub(super) fn dpi_aware() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    });
}

pub(super) fn monitors() -> Vec<(String, RECT)> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> i32 {
        unsafe {
            let list = &mut *(data as *mut Vec<(String, RECT)>);
            let mut info: MONITORINFOEXW = mem::zeroed();
            info.monitorInfo.cbSize = mem::size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) != 0 {
                let len = info
                    .szDevice
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(info.szDevice.len());
                list.push((
                    String::from_utf16_lossy(&info.szDevice[..len]),
                    info.monitorInfo.rcMonitor,
                ));
            }
            TRUE
        }
    }
    dpi_aware();
    let mut list = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            Some(collect),
            &mut list as *mut _ as LPARAM,
        )
    };
    list
}

/// The screens, the main one (at the desktop's origin) first.
pub fn list_outputs() -> Result<Vec<OutputInfo>> {
    let mut all = monitors();
    all.sort_by_key(|(_, r)| (r.left, r.top) != (0, 0));
    Ok(all
        .into_iter()
        .map(|(name, r)| OutputInfo {
            description: format!(
                "{}x{} at {},{}",
                r.right - r.left,
                r.bottom - r.top,
                r.left,
                r.top
            ),
            name,
            width: (r.right - r.left) as u32,
            height: (r.bottom - r.top) as u32,
        })
        .collect())
}

pub struct GdiSource {
    name: String,
    screen_dc: HDC,
    mem_dc: HDC,
    bitmap: HBITMAP,
    size: (i32, i32),
}

impl GdiSource {
    pub fn open(name: Option<&str>) -> Result<Self> {
        let all = monitors();
        let name = match name {
            Some(name) => all
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(n, _)| n.clone())
                .ok_or_else(|| anyhow!("no display named {name:?}"))?,
            None => all
                .first()
                .map(|(n, _)| n.clone())
                .ok_or_else(|| anyhow!("no displays found"))?,
        };
        unsafe {
            let screen_dc = GetDC(ptr::null_mut());
            let mem_dc = CreateCompatibleDC(screen_dc);
            Ok(Self {
                name,
                screen_dc,
                mem_dc,
                bitmap: ptr::null_mut(),
                size: (0, 0),
            })
        }
    }
}

impl super::Source for GdiSource {
    /// Mirrored, the screen is worked from the device like a touchscreen.
    fn input(&mut self) -> Option<mpsc::Sender<InputEvent>> {
        input(self.name.clone())
    }

    fn capture(&mut self, out: &mut Vec<u8>) -> Result<(u32, u32)> {
        // Looked up on every frame: displays can move, resize or disappear.
        let Some((_, rect)) = monitors().into_iter().find(|(n, _)| *n == self.name) else {
            bail!("display {} is gone", self.name);
        };
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        unsafe {
            if self.size != (w, h) {
                if !self.bitmap.is_null() {
                    DeleteObject(self.bitmap);
                }
                self.bitmap = CreateCompatibleBitmap(self.screen_dc, w, h);
                SelectObject(self.mem_dc, self.bitmap);
                self.size = (w, h);
            }
            if BitBlt(
                self.mem_dc,
                0,
                0,
                w,
                h,
                self.screen_dc,
                rect.left,
                rect.top,
                SRCCOPY | CAPTUREBLT,
            ) == 0
            {
                bail!("BitBlt failed (is the desktop locked?)");
            }
            draw_cursor(self.mem_dc, &rect);

            let mut info: BITMAPINFO = mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..mem::zeroed()
            };
            out.resize((w * h * 4) as usize, 0);
            let rows = GetDIBits(
                self.mem_dc,
                self.bitmap,
                0,
                h as u32,
                out.as_mut_ptr().cast(),
                &mut info,
                DIB_RGB_COLORS,
            );
            if rows != h {
                bail!("GetDIBits failed");
            }
        }
        Ok((w as u32, h as u32))
    }
}

/// GDI captures do not include the mouse pointer, so draw it in.
unsafe fn draw_cursor(dc: HDC, rect: &RECT) {
    unsafe {
        let mut cursor: CURSORINFO = mem::zeroed();
        cursor.cbSize = mem::size_of::<CURSORINFO>() as u32;
        if GetCursorInfo(&mut cursor) == 0 || cursor.flags & CURSOR_SHOWING == 0 {
            return;
        }
        let mut icon: ICONINFO = mem::zeroed();
        if GetIconInfo(cursor.hCursor, &mut icon) == 0 {
            return;
        }
        let x = cursor.ptScreenPos.x - rect.left - icon.xHotspot as i32;
        let y = cursor.ptScreenPos.y - rect.top - icon.yHotspot as i32;
        DrawIconEx(
            dc,
            x,
            y,
            cursor.hCursor,
            0,
            0,
            0,
            ptr::null_mut(),
            DI_NORMAL,
        );
        if !icon.hbmMask.is_null() {
            DeleteObject(icon.hbmMask);
        }
        if !icon.hbmColor.is_null() {
            DeleteObject(icon.hbmColor);
        }
    }
}

impl Drop for GdiSource {
    fn drop(&mut self) {
        unsafe {
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            DeleteDC(self.mem_dc);
            ReleaseDC(ptr::null_mut(), self.screen_dc);
        }
    }
}

/// Viewers' input for the display `name`: clicked and typed on this desktop with `SendInput`,
/// the pointer placed on that display. (Windows doesn't let it reach programs running as
/// administrator.)
pub(super) fn input(name: String) -> Option<mpsc::Sender<InputEvent>> {
    let (events, received) = mpsc::channel::<InputEvent>();
    thread::Builder::new()
        .name(format!("input-{name}"))
        .spawn(move || {
            for event in received {
                for input in inputs(&name, &event) {
                    unsafe { SendInput(1, &input, mem::size_of::<INPUT>() as i32) };
                }
            }
        })
        .ok()?;
    Some(events)
}

fn inputs(display: &str, event: &InputEvent) -> Vec<INPUT> {
    match *event {
        InputEvent::Move { x, y } => {
            let Some((_, r)) = monitors().into_iter().find(|(n, _)| n == display) else {
                return Vec::new();
            };
            // Absolute positions span the whole desktop, 0..=65535 on each axis.
            let (left, top, width, height) = unsafe {
                (
                    GetSystemMetrics(SM_XVIRTUALSCREEN),
                    GetSystemMetrics(SM_YVIRTUALSCREEN),
                    GetSystemMetrics(SM_CXVIRTUALSCREEN),
                    GetSystemMetrics(SM_CYVIRTUALSCREEN),
                )
            };
            let px = r.left as f32 + x * (r.right - r.left - 1) as f32 - left as f32;
            let py = r.top as f32 + y * (r.bottom - r.top - 1) as f32 - top as f32;
            let nx = (px * 65535.0 / (width - 1).max(1) as f32).round() as i32;
            let ny = (py * 65535.0 / (height - 1).max(1) as f32).round() as i32;
            vec![mouse(nx, ny, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK)]
        }
        InputEvent::Button { button, down } => {
            let flags = match (button, down) {
                (Button::Left, true) => MOUSEEVENTF_LEFTDOWN,
                (Button::Left, false) => MOUSEEVENTF_LEFTUP,
                (Button::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                (Button::Right, false) => MOUSEEVENTF_RIGHTUP,
                (Button::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                (Button::Middle, false) => MOUSEEVENTF_MIDDLEUP,
            };
            vec![mouse(0, 0, 0, flags)]
        }
        // Both in 1/120ths of a notch; Windows scrolls down for a negative wheel.
        InputEvent::Wheel { dx, dy } => [(-dy, MOUSEEVENTF_WHEEL), (dx, MOUSEEVENTF_HWHEEL)]
            .into_iter()
            .filter(|(amount, _)| *amount != 0)
            .map(|(amount, flags)| mouse(0, 0, amount, flags))
            .collect(),
        InputEvent::Key { ref code, down } => {
            let up = if down { 0 } else { KEYEVENTF_KEYUP };
            let key = if code == "Pause" {
                Some((VK_PAUSE, 0, up))
            } else {
                pc_scancode(code).map(|(scan, extended)| {
                    let extended = if extended { KEYEVENTF_EXTENDEDKEY } else { 0 };
                    (0, scan, KEYEVENTF_SCANCODE | extended | up)
                })
            };
            key.map(|(vk, scan, flags)| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
                },
            })
            .into_iter()
            .collect()
        }
    }
}

fn mouse(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}
