//! Window that shows a monitor's stream (winit + softbuffer, no GPU needed).

use std::{
    io::Cursor,
    num::NonZeroU32,
    rc::Rc,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use softbuffer::Surface;
use tungstenite::{Bytes, Message};
use vmon_proto::MonitorInfo;
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, KeyEvent, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Fullscreen, Window, WindowId},
};
use zune_jpeg::{
    JpegDecoder,
    zune_core::{colorspace::ColorSpace, options::DecoderOptions},
};

use crate::Socket;

enum StreamEvent {
    Frame,
    Ended(String),
}

/// Shows the stream until the window is closed, Esc is pressed or the stream ends.
pub fn run(info: MonitorInfo, mut socket: Socket) -> Result<()> {
    let event_loop = EventLoop::<StreamEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();

    // The network thread keeps only the newest frame and wakes the window.
    let latest: Arc<Mutex<Option<Bytes>>> = Arc::default();
    {
        let latest = latest.clone();
        thread::spawn(move || {
            let reason = loop {
                match socket.read() {
                    Ok(Message::Binary(jpeg)) => {
                        *latest.lock().unwrap() = Some(jpeg);
                        if proxy.send_event(StreamEvent::Frame).is_err() {
                            return; // window closed
                        }
                    }
                    Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => {
                        break "the monitor was closed".to_string();
                    }
                    Ok(_) => {}
                    Err(e) => break format!("connection lost: {e}"),
                }
            };
            let _ = proxy.send_event(StreamEvent::Ended(reason));
        });
    }

    let mut app = Viewer {
        info,
        latest,
        window: None,
        surface: None,
        frame: Vec::new(),
        frame_size: (0, 0),
        frames: 0,
        since: Instant::now(),
        ended: None,
    };
    event_loop.run_app(&mut app)?;
    if let Some(reason) = app.ended {
        println!("Stream ended: {reason}");
    }
    Ok(())
}

struct Viewer {
    info: MonitorInfo,
    latest: Arc<Mutex<Option<Bytes>>>,
    window: Option<Rc<Window>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
    /// Last decoded frame as `0RGB` pixels.
    frame: Vec<u32>,
    frame_size: (usize, usize),
    frames: u32,
    since: Instant,
    ended: Option<String>,
}

impl ApplicationHandler<StreamEvent> for Viewer {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // Start at the monitor's size, shrunk to fit a typical screen.
        let (w, h) = (self.info.width as f64, self.info.height as f64);
        let scale = (1280.0 / w).min(800.0 / h).min(1.0);
        let attributes = Window::default_attributes()
            .with_title(format!("{} - demirror", self.info.name))
            .with_inner_size(LogicalSize::new(w * scale, h * scale));
        let created = (|| -> Result<_> {
            let window = Rc::new(event_loop.create_window(attributes)?);
            let context = softbuffer::Context::new(window.clone()).map_err(other)?;
            let surface = Surface::new(&context, window.clone()).map_err(other)?;
            Ok((window, surface))
        })();
        match created {
            Ok((window, surface)) => {
                self.window = Some(window);
                self.surface = Some(surface);
            }
            Err(e) => {
                self.ended = Some(format!("could not open a window: {e}"));
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: StreamEvent) {
        match event {
            StreamEvent::Frame => {
                let Some(jpeg) = self.latest.lock().unwrap().take() else {
                    return;
                };
                match decode(&jpeg, &mut self.frame) {
                    Ok(size) => self.frame_size = size,
                    Err(e) => eprintln!("bad frame: {e:#}"),
                }
                self.frames += 1;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            StreamEvent::Ended(reason) => {
                self.ended = Some(reason);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        repeat: false,
                        ..
                    },
                ..
            } => match logical_key {
                Key::Named(NamedKey::Escape) => event_loop.exit(),
                Key::Named(NamedKey::F11) => self.toggle_fullscreen(),
                Key::Character(c) if c.eq_ignore_ascii_case("f") => self.toggle_fullscreen(),
                _ => {}
            },
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.draw() {
                    eprintln!("draw failed: {e:#}");
                }
                self.update_title();
            }
            _ => {}
        }
    }
}

impl Viewer {
    fn toggle_fullscreen(&self) {
        if let Some(window) = &self.window {
            let next = match window.fullscreen() {
                Some(_) => None,
                None => Some(Fullscreen::Borderless(None)),
            };
            window.set_fullscreen(next);
        }
    }

    fn update_title(&mut self) {
        if self.since.elapsed() < Duration::from_secs(1) {
            return;
        }
        if let Some(window) = &self.window {
            let (w, h) = self.frame_size;
            window.set_title(&format!(
                "{} - {w}x{h} - {} fps - demirror",
                self.info.name, self.frames
            ));
        }
        (self.frames, self.since) = (0, Instant::now());
    }

    /// Scales the frame into the window, keeping its aspect ratio (letterboxed).
    fn draw(&mut self) -> Result<()> {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return Ok(());
        };
        let size = window.inner_size();
        let (Some(win_w), Some(win_h)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return Ok(());
        };
        surface.resize(win_w, win_h).map_err(other)?;
        let mut buffer = surface.buffer_mut().map_err(other)?;
        let (ww, wh) = (size.width as usize, size.height as usize);
        let (fw, fh) = self.frame_size;
        buffer.fill(0);
        if fw > 0 && fh > 0 && self.frame.len() == fw * fh {
            let scale = (ww as f64 / fw as f64).min(wh as f64 / fh as f64);
            let (dw, dh) = (
                ((fw as f64 * scale) as usize).max(1),
                ((fh as f64 * scale) as usize).max(1),
            );
            let (ox, oy) = ((ww - dw.min(ww)) / 2, (wh - dh.min(wh)) / 2);
            let src_x: Vec<usize> = (0..dw).map(|x| x * fw / dw).collect();
            for y in 0..dh.min(wh) {
                let src_row = &self.frame[(y * fh / dh) * fw..][..fw];
                let dst_row = &mut buffer[(oy + y) * ww + ox..][..dw.min(ww)];
                for (dst, &sx) in dst_row.iter_mut().zip(&src_x) {
                    *dst = src_row[sx];
                }
            }
        }
        window.pre_present_notify();
        buffer.present().map_err(other)?;
        Ok(())
    }
}

/// softbuffer's error type is not `Send + Sync`, so anyhow can't wrap it directly.
fn other(e: softbuffer::SoftBufferError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

/// Decodes a JPEG into `0RGB` pixels and returns its size.
pub fn decode(jpeg: &[u8], out: &mut Vec<u32>) -> Result<(usize, usize)> {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::BGRA);
    let mut decoder = JpegDecoder::new_with_options(Cursor::new(jpeg), options);
    let bgra = decoder.decode().map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let (w, h) = decoder.dimensions().context("missing JPEG size")?;
    out.clear();
    out.extend(
        bgra.as_chunks::<4>()
            .0
            .iter()
            .map(|px| u32::from_le_bytes([px[0], px[1], px[2], 0])),
    );
    Ok((w, h))
}
