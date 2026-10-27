//! Windows screen capture with GDI, pure Rust through `windows-sys`.
//!
//! Any display Windows knows about can be streamed, including one added by a
//! virtual display driver, which is how a monitor extends the desktop on Windows.

use std::{mem, ptr, sync::Once};

use anyhow::{Result, anyhow, bail};
use vmon_proto::OutputInfo;
use windows_sys::Win32::{
    Foundation::{LPARAM, RECT, TRUE},
    Graphics::Gdi::*,
    UI::{HiDpi::*, WindowsAndMessaging::*},
};

/// Without this Windows reports scaled (not physical) monitor sizes.
fn dpi_aware() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    });
}

fn monitors() -> Vec<(String, RECT)> {
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

pub fn list_outputs() -> Result<Vec<OutputInfo>> {
    Ok(monitors()
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
