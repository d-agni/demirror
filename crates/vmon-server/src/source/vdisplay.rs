//! Virtual displays on Linux, through the `vmon-vdisplayd` daemon (VKMS).
//!
//! The daemon plugs a display into the kernel, the desktop extends onto it
//! like onto any new monitor, and we read back what the desktop draws there.
//! The display lives as long as this source's connection.

use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::FileExt, net::UnixStream},
    time::Duration,
};

use std::{sync::mpsc, thread};

use anyhow::{Context, Result, bail};
use vmon_proto::{
    input::InputEvent,
    vdisplay::{Request, Response, SOCKET_PATH},
};

struct Connection {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Connection {
    fn open() -> Result<Self> {
        let stream = UnixStream::connect(SOCKET_PATH).with_context(|| {
            format!(
                "the virtual display service is not running ({SOCKET_PATH}); install vmon-vdisplayd"
            )
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        })
    }

    fn request(&mut self, request: &Request) -> Result<Response> {
        writeln!(self.writer, "{}", serde_json::to_string(request)?)?;
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            bail!("the virtual display service closed the connection");
        }
        match serde_json::from_str(&line)? {
            Response::Error { message } => bail!("{message}"),
            response => Ok(response),
        }
    }
}

/// Whether virtual displays can be created, and the driver or the reason why not.
pub fn support() -> (bool, String) {
    match Connection::open().and_then(|mut c| c.request(&Request::Hello)) {
        Ok(Response::Hello { driver, version }) => {
            (true, format!("vmon-vdisplayd {version} ({driver})"))
        }
        Ok(_) => (false, "unexpected answer from vmon-vdisplayd".into()),
        Err(e) => (false, format!("{e:#}")),
    }
}

pub struct VirtualSource {
    connection: Connection,
    display: String,
    frame: File,
    /// Shown (black) until the desktop starts using the display.
    size: (u32, u32),
    waiting: bool,
}

impl VirtualSource {
    pub fn open(width: u32, height: u32) -> Result<Self> {
        let mut connection = Connection::open()?;
        let Response::Created {
            display: name,
            card,
            frame_path,
        } = connection.request(&Request::Create { width, height })?
        else {
            bail!("unexpected answer from vmon-vdisplayd");
        };
        let frame = File::open(&frame_path).with_context(|| format!("opening {frame_path}"))?;
        tracing::info!("virtual display {name} plugged in ({card})");
        Ok(Self {
            connection,
            display: name,
            frame,
            size: (width, height),
            waiting: false,
        })
    }
}

impl super::Source for VirtualSource {
    fn capture(&mut self, out: &mut Vec<u8>) -> Result<(u32, u32)> {
        let Response::Captured(frame) = self.connection.request(&Request::Capture)? else {
            bail!("unexpected answer from vmon-vdisplayd");
        };
        if !frame.active {
            if !self.waiting {
                tracing::info!("waiting for the desktop to start using {}", self.display);
                self.waiting = true;
            }
            let (w, h) = self.size;
            out.clear();
            out.resize((w * h * 4) as usize, 0);
            return Ok((w, h));
        }
        if self.waiting {
            let (name, w, h) = (&self.display, frame.width, frame.height);
            tracing::info!("the desktop extended onto {name} at {w}x{h}");
            self.waiting = false;
        }
        self.size = (frame.width, frame.height);
        out.resize((frame.stride * frame.height) as usize, 0);
        self.frame.read_exact_at(out, 0)?;
        Ok((frame.width, frame.height))
    }

    /// Input goes to the daemon on a connection of its own, written as it comes (the daemon
    /// doesn't answer it), so it never waits behind a capture.
    fn input(&mut self) -> Option<mpsc::Sender<InputEvent>> {
        let mut connection = match Connection::open() {
            Ok(connection) => connection,
            Err(e) => {
                tracing::warn!("no input for {}: {e:#}", self.display);
                return None;
            }
        };
        let (events, received) = mpsc::channel::<InputEvent>();
        let name = self.display.clone();
        thread::Builder::new()
            .name(format!("input-{name}"))
            .spawn(move || {
                for event in received {
                    let request = Request::Input { display: name.clone(), event };
                    let sent = serde_json::to_string(&request)
                        .map_err(anyhow::Error::from)
                        .and_then(|line| Ok(writeln!(connection.writer, "{line}")?));
                    if let Err(e) = sent {
                        tracing::warn!("input for {name} stopped: {e:#}");
                        return;
                    }
                }
            })
            .ok()?;
        Some(events)
    }

    /// Updates the display's mode hint; the desktop picks the closest mode it offers.
    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        match self.connection.request(&Request::Resize { width, height })? {
            Response::Resized => {
                if self.waiting {
                    self.size = (width, height);
                }
                Ok(())
            }
            _ => bail!("unexpected answer from vmon-vdisplayd"),
        }
    }
}
