//! Virtual monitor lifecycle: create, capture + encode while viewed, close.

use std::{
    collections::BTreeMap,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use axum::body::Bytes;
use jpeg_encoder::{ColorType, Encoder};
use parking_lot::Mutex;
use tokio::sync::watch;
use vmon_proto::{MonitorInfo, MonitorSpec, SourceSpec};

use crate::source::{self, Source, Synthetic};

/// Latest encoded JPEG of a monitor; `None` until the first frame.
pub type FrameRx = watch::Receiver<Option<Bytes>>;

/// Shell commands that add and remove an OS display for each screen monitor.
#[derive(Clone, Default)]
pub struct OutputCommands {
    pub create: Option<String>,
    /// `{output}` is replaced with the name of the display to remove.
    pub destroy: Option<String>,
}

struct Monitor {
    info: MonitorInfo,
    frames: Arc<watch::Sender<Option<Bytes>>>,
    stop: Arc<AtomicBool>,
    /// Size of the last frame streamed (`pack_size`): a virtual display follows the viewer.
    size: Arc<AtomicU64>,
    /// New viewer sizes for the source (`Source::resize`).
    resize: mpsc::Sender<(u32, u32)>,
    /// Display created by `OutputCommands::create`, removed on close.
    created_output: Option<String>,
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub struct MonitorManager {
    monitors: Mutex<BTreeMap<u32, Monitor>>,
    next_id: AtomicU32,
    commands: OutputCommands,
}

impl MonitorManager {
    pub fn new(commands: OutputCommands) -> Self {
        Self {
            monitors: Mutex::default(),
            next_id: AtomicU32::new(1),
            commands,
        }
    }

    pub fn can_create_outputs(&self) -> bool {
        self.commands.create.is_some()
    }

    pub fn list(&self) -> Vec<MonitorInfo> {
        self.monitors
            .lock()
            .values()
            .map(Monitor::current_info)
            .collect()
    }

    pub fn subscribe(&self, id: u32) -> Option<(MonitorInfo, FrameRx)> {
        let monitors = self.monitors.lock();
        let monitor = monitors.get(&id)?;
        let rx = monitor.frames.subscribe();
        Some((monitor.current_info(), rx))
    }

    /// Creates a monitor and captures its first frame. Blocking.
    pub fn create(&self, mut spec: MonitorSpec) -> Result<MonitorInfo> {
        validate(&spec)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let mut created_output = None;
        if let (
            SourceSpec::Screen {
                output: output @ None,
            },
            Some(cmd),
        ) = (&mut spec.source, &self.commands.create)
        {
            let name = create_output(cmd)?;
            *output = Some(name.clone());
            created_output = Some(name);
        }

        let (frames, _) = watch::channel(None);
        let frames = Arc::new(frames);
        let stop = Arc::new(AtomicBool::new(false));
        let size = Arc::new(AtomicU64::new(0));
        let (resize, resize_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        {
            let (spec, frames, stop) = (spec.clone(), frames.clone(), stop.clone());
            let shared = Shared { frames, stop, size: size.clone(), resize: resize_rx };
            thread::Builder::new()
                .name(format!("monitor-{id}"))
                .spawn(move || capture_loop(id, spec, shared, ready_tx))?;
        }
        let first = ready_rx
            .recv()
            .map_err(|_| anyhow!("capture thread died"))
            .and_then(|r| r);
        let (width, height) = match first {
            Ok(size) => size,
            Err(e) => {
                if let Some(name) = &created_output {
                    self.destroy_output(name);
                }
                return Err(e);
            }
        };

        let info = MonitorInfo {
            id,
            name: spec
                .name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| format!("Monitor {id}")),
            width,
            height,
            fps: spec.fps,
            quality: spec.quality,
            source: spec.source,
            viewers: 0,
        };
        tracing::info!(id, name = info.name, width, height, "monitor created");
        self.monitors.lock().insert(
            id,
            Monitor {
                info: info.clone(),
                frames,
                stop,
                size,
                resize,
                created_output,
            },
        );
        Ok(info)
    }

    /// The viewing device's screen changed size. A virtual display asks the desktop for the
    /// closest mode; other sources keep their size (the viewer scales).
    pub fn resize(&self, id: u32, width: u32, height: u32) -> Result<bool> {
        validate_size(width, height)?;
        let monitors = self.monitors.lock();
        let Some(monitor) = monitors.get(&id) else {
            return Ok(false);
        };
        let _ = monitor.resize.send((width, height));
        tracing::info!(id, width, height, "viewer size");
        Ok(true)
    }

    /// Closes a monitor; its viewers' streams end. Blocking.
    pub fn close(&self, id: u32) -> bool {
        let Some(monitor) = self.monitors.lock().remove(&id) else {
            return false;
        };
        if let Some(name) = &monitor.created_output {
            self.destroy_output(name);
        }
        tracing::info!(id, "monitor closed");
        true
    }

    pub fn close_all(&self) {
        let ids: Vec<u32> = self.monitors.lock().keys().copied().collect();
        for id in ids {
            self.close(id);
        }
    }

    fn destroy_output(&self, name: &str) {
        if let Some(cmd) = &self.commands.destroy
            && let Err(e) = run_shell(&cmd.replace("{output}", name))
        {
            tracing::warn!("removing display {name} failed: {e:#}");
        }
    }
}

impl Monitor {
    fn current_info(&self) -> MonitorInfo {
        let (width, height) = match unpack_size(self.size.load(Ordering::Relaxed)) {
            (0, 0) => (self.info.width, self.info.height),
            size => size,
        };
        MonitorInfo {
            viewers: self.frames.receiver_count(),
            width,
            height,
            ..self.info.clone()
        }
    }
}

fn pack_size(width: u32, height: u32) -> u64 {
    (u64::from(width) << 32) | u64::from(height)
}

fn unpack_size(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

fn validate_size(width: u32, height: u32) -> Result<()> {
    if !(16..=8192).contains(&width) || !(16..=8192).contains(&height) {
        bail!("size must be between 16x16 and 8192x8192");
    }
    Ok(())
}

fn validate(spec: &MonitorSpec) -> Result<()> {
    validate_size(spec.width, spec.height)?;
    if !(1..=120).contains(&spec.fps) {
        bail!("fps must be between 1 and 120");
    }
    if !(1..=100).contains(&spec.quality) {
        bail!("quality must be between 1 and 100");
    }
    Ok(())
}

/// What the capture thread shares with its `Monitor`.
struct Shared {
    frames: Arc<watch::Sender<Option<Bytes>>>,
    stop: Arc<AtomicBool>,
    size: Arc<AtomicU64>,
    resize: mpsc::Receiver<(u32, u32)>,
}

fn capture_loop(id: u32, spec: MonitorSpec, shared: Shared, ready: mpsc::Sender<Result<(u32, u32)>>) {
    let Shared { frames, stop, size, resize } = shared;
    let mut pixels = Vec::new();
    let opened = (|| -> Result<_> {
        let mut source: Box<dyn Source> = match &spec.source {
            SourceSpec::Synthetic => Box::new(Synthetic::new(id, spec.width, spec.height)),
            SourceSpec::Screen { output } => source::open_screen(output.as_deref())?,
            SourceSpec::Virtual => source::open_virtual(spec.width, spec.height)?,
        };
        let (w, h) = source.capture(&mut pixels)?;
        frames.send_replace(Some(encode(&pixels, w, h, spec.quality)?));
        size.store(pack_size(w, h), Ordering::Relaxed);
        Ok((source, (w, h)))
    })();
    let mut source = match opened {
        Ok((source, size)) => {
            let _ = ready.send(Ok(size));
            source
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    // Capture and encode run on separate threads so they overlap: the
    // encoder works on frame N while the compositor delivers frame N + 1.
    let (raw_tx, raw_rx) = mpsc::sync_channel::<(Vec<u8>, u32, u32)>(1);
    let (free_tx, free_rx) = mpsc::channel::<Vec<u8>>();
    {
        let (frames, size) = (frames.clone(), size.clone());
        let quality = spec.quality;
        let spawned = thread::Builder::new()
            .name(format!("encode-{id}"))
            .spawn(move || {
                for (pixels, w, h) in raw_rx {
                    match encode(&pixels, w, h, quality) {
                        Ok(jpeg) => {
                            frames.send_replace(Some(jpeg));
                            size.store(pack_size(w, h), Ordering::Relaxed);
                        }
                        Err(e) => tracing::warn!(id, "encoding failed: {e:#}"),
                    }
                    let _ = free_tx.send(pixels);
                }
            });
        if let Err(e) = spawned {
            tracing::error!(id, "no encoder thread: {e}");
            return;
        }
    }

    let interval = Duration::from_secs_f64(1.0 / spec.fps as f64);
    let mut next = Instant::now();
    let mut failing = false;
    while !stop.load(Ordering::Relaxed) {
        // Only the latest size counts (a phone turning sends several).
        if let Some((w, h)) = resize.try_iter().last()
            && let Err(e) = source.resize(w, h)
        {
            tracing::warn!(id, "resizing to {w}x{h} failed: {e:#}");
        }
        // Nobody is watching: don't spend CPU capturing and encoding.
        if frames.receiver_count() == 0 {
            thread::sleep(Duration::from_millis(100));
            next = Instant::now();
            continue;
        }
        let mut pixels = free_rx.try_recv().unwrap_or_default();
        match source.capture(&mut pixels) {
            Ok((w, h)) => {
                // Blocks while the encoder is still busy with the previous frame.
                if raw_tx.send((pixels, w, h)).is_err() {
                    break;
                }
                failing = false;
            }
            Err(e) => {
                if !failing {
                    tracing::warn!(id, "capture failed, retrying: {e:#}");
                    failing = true;
                }
                thread::sleep(Duration::from_millis(500));
            }
        }
        next += interval;
        let now = Instant::now();
        match next.checked_duration_since(now) {
            Some(wait) => thread::sleep(wait),
            None => next = now,
        }
    }
}

fn encode(bgra: &[u8], width: u32, height: u32, quality: u8) -> Result<Bytes> {
    let (w, h) = (u16::try_from(width)?, u16::try_from(height)?);
    let mut out = Vec::with_capacity(bgra.len() / 8);
    Encoder::new(&mut out, quality).encode(bgra, w, h, ColorType::Bgra)?;
    Ok(out.into())
}

/// Runs the create command and returns the name of the display it added.
fn create_output(cmd: &str) -> Result<String> {
    let names = || -> Result<Vec<String>> {
        Ok(source::list_outputs()?
            .into_iter()
            .map(|o| o.name)
            .collect())
    };
    let before = names()?;
    run_shell(cmd)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(new) = names()?.into_iter().find(|n| !before.contains(n)) {
            return Ok(new);
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!("`{cmd}` ran but no new display appeared")
}

fn run_shell(cmd: &str) -> Result<()> {
    let status = if cfg!(windows) {
        Command::new("cmd").args(["/C", cmd]).status()
    } else {
        Command::new("sh").args(["-c", cmd]).status()
    }
    .with_context(|| format!("running `{cmd}`"))?;
    if !status.success() {
        bail!("`{cmd}` exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_monitor_lifecycle() {
        let manager = MonitorManager::new(OutputCommands::default());
        let spec = MonitorSpec {
            width: 320,
            height: 200,
            ..Default::default()
        };
        let info = manager.create(spec).unwrap();
        assert_eq!((info.width, info.height), (320, 200));

        let (_, rx) = manager.subscribe(info.id).unwrap();
        let jpeg = rx.borrow().clone().expect("first frame is ready on create");
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG start marker");
        assert_eq!(manager.list()[0].viewers, 1);

        assert!(manager.close(info.id));
        assert!(manager.list().is_empty());
        assert!(!manager.close(info.id));
    }

    #[test]
    fn rejects_bad_specs() {
        let manager = MonitorManager::new(OutputCommands::default());
        assert!(
            manager
                .create(MonitorSpec {
                    width: 1,
                    ..Default::default()
                })
                .is_err()
        );
        assert!(
            manager
                .create(MonitorSpec {
                    fps: 0,
                    ..Default::default()
                })
                .is_err()
        );
    }
}
