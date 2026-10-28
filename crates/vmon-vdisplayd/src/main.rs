//! vmon-vdisplayd: plugs virtual displays into Linux for demirror.
//!
//! Runs as root (creating display devices and reading their framebuffers
//! needs it) and serves the unprivileged demirror server over a Unix socket.

#[cfg(target_os = "linux")]
mod daemon;
#[cfg(target_os = "linux")]
mod kms;
#[cfg(target_os = "linux")]
mod uinput;
#[cfg(target_os = "linux")]
mod vkms;

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("vmon-vdisplayd is only needed on Linux.");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        fs,
        path::PathBuf,
        thread,
        time::{Duration, Instant},
    };

    use anyhow::{Context, Result, bail};
    use clap::{Parser, Subcommand};
    use vmon_proto::vdisplay::SOCKET_PATH;

    use crate::{daemon, kms, uinput, vkms};

    #[derive(Parser)]
    #[command(version, about = "Virtual display daemon for demirror (Linux, VKMS)")]
    struct Args {
        #[command(subcommand)]
        command: Option<Command>,
    }

    #[derive(Subcommand)]
    enum Command {
        /// Serve virtual displays (the default).
        Run {
            /// User allowed to create displays, by name or uid. Repeatable; root always may.
            #[arg(long = "user")]
            users: Vec<String>,
            #[arg(long, default_value = SOCKET_PATH)]
            socket: PathBuf,
        },
        /// Create a display, check that its pixels can be read, and remove it.
        Selftest {
            /// Also save the captured image as a PPM file.
            #[arg(long)]
            save: Option<PathBuf>,
        },
    }

    pub fn main() -> Result<()> {
        if !rustix::process::geteuid().is_root() {
            bail!("vmon-vdisplayd must run as root (it creates display devices)");
        }
        match Args::parse().command.unwrap_or(Command::Run {
            users: Vec::new(),
            socket: SOCKET_PATH.into(),
        }) {
            Command::Run { users, socket } => {
                let uids = users
                    .iter()
                    .map(|u| uid_of(u))
                    .collect::<Result<Vec<_>>>()?;
                daemon::serve(&socket, uids)
            }
            Command::Selftest { save } => selftest(save),
        }
    }

    fn uid_of(user: &str) -> Result<u32> {
        if let Ok(uid) = user.parse() {
            return Ok(uid);
        }
        let passwd = fs::read_to_string("/etc/passwd")?;
        passwd
            .lines()
            .map(|l| l.split(':').collect::<Vec<_>>())
            .find(|f| f.first() == Some(&user))
            .and_then(|f| f.get(2)?.parse().ok())
            .with_context(|| format!("unknown user {user}"))
    }

    fn selftest(save: Option<PathBuf>) -> Result<()> {
        vkms::ensure_loaded()?;
        vkms::remove_stale();
        let display = vkms::VkmsDisplay::create(&format!("{}selftest", vkms::PREFIX), None)?;
        println!("1. virtual display created: {}", display.card.display());

        let card = kms::Card::open(&display.card)?;
        // If no desktop picks the display up, drive it ourselves with a known image.
        thread::sleep(Duration::from_secs(1));
        let test_image = card.show_test_image()?;
        match &test_image {
            Some((_, _, (w, h))) => {
                println!("2. no desktop took the display; showing a test image at {w}x{h}")
            }
            None => println!("2. the desktop took the display (it is extending onto it)"),
        }

        let path = std::env::temp_dir().join("demirror-selftest.bgra");
        let out = fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        let mut scratch = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        let frame = loop {
            let frame = card.capture(&out, &mut scratch)?;
            if frame.active || Instant::now() > deadline {
                break frame;
            }
            thread::sleep(Duration::from_millis(200));
        };
        if !frame.active {
            bail!("the display never lit up");
        }
        println!(
            "3. captured {}x{} from the kernel",
            frame.width, frame.height
        );

        if test_image.is_some() {
            let px = |x: usize, y: usize| {
                let i = y * frame.stride as usize + x * 4;
                u32::from_le_bytes([scratch[i], scratch[i + 1], scratch[i + 2], 0])
            };
            for (x, y) in [
                (0, 0),
                (37, 91),
                (frame.width as usize - 1, frame.height as usize - 1),
            ] {
                if px(x, y) != kms::test_pixel(x, y) {
                    bail!(
                        "pixel ({x},{y}) is {:06x}, expected {:06x}",
                        px(x, y),
                        kms::test_pixel(x, y)
                    );
                }
            }
            println!("4. pixels match the test image");
        }
        if let Some(save) = save {
            let mut ppm = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
            ppm.extend(
                scratch
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[2], p[1], p[0]]),
            );
            fs::write(&save, ppm)?;
            println!("   saved {}", save.display());
        }
        let _ = fs::remove_file(path);

        if let Some((buffer, fb, _)) = test_image {
            use drm::control::Device;
            let _ = card.destroy_framebuffer(fb);
            let _ = card.destroy_dumb_buffer(buffer);
        }
        match uinput::Inputs::create(&display.name) {
            Ok(inputs) => println!("5. input devices created ({} and its keyboard)", inputs.pointer_name()),
            Err(e) => bail!("input devices: {e:#}"),
        }
        drop(card);
        drop(display);
        println!("6. virtual display removed\nPASS");
        Ok(())
    }
}
