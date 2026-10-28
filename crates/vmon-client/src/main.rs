//! Native demirror client: create, list, view and close virtual monitors.

mod viewer;

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tungstenite::Message;
use vmon_proto::{MonitorInfo, MonitorSpec, SourceSpec, SourcesInfo};

#[derive(Parser)]
#[command(version, about = "View demirror virtual monitors")]
struct Args {
    /// Address of the demirror server.
    #[arg(
        long,
        short,
        env = "DEMIRROR_SERVER",
        default_value = "http://127.0.0.1:7878"
    )]
    server: String,
    #[arg(long, env = "DEMIRROR_TOKEN")]
    token: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List monitors and capturable screens.
    List,
    /// Create a monitor, show it in a window, and close it when the window closes.
    New {
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
        #[arg(long, default_value_t = 30)]
        fps: u32,
        #[arg(long, default_value_t = 75)]
        quality: u8,
        /// Stream a host screen instead of the test pattern; give a name from `list`,
        /// or `new` for a display created by the server's --create-output-cmd.
        #[arg(long, conflicts_with = "virtual_display")]
        screen: Option<String>,
        /// Plug a new virtual display into the host, so its desktop extends
        /// onto this window like onto a real secondary monitor.
        #[arg(long = "virtual")]
        virtual_display: bool,
        /// Leave the monitor running after the window closes.
        #[arg(long)]
        keep: bool,
    },
    /// Show an existing monitor in a window.
    View { id: u32 },
    /// Save the current frame of a monitor as a JPEG file.
    Snapshot { id: u32, path: String },
    /// Measure frame rate and bandwidth of a monitor's stream.
    Bench {
        id: u32,
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
    /// Close a monitor.
    Close { id: u32 },
}

struct Client {
    base: String,
    token: Option<String>,
}

impl Client {
    fn url(&self, path: &str) -> String {
        match &self.token {
            Some(t) => format!("{}{path}?token={t}", self.base),
            None => format!("{}{path}", self.base),
        }
    }

    fn ws_url(&self, id: u32) -> String {
        let url = self.url(&format!("/ws/monitors/{id}"));
        url.replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1)
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(ureq::get(&self.url(path)).call()?.body_mut().read_json()?)
    }

    fn create(&self, spec: &MonitorSpec) -> Result<MonitorInfo> {
        let mut res = ureq::post(&self.url("/api/monitors"))
            .config()
            .http_status_as_error(false)
            .build()
            .send_json(spec)?;
        let body: serde_json::Value = res.body_mut().read_json()?;
        if !res.status().is_success() {
            bail!(
                "{}",
                body["error"]
                    .as_str()
                    .unwrap_or("could not create the monitor")
            );
        }
        Ok(serde_json::from_value(body)?)
    }

    fn close(&self, id: u32) -> Result<()> {
        ureq::delete(&self.url(&format!("/api/monitors/{id}"))).call()?;
        Ok(())
    }

    /// Connects to a monitor's stream and returns its info and the socket.
    fn connect(&self, id: u32) -> Result<(MonitorInfo, Socket)> {
        let (mut socket, _) = tungstenite::connect(self.ws_url(id))
            .with_context(|| format!("connecting to monitor {id}"))?;
        let info = match socket.read()? {
            Message::Text(text) => serde_json::from_str(&text)?,
            other => bail!("unexpected first message {other:?}"),
        };
        Ok((info, socket))
    }
}

type Socket = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

fn main() -> Result<()> {
    let args = Args::parse();
    let client = Client {
        base: args.server.trim_end_matches('/').to_string(),
        token: args.token,
    };
    match args.command {
        Command::List => {
            let monitors: Vec<MonitorInfo> = client.get("/api/monitors")?;
            if monitors.is_empty() {
                println!("No monitors.");
            }
            for m in monitors {
                println!(
                    "#{:<3} {:<20} {}x{} @ {} fps  {} viewer(s)",
                    m.id, m.name, m.width, m.height, m.fps, m.viewers
                );
            }
            let sources: SourcesInfo = client.get("/api/sources")?;
            println!(
                "\nScreens ({}):",
                sources.screen_backend.as_deref().unwrap_or("none")
            );
            for o in sources.outputs {
                println!(
                    "  {:<16} {}x{}  {}",
                    o.name, o.width, o.height, o.description
                );
            }
            if sources.can_create_outputs {
                println!("  new              (the server adds a display to the OS)");
            }
            let state = if sources.virtual_available {
                "available"
            } else {
                "unavailable"
            };
            println!("\nVirtual displays: {state}, {}", sources.virtual_detail);
        }
        Command::New {
            name,
            width,
            height,
            fps,
            quality,
            screen,
            virtual_display,
            keep,
        } => {
            let source = match screen.as_deref() {
                None if virtual_display => SourceSpec::Virtual,
                None => SourceSpec::Synthetic,
                Some("new") => SourceSpec::Screen { output: None },
                Some(output) => SourceSpec::Screen {
                    output: Some(output.into()),
                },
            };
            let info = client.create(&MonitorSpec {
                name,
                width,
                height,
                fps,
                quality,
                source,
                ..Default::default()
            })?;
            println!(
                "Created monitor #{} ({}x{})",
                info.id, info.width, info.height
            );
            let result = view(&client, info.id);
            if !keep {
                client.close(info.id)?;
                println!("Closed monitor #{}", info.id);
            }
            result?;
        }
        Command::View { id } => view(&client, id)?,
        Command::Snapshot { id, path } => {
            let (_, mut socket) = client.connect(id)?;
            loop {
                if let Message::Binary(jpeg) = socket.read()? {
                    std::fs::write(&path, &jpeg)?;
                    println!("Saved {} bytes to {path}", jpeg.len());
                    break;
                }
            }
        }
        Command::Bench { id, seconds } => {
            let (info, mut socket) = client.connect(id)?;
            let (mut frames, mut bytes, start) = (0u64, 0usize, Instant::now());
            while start.elapsed() < Duration::from_secs(seconds) {
                match socket.read() {
                    Ok(Message::Binary(jpeg)) => {
                        frames += 1;
                        bytes += jpeg.len();
                    }
                    Ok(_) => {}
                    Err(tungstenite::Error::ConnectionClosed) => {
                        println!("The monitor was closed.");
                        break;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            let secs = start.elapsed().as_secs_f64();
            println!(
                "#{} {}x{}: {:.1} fps (target {}), {:.2} MB/s, {:.0} KB/frame",
                info.id,
                info.width,
                info.height,
                frames as f64 / secs,
                info.fps,
                bytes as f64 / secs / 1e6,
                bytes as f64 / frames.max(1) as f64 / 1e3
            );
        }
        Command::Close { id } => {
            client.close(id)?;
            println!("Closed monitor #{id}");
        }
    }
    Ok(())
}

/// Shows a monitor in a window until it is closed (or Esc is pressed).
fn view(client: &Client, id: u32) -> Result<()> {
    let (info, socket) = client.connect(id)?;
    viewer::run(info, socket)
}
