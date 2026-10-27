//! The Unix socket service: one connection owns one virtual display.

use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    thread,
};

use anyhow::{Context, Result};
use rustix::{
    fs::fchown,
    net::sockopt::socket_peercred,
    process::{Gid, Uid},
};
use vmon_proto::vdisplay::{Request, Response};

use crate::{kms::Card, vkms};

const RUN_DIR: &str = "/run/demirror";

pub fn serve(socket: &Path, allowed_uids: Vec<u32>) -> Result<()> {
    vkms::ensure_loaded()?;
    vkms::remove_stale();

    fs::create_dir_all(RUN_DIR)?;
    fs::set_permissions(RUN_DIR, fs::Permissions::from_mode(0o755))?;
    let _ = fs::remove_file(socket);
    let listener =
        UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    // Anyone may connect; each connection's user is checked against the allow list.
    fs::set_permissions(socket, fs::Permissions::from_mode(0o666))?;
    eprintln!("vmon-vdisplayd listening on {} (VKMS)", socket.display());

    let allowed = Arc::new(allowed_uids);
    let next = Arc::new(AtomicU32::new(1));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (allowed, next) = (allowed.clone(), next.clone());
        thread::spawn(move || {
            if let Err(e) = handle(stream, &allowed, &next) {
                eprintln!("connection ended: {e:#}");
            }
        });
    }
    Ok(())
}

/// A display owned by one connection. Field order matters: the card is
/// closed before the display is unplugged.
struct Session {
    card: Card,
    frame: File,
    frame_path: PathBuf,
    scratch: Vec<u8>,
    display: vkms::VkmsDisplay,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.frame_path);
        eprintln!("virtual display {} removed", self.display.name);
    }
}

fn handle(stream: UnixStream, allowed: &[u32], next: &AtomicU32) -> Result<()> {
    let peer = socket_peercred(&stream)?;
    let mut writer = stream.try_clone()?;
    let mut reply = |response: &Response| -> Result<()> {
        writeln!(writer, "{}", serde_json::to_string(response)?)?;
        Ok(())
    };
    if !peer.uid.is_root() && !allowed.contains(&peer.uid.as_raw()) {
        let message = format!(
            "user {} may not create virtual displays (see --user)",
            peer.uid.as_raw()
        );
        return reply(&Response::Error { message });
    }

    let mut session: Option<Session> = None;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        let response = match serde_json::from_str::<Request>(&line) {
            Err(e) => Response::Error {
                message: format!("bad request: {e}"),
            },
            Ok(Request::Hello) => Response::Hello {
                version: env!("CARGO_PKG_VERSION").into(),
                driver: "vkms".into(),
            },
            Ok(Request::Create { .. }) if session.is_some() => Response::Error {
                message: "this connection already has a display".into(),
            },
            Ok(Request::Create { width, height }) => {
                let name = format!("{}{}", vkms::PREFIX, next.fetch_add(1, Ordering::Relaxed));
                match create(&name, (width, height), peer.uid, peer.gid) {
                    Ok(s) => {
                        eprintln!(
                            "virtual display {name} on {} for uid {} (asked for {width}x{height})",
                            s.display.card.display(),
                            peer.uid.as_raw()
                        );
                        let response = Response::Created {
                            display: name,
                            card: s.display.card.display().to_string(),
                            frame_path: s.frame_path.display().to_string(),
                        };
                        session = Some(s);
                        response
                    }
                    Err(e) => Response::Error {
                        message: format!("{e:#}"),
                    },
                }
            }
            Ok(Request::Resize { width, height }) => match &session {
                None => Response::Error {
                    message: "create a display first".into(),
                },
                Some(s) => match vkms::write_mode_hint(&s.display.name, width, height) {
                    Ok(()) => {
                        eprintln!("virtual display {} viewed at {width}x{height}", s.display.name);
                        Response::Resized
                    }
                    Err(e) => Response::Error {
                        message: format!("{e:#}"),
                    },
                },
            },
            Ok(Request::Capture) => match &mut session {
                None => Response::Error {
                    message: "create a display first".into(),
                },
                Some(s) => match s.card.capture(&s.frame, &mut s.scratch) {
                    Ok(frame) => Response::Captured(frame),
                    Err(e) => Response::Error {
                        message: format!("{e:#}"),
                    },
                },
            },
        };
        reply(&response)?;
    }
    Ok(())
}

fn create(name: &str, size: (u32, u32), uid: Uid, gid: Gid) -> Result<Session> {
    let display = vkms::VkmsDisplay::create(name, Some(size))?;
    let card = Card::open(&display.card)?;
    let frame_path = Path::new(RUN_DIR).join(format!("{name}.bgra"));
    let frame = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&frame_path)?;
    // Only the client that owns the display can read its pixels.
    fchown(&frame, Some(uid), Some(gid))?;
    Ok(Session {
        card,
        frame,
        frame_path,
        scratch: Vec::new(),
        display,
    })
}
