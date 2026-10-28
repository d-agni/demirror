//! The Unix socket service: one connection owns one virtual display. Input for a display
//! may come on another connection of the same user (`Request::Input`).

use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    collections::HashMap,
    sync::{
        Arc, Mutex,
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
use vmon_proto::vdisplay::{INPUT_HINT_DIR, Request, Response};

use crate::{kms::Card, uinput::Inputs, vkms};

/// The input devices of every display, by display name, with the user who owns it.
type Registry = Arc<Mutex<HashMap<String, (u32, Arc<Mutex<Inputs>>)>>>;

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
    let registry = Registry::default();
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (allowed, next, registry) = (allowed.clone(), next.clone(), registry.clone());
        thread::spawn(move || {
            if let Err(e) = handle(stream, &allowed, &next, &registry) {
                eprintln!("connection ended: {e:#}");
            }
        });
    }
    Ok(())
}

/// A display owned by one connection. Field order matters: the card is closed and the input
/// devices are gone before the display is unplugged.
struct Session {
    card: Card,
    frame: File,
    frame_path: PathBuf,
    scratch: Vec<u8>,
    registry: Registry,
    display: vkms::VkmsDisplay,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.frame_path);
        let _ = fs::remove_file(Path::new(INPUT_HINT_DIR).join(&self.display.name));
        // The devices go with the last reference (an input being written finishes first).
        self.registry.lock().unwrap().remove(&self.display.name);
        eprintln!("virtual display {} removed", self.display.name);
    }
}

fn handle(stream: UnixStream, allowed: &[u32], next: &AtomicU32, registry: &Registry) -> Result<()> {
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
    let mut input_failed = false;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        let response = match serde_json::from_str::<Request>(&line) {
            // Not answered: input must never wait for a reply.
            Ok(Request::Input { display, event }) => {
                let inputs = registry.lock().unwrap().get(&display).cloned();
                let result = match inputs {
                    Some((owner, inputs)) if owner == peer.uid.as_raw() || peer.uid.is_root() => {
                        if event.is_valid() { inputs.lock().unwrap().send(&event) } else { Ok(()) }
                    }
                    Some(_) => Err(anyhow::anyhow!("display {display} belongs to another user")),
                    None => Err(anyhow::anyhow!("no input devices for display {display}")),
                };
                if let Err(e) = result
                    && !std::mem::replace(&mut input_failed, true)
                {
                    eprintln!("input for {display}: {e:#}");
                }
                continue;
            }
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
                match create(&name, (width, height), peer.uid, peer.gid, registry) {
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

fn create(name: &str, size: (u32, u32), uid: Uid, gid: Gid, registry: &Registry) -> Result<Session> {
    let display = vkms::VkmsDisplay::create(name, Some(size))?;
    // Input is a bonus: a display without it still shows.
    match Inputs::create(name) {
        Ok(inputs) => {
            if let Err(e) = write_input_hint(name, inputs.pointer_name()) {
                eprintln!("input hint for {name}: {e:#}");
            }
            registry.lock().unwrap().insert(name.into(), (uid.as_raw(), Arc::new(Mutex::new(inputs))));
        }
        Err(e) => eprintln!("no input devices for {name}: {e:#}"),
    }
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
        registry: registry.clone(),
        display,
    })
}

/// Tells desktops which input device points at the display (`vdisplay::INPUT_HINT_DIR`).
fn write_input_hint(display: &str, pointer: &str) -> Result<()> {
    fs::create_dir_all(INPUT_HINT_DIR)?;
    fs::set_permissions(INPUT_HINT_DIR, fs::Permissions::from_mode(0o755))?;
    let path = Path::new(INPUT_HINT_DIR).join(display);
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, format!("{pointer}\n"))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
    fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
}
