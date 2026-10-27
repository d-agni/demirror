//! Reading what the desktop shows on a virtual display, straight from the
//! kernel (KMS framebuffer grab). Needs CAP_SYS_ADMIN, works with any desktop.

use std::{
    fs::File,
    os::{
        fd::{AsFd, BorrowedFd},
        unix::fs::FileExt,
    },
    ptr, slice,
};

use anyhow::{Context, Result, anyhow, bail};
use drm::{
    ClientCapability, Device,
    buffer::{Buffer, DrmFourcc, DrmModifier},
    control::{Device as ControlDevice, ModeTypeFlags, crtc, dumbbuffer::DumbBuffer, framebuffer},
};
use rustix::{
    fs::OFlags,
    mm::{MapFlags, ProtFlags, mmap, munmap},
};
use vmon_proto::vdisplay::Frame;

pub struct Card(File);

/// Buffer, framebuffer and size of the self-test image on screen.
pub type TestImage = (DumbBuffer, framebuffer::Handle, (u32, u32));

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl Device for Card {}
impl ControlDevice for Card {}

impl Card {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let file = File::options()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        let card = Card(file);
        // Being root and the first to open the new device made us its DRM master, and only
        // the master may light a display up. We only read it: let the desktop drive it.
        let _ = card.release_master_lock();
        card.set_client_capability(ClientCapability::UniversalPlanes, true)?;
        Ok(card)
    }

    /// The CRTC that is lit up and the framebuffer it scans out, if any.
    fn scanout(&self) -> Result<Option<framebuffer::Handle>> {
        let resources = self.resource_handles()?;
        let Some(crtc) = resources
            .crtcs()
            .iter()
            .filter_map(|&c| self.get_crtc(c).ok())
            .find(|c| c.mode().is_some())
        else {
            return Ok(None);
        };
        // Atomic desktops attach the image to the primary plane; legacy ones to the CRTC.
        let from_plane = self
            .plane_handles()?
            .into_iter()
            .filter_map(|p| self.get_plane(p).ok())
            .find(|p| p.crtc() == Some(crtc.handle()))
            .and_then(|p| p.framebuffer());
        Ok(from_plane.or(crtc.framebuffer()))
    }

    /// Copies the current image into `out` as tightly packed BGRA.
    pub fn capture(&self, out: &File, scratch: &mut Vec<u8>) -> Result<Frame> {
        let inactive = Frame {
            active: false,
            width: 0,
            height: 0,
            stride: 0,
        };
        let Some(fb) = self.scanout()? else {
            return Ok(inactive);
        };
        let info = match self.get_planar_framebuffer(fb) {
            Ok(info) => info,
            // The desktop swapped buffers and removed this one meanwhile.
            Err(_) => return Ok(inactive),
        };
        let swap_rb = match info.pixel_format() {
            DrmFourcc::Xrgb8888 | DrmFourcc::Argb8888 => false,
            DrmFourcc::Xbgr8888 | DrmFourcc::Abgr8888 => true,
            other => bail!("unsupported pixel format {other:?}"),
        };
        if info
            .modifier()
            .is_some_and(|m| m != DrmModifier::Linear && m != DrmModifier::Invalid)
        {
            bail!("tiled framebuffers are not supported");
        }
        let handle = info.buffers()[0]
            .ok_or_else(|| anyhow!("no buffer handle (the daemon must run as root)"))?;
        let result = self.copy_buffer(handle, &info, swap_rb, out, scratch);
        let _ = self.close_buffer(handle);
        result
    }

    fn copy_buffer(
        &self,
        handle: drm::buffer::Handle,
        info: &framebuffer::PlanarInfo,
        swap_rb: bool,
        out: &File,
        scratch: &mut Vec<u8>,
    ) -> Result<Frame> {
        let (width, height) = info.size();
        let (pitch, offset) = (info.pitches()[0] as usize, info.offsets()[0] as usize);
        let row = width as usize * 4;
        let len = offset + pitch * height as usize;

        let dmabuf = self.buffer_to_prime_fd(handle, OFlags::CLOEXEC.bits())?;
        // SAFETY: a fresh read-only shared mapping, only read below and unmapped before returning.
        let map = unsafe {
            mmap(
                ptr::null_mut(),
                len,
                ProtFlags::READ,
                MapFlags::SHARED,
                &dmabuf,
                0,
            )?
        };
        dma_buf_sync(&dmabuf, DMA_BUF_SYNC_START);
        // SAFETY: `map` is valid for `len` bytes until the munmap below.
        let src = unsafe { slice::from_raw_parts(map.cast::<u8>(), len) };
        scratch.resize(row * height as usize, 0);
        for y in 0..height as usize {
            let dst = &mut scratch[y * row..][..row];
            dst.copy_from_slice(&src[offset + y * pitch..][..row]);
            if swap_rb {
                dst.as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .for_each(|px| px.swap(0, 2));
            }
        }
        dma_buf_sync(&dmabuf, DMA_BUF_SYNC_END);
        // SAFETY: unmapping exactly what was mapped above; `src` is not used after this.
        unsafe { munmap(map, len)? };

        out.write_all_at(scratch, 0)?;
        Ok(Frame {
            active: true,
            width,
            height,
            stride: row as u32,
        })
    }

    /// Lights the display up with a test image when no desktop drives it.
    /// Returns `None` if something else (the desktop) is already in control.
    pub fn show_test_image(&self) -> Result<Option<TestImage>> {
        if self.acquire_master_lock().is_err() {
            return Ok(None);
        }
        let resources = self.resource_handles()?;
        let connector = *resources.connectors().first().context("no connector")?;
        let info = self.get_connector(connector, true)?;
        let mode = info
            .modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or(info.modes().first())
            .copied()
            .context("the connector has no modes")?;
        let (w, h) = (mode.size().0 as u32, mode.size().1 as u32);

        let mut buffer = self.create_dumb_buffer((w, h), DrmFourcc::Xrgb8888, 32)?;
        let pitch = buffer.pitch() as usize;
        {
            let mut map = self.map_dumb_buffer(&mut buffer)?;
            for y in 0..h as usize {
                for x in 0..w as usize {
                    map[y * pitch + x * 4..][..4].copy_from_slice(&test_pixel(x, y).to_le_bytes());
                }
            }
        }
        let fb = self.add_framebuffer(&buffer, 24, 32)?;
        let crtc: crtc::Handle = *resources.crtcs().first().context("no CRTC")?;
        self.set_crtc(crtc, Some(fb), (0, 0), &[connector], Some(mode))?;
        Ok(Some((buffer, fb, (w, h))))
    }
}

/// `0x00RRGGBB` pixel of the self-test image.
pub fn test_pixel(x: usize, y: usize) -> u32 {
    ((x as u32 & 0xff) << 16) | ((y as u32 & 0xff) << 8) | 0x80
}

const DMA_BUF_SYNC_READ: u64 = 1;
const DMA_BUF_SYNC_START: u64 = DMA_BUF_SYNC_READ;
const DMA_BUF_SYNC_END: u64 = 4 | DMA_BUF_SYNC_READ;

/// Brackets CPU reads of a dma-buf so caches are coherent (`DMA_BUF_IOCTL_SYNC`).
fn dma_buf_sync(fd: &impl AsFd, flags: u64) {
    use rustix::ioctl::{Setter, ioctl, opcode};
    // SAFETY: DMA_BUF_IOCTL_SYNC takes a `struct dma_buf_sync { __u64 flags; }`.
    let _ = unsafe {
        ioctl(
            fd,
            Setter::<{ opcode::write::<u64>(b'b', 0) }, u64>::new(flags),
        )
    };
}
