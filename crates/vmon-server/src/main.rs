//! demirror server (command line): creates virtual monitors on this host and streams them
//! to browsers and native clients. The `demirror` app runs the same server with a window.

use std::net::IpAddr;

use anyhow::Result;
use clap::Parser;
use vmon_server::{Config, OutputCommands, Server};

#[derive(Parser)]
#[command(version, about = "Create virtual monitors and view them from any device")]
struct Args {
    /// Address to listen on. 0.0.0.0 makes it reachable from other devices on the LAN.
    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,
    #[arg(long, default_value_t = vmon_proto::DEFAULT_PORT)]
    port: u16,
    /// Require this token from every client (`?token=...` or `Authorization: Bearer ...`).
    #[arg(long, env = "DEMIRROR_TOKEN")]
    token: Option<String>,
    /// Don't announce this host on the local network (mDNS).
    #[arg(long)]
    no_announce: bool,
    /// Shell command that adds a display to the OS, run when a screen monitor is
    /// created without a named output. Examples: `swaymsg create_output`,
    /// `hyprctl output create headless`, or a Windows virtual display driver's tool.
    #[arg(long)]
    create_output_cmd: Option<String>,
    /// Shell command that removes a display, `{output}` is its name. Examples:
    /// `swaymsg output {output} unplug`, `hyprctl output remove {output}`.
    #[arg(long)]
    destroy_output_cmd: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vmon_server=info".into()),
        )
        .init();
    let args = Args::parse();

    let server = Server::start(Config {
        bind: args.bind,
        port: args.port,
        token: args.token,
        commands: OutputCommands { create: args.create_output_cmd, destroy: args.destroy_output_cmd },
        advertise: !args.no_announce,
    })?;
    println!("demirror is running. Open on any device:");
    for url in server.urls() {
        println!("  {url}");
    }

    let _ = tokio::signal::ctrl_c().await;
    // Remove any displays the server added to the OS.
    tokio::task::spawn_blocking(move || server.stop()).await?;
    Ok(())
}
