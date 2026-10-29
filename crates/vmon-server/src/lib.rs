//! demirror's host side as a library: creates virtual monitors on this machine and streams
//! them to browsers and native viewers. The `vmon-server` binary and the `demirror` app both
//! run it.

mod discovery;
pub mod monitor;
pub mod source;
mod web;

use std::{
    net::{IpAddr, SocketAddr, TcpListener, UdpSocket},
    sync::Arc,
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result};
use tokio::sync::oneshot;

pub use monitor::{FrameRx, MonitorManager, OutputCommands};

/// How to run the server.
#[derive(Clone)]
pub struct Config {
    pub bind: IpAddr,
    pub port: u16,
    /// Required from every client when set.
    pub token: Option<String>,
    pub commands: OutputCommands,
    /// Announce this host on the local network (mDNS) so viewers find it.
    pub advertise: bool,
    /// Viewers may control this computer (pointer, keys) through virtual monitors.
    pub allow_input: bool,
}

/// A running server; stopped (monitors closed, displays unplugged) when dropped.
pub struct Server {
    pub manager: Arc<MonitorManager>,
    pub port: u16,
    pub token: Option<String>,
    bind: IpAddr,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    _mdns: Option<mdns_sd::ServiceDaemon>,
}

impl Server {
    /// Starts serving on a thread of its own (with its own async runtime).
    pub fn start(config: Config) -> Result<Server> {
        let addr = SocketAddr::new(config.bind, config.port);
        let listener = TcpListener::bind(addr).with_context(|| format!("listening on {addr}"))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();

        let manager = Arc::new(MonitorManager::new(config.commands));
        manager.set_allow_input(config.allow_input);
        let app = web::App { manager: manager.clone(), token: config.token.clone() };
        let (shutdown, stopped) = oneshot::channel::<()>();
        let thread = thread::Builder::new().name("demirror-server".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(e) => return tracing::error!("no async runtime: {e}"),
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(e) => return tracing::error!("listener: {e}"),
                };
                let service = web::router(app).into_make_service_with_connect_info::<SocketAddr>();
                let served = axum::serve(listener, service).with_graceful_shutdown(async {
                    let _ = stopped.await;
                });
                if let Err(e) = served.await {
                    tracing::error!("server stopped: {e}");
                }
            });
        })?;

        let mdns = if config.advertise {
            discovery::advertise(port, config.token.is_some())
                .inspect_err(|e| tracing::warn!("not announced on the network (mDNS): {e:#}"))
                .ok()
        } else {
            None
        };
        tracing::info!(port, "demirror server started");
        Ok(Server {
            manager,
            port,
            token: config.token,
            bind: config.bind,
            shutdown: Some(shutdown),
            thread: Some(thread),
            _mdns: mdns,
        })
    }

    /// Addresses to open on other devices: this machine, then its LAN address.
    pub fn urls(&self) -> Vec<String> {
        let query = self.token.as_ref().map(|t| format!("/?token={t}")).unwrap_or_default();
        let mut urls = vec![format!("http://127.0.0.1:{}{query}", self.port)];
        if self.bind.is_unspecified()
            && let Some(ip) = lan_ip()
        {
            urls.push(format!("http://{ip}:{}{query}", self.port));
        }
        urls
    }

    /// The address other devices should use (the LAN one when there is one).
    pub fn share_url(&self) -> String {
        self.urls().pop().unwrap_or_default()
    }

    /// Closes every monitor (unplugging their displays) and stops serving. Blocking.
    pub fn stop(mut self) {
        self.shut_down();
    }

    fn shut_down(&mut self) {
        // Closing the monitors first ends their streams, so serving can stop at once.
        self.manager.close_all();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// The address other devices on the LAN reach this host at. Connecting a UDP socket sends
/// nothing; it only makes the OS pick the outgoing interface.
pub fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    Some(socket.local_addr().ok()?.ip())
}

/// This machine's name, for announcing it and naming things.
pub fn host_name() -> String {
    let name = std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_default();
    let name = name.trim();
    if name.is_empty() { "demirror".into() } else { name.into() }
}
