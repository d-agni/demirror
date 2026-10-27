//! Wayland screen capture through `wlr-screencopy-unstable-v1`, pure Rust.
//!
//! Supported by wlroots compositors (sway, Hyprland, river, ...), kompact and
//! others. Combined with a compositor that can create headless outputs this
//! gives a real extra monitor; otherwise it mirrors an existing screen.

use std::{fs::File, os::fd::AsFd, os::unix::fs::FileExt};

use anyhow::{Context, Result, anyhow, bail};
use rustix::fs::{MemfdFlags, ftruncate, memfd_create};
use vmon_proto::OutputInfo;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_buffer::WlBuffer, wl_output, wl_registry, wl_shm, wl_shm_pool::WlShmPool},
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

struct Output {
    output: wl_output::WlOutput,
    name: Option<String>,
    description: String,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct FrameState {
    /// The `wl_shm` buffer the compositor wants: format, width, height, stride.
    shm: Option<(wl_shm::Format, u32, u32, u32)>,
    buffer_done: bool,
    y_invert: bool,
    ready: bool,
    failed: bool,
}

#[derive(Default)]
struct State {
    outputs: Vec<Output>,
    frame: FrameState,
}

/// A connection with the globals this backend needs.
struct Session {
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    shm: Option<wl_shm::WlShm>,
    screencopy: Option<ZwlrScreencopyManagerV1>,
}

impl Session {
    fn connect() -> Result<Self> {
        let conn =
            Connection::connect_to_env().context("no Wayland session (is WAYLAND_DISPLAY set?)")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let mut state = State::default();
        let registry = globals.registry();
        for global in globals.contents().clone_list() {
            if global.interface == "wl_output" {
                let output =
                    registry.bind(global.name, global.version.min(4), &qh, state.outputs.len());
                state.outputs.push(Output {
                    output,
                    name: None,
                    description: String::new(),
                    width: 0,
                    height: 0,
                });
            }
        }
        let shm = globals.bind(&qh, 1..=1, ()).ok();
        let screencopy = globals.bind(&qh, 1..=3, ()).ok();
        queue.roundtrip(&mut state)?;
        Ok(Self {
            queue,
            qh,
            state,
            shm,
            screencopy,
        })
    }

    fn output_infos(&self) -> Vec<OutputInfo> {
        self.state
            .outputs
            .iter()
            .enumerate()
            .map(|(i, o)| OutputInfo {
                name: o.name.clone().unwrap_or_else(|| format!("output-{i}")),
                description: o.description.clone(),
                width: o.width,
                height: o.height,
            })
            .collect()
    }
}

pub fn list_outputs() -> Result<Vec<OutputInfo>> {
    Ok(Session::connect()?.output_infos())
}

/// A shared-memory buffer reused while the frame layout stays the same.
struct ShmBuffer {
    file: File,
    buffer: WlBuffer,
    layout: (wl_shm::Format, u32, u32, u32),
}

pub struct WlrSource {
    session: Session,
    output: wl_output::WlOutput,
    screencopy: ZwlrScreencopyManagerV1,
    shm: wl_shm::WlShm,
    buffer: Option<ShmBuffer>,
    raw: Vec<u8>,
}

impl WlrSource {
    pub fn open(name: Option<&str>) -> Result<Self> {
        let session = Session::connect()?;
        let screencopy = session
            .screencopy
            .clone()
            .ok_or_else(|| anyhow!("the compositor does not offer wlr-screencopy"))?;
        let shm = session
            .shm
            .clone()
            .ok_or_else(|| anyhow!("the compositor does not offer wl_shm"))?;
        let infos = session.output_infos();
        let index = match name {
            Some(name) => infos
                .iter()
                .position(|o| o.name == name)
                .ok_or_else(|| anyhow!("no output named {name:?}"))?,
            None if infos.is_empty() => bail!("the compositor has no outputs"),
            None => 0,
        };
        let output = session.state.outputs[index].output.clone();
        Ok(Self {
            session,
            output,
            screencopy,
            shm,
            buffer: None,
            raw: Vec::new(),
        })
    }

    fn dispatch_until(&mut self, done: impl Fn(&FrameState) -> bool) -> Result<()> {
        while !done(&self.session.state.frame) {
            if self.session.state.frame.failed {
                bail!("the compositor failed to copy the frame");
            }
            self.session
                .queue
                .blocking_dispatch(&mut self.session.state)?;
        }
        Ok(())
    }

    fn buffer_for(&mut self, layout: (wl_shm::Format, u32, u32, u32)) -> Result<&ShmBuffer> {
        if self.buffer.as_ref().is_none_or(|b| b.layout != layout) {
            if let Some(old) = self.buffer.take() {
                old.buffer.destroy();
            }
            let (format, width, height, stride) = layout;
            let size = stride * height;
            let fd = memfd_create("demirror-frame", MemfdFlags::CLOEXEC)?;
            ftruncate(&fd, size.into())?;
            let pool = self
                .shm
                .create_pool(fd.as_fd(), size as i32, &self.session.qh, ());
            let buffer = pool.create_buffer(
                0,
                width as i32,
                height as i32,
                stride as i32,
                format,
                &self.session.qh,
                (),
            );
            pool.destroy();
            self.buffer = Some(ShmBuffer {
                file: File::from(fd),
                buffer,
                layout,
            });
        }
        Ok(self.buffer.as_ref().unwrap())
    }
}

impl super::Source for WlrSource {
    fn capture(&mut self, out: &mut Vec<u8>) -> Result<(u32, u32)> {
        self.session.state.frame = FrameState::default();
        let frame = self
            .screencopy
            .capture_output(1, &self.output, &self.session.qh, ());
        // Version 3 announces every buffer type and then `buffer_done`; older
        // versions only send the single shm `buffer` event.
        let v3 = self.screencopy.version() >= 3;
        let result = (|| {
            self.dispatch_until(|f| if v3 { f.buffer_done } else { f.shm.is_some() })?;
            let layout = self
                .session
                .state
                .frame
                .shm
                .ok_or_else(|| anyhow!("the compositor offers no shm buffer"))?;
            let buffer = self.buffer_for(layout)?.buffer.clone();
            frame.copy(&buffer);
            self.dispatch_until(|f| f.ready)?;

            let (format, width, height, stride) = layout;
            self.raw.resize((stride * height) as usize, 0);
            self.buffer
                .as_ref()
                .unwrap()
                .file
                .read_exact_at(&mut self.raw, 0)?;

            let swap_rb = match format {
                wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => false,
                wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888 => true,
                other => bail!("unsupported pixel format {other:?}"),
            };
            let row_len = (width * 4) as usize;
            out.resize(row_len * height as usize, 0);
            for y in 0..height as usize {
                let src_y = if self.session.state.frame.y_invert {
                    height as usize - 1 - y
                } else {
                    y
                };
                let src = &self.raw[src_y * stride as usize..][..row_len];
                let dst = &mut out[y * row_len..][..row_len];
                dst.copy_from_slice(src);
                if swap_rb {
                    dst.as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .for_each(|px| px.swap(0, 2));
                }
            }
            Ok((width, height))
        })();
        frame.destroy();
        result
    }
}

impl Drop for WlrSource {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            buffer.buffer.destroy();
        }
        let _ = self.session.queue.flush();
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let output = &mut state.outputs[*index];
        match event {
            wl_output::Event::Name { name } => output.name = Some(name),
            wl_output::Event::Description { description } => output.description = description,
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) => {
                output.width = width as u32;
                output.height = height as u32;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let frame = &mut state.frame;
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => {
                frame.shm = Some((format, width, height, stride));
            }
            zwlr_screencopy_frame_v1::Event::Flags {
                flags: WEnum::Value(flags),
            } => {
                frame.y_invert = flags.contains(zwlr_screencopy_frame_v1::Flags::YInvert);
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => frame.buffer_done = true,
            zwlr_screencopy_frame_v1::Event::Ready { .. } => frame.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => frame.failed = true,
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ZwlrScreencopyManagerV1);
