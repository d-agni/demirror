//! Talking to a demirror host: its HTTP API, a monitor's stream, and finding hosts on the
//! local network.

use std::{
    io::Cursor,
    net::TcpStream,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use eframe::egui::ColorImage;
use tungstenite::{Message, stream::MaybeTlsStream};
use vmon_proto::{MDNS_SERVICE, MonitorInfo, MonitorSpec, Size, SourcesInfo};
use zune_jpeg::{
    JpegDecoder,
    zune_core::{colorspace::ColorSpace, options::DecoderOptions},
};

pub type Socket = tungstenite::WebSocket<MaybeTlsStream<TcpStream>>;

/// A host's address (`http://192.168.1.5:7878`) and its token, if it wants one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub base: String,
    pub token: Option<String>,
}

impl Target {
    /// From what someone typed: `192.168.1.5`, `host:7878`, a full URL, even one with `?token=`.
    pub fn parse(address: &str, token: &str) -> Result<Target> {
        let mut address = address.trim().trim_end_matches('/').to_string();
        if address.is_empty() {
            bail!("enter the host's address");
        }
        let mut token = Some(token.trim().to_string()).filter(|t| !t.is_empty());
        if let Some((base, query)) = address.split_once('?') {
            if let Some(t) = query.split('&').find_map(|p| p.strip_prefix("token=")) {
                token.get_or_insert_with(|| t.to_string());
            }
            address = base.trim_end_matches('/').to_string();
        }
        if !address.contains("://") {
            address = format!("http://{address}");
        }
        let host_part = address.split_once("://").map_or("", |(_, h)| h);
        if !host_part.contains(':') {
            address = format!("{address}:{}", vmon_proto::DEFAULT_PORT);
        }
        Ok(Target { base: address, token })
    }

    fn url(&self, path: &str) -> String {
        match &self.token {
            Some(t) => format!("{}{path}?token={t}", self.base),
            None => format!("{}{path}", self.base),
        }
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(8)))
            .http_status_as_error(false)
            .build()
            .into()
    }

    fn check(mut response: ureq::http::Response<ureq::Body>) -> Result<ureq::http::Response<ureq::Body>> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body: serde_json::Value = response.body_mut().read_json().unwrap_or_default();
        match body["error"].as_str() {
            Some(e) => bail!("{e}"),
            None if status == 401 => bail!("the host wants a token (or this one is wrong)"),
            None => bail!("the host answered {status}"),
        }
    }

    pub fn sources(&self) -> Result<SourcesInfo> {
        let response = Self::agent().get(self.url("/api/sources")).call().context("can't reach the host")?;
        Ok(Self::check(response)?.body_mut().read_json()?)
    }

    pub fn create(&self, spec: &MonitorSpec) -> Result<MonitorInfo> {
        let response = Self::agent().post(self.url("/api/monitors")).send_json(spec).context("can't reach the host")?;
        Ok(Self::check(response)?.body_mut().read_json()?)
    }

    pub fn close(&self, id: u32) -> Result<()> {
        Self::check(Self::agent().delete(self.url(&format!("/api/monitors/{id}"))).call()?)?;
        Ok(())
    }

    pub fn resize(&self, id: u32, width: u32, height: u32) -> Result<()> {
        let url = self.url(&format!("/api/monitors/{id}/size"));
        Self::check(Self::agent().post(url).send_json(Size { width, height })?)?;
        Ok(())
    }

    /// A monitor's stream: its info first, then JPEG frames.
    pub fn connect(&self, id: u32) -> Result<(MonitorInfo, Socket)> {
        let url = self.url(&format!("/ws/monitors/{id}"));
        let url = url.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
        let (mut socket, _) = tungstenite::connect(url).context("can't open the monitor's stream")?;
        let info = match socket.read()? {
            Message::Text(text) => serde_json::from_str(&text)?,
            other => bail!("unexpected first message {other:?}"),
        };
        Ok((info, socket))
    }
}

/// Decodes a JPEG frame for a texture.
pub fn decode(jpeg: &[u8]) -> Result<ColorImage> {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(Cursor::new(jpeg), options);
    let rgba = decoder.decode().map_err(|e| anyhow!("{e:?}"))?;
    let (w, h) = decoder.dimensions().context("missing JPEG size")?;
    Ok(ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}

/// A random token for a host that wants one.
pub fn new_token() -> String {
    use std::hash::{BuildHasher, RandomState};
    let a = RandomState::new().hash_one(std::time::SystemTime::now());
    let b = RandomState::new().hash_one(std::process::id());
    format!("{:08x}{:08x}", a as u32, b as u32)
}

/// A host found on the local network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundHost {
    pub name: String,
    pub fullname: String,
    pub address: String,
    pub token_required: bool,
}

/// Hosts announcing themselves on the local network, kept up to date by a thread.
pub fn discover(on_change: impl Fn() + Send + 'static) -> Arc<Mutex<Vec<FoundHost>>> {
    let hosts: Arc<Mutex<Vec<FoundHost>>> = Arc::default();
    let list = hosts.clone();
    thread::spawn(move || {
        let Ok(mdns) = mdns_sd::ServiceDaemon::new() else { return };
        let Ok(events) = mdns.browse(MDNS_SERVICE) else { return };
        while let Ok(event) = events.recv() {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                    // Every interface is announced; another device needs a LAN address.
                    let mut ips: Vec<_> = info.get_addresses_v4().into_iter().collect();
                    ips.sort_by_key(|ip| (ip.is_loopback(), ip.is_link_local()));
                    let Some(ip) = ips.first() else { continue };
                    let name = info.fullname.trim_end_matches(MDNS_SERVICE).trim_end_matches('.').to_string();
                    let host = FoundHost {
                        name,
                        fullname: info.fullname.clone(),
                        address: format!("{ip}:{}", info.port),
                        token_required: info.get_property_val_str("token") == Some("required"),
                    };
                    let mut list = list.lock().unwrap();
                    list.retain(|h| h.fullname != host.fullname);
                    list.push(host);
                    list.sort_by(|a, b| a.name.cmp(&b.name));
                }
                mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                    list.lock().unwrap().retain(|h| h.fullname != fullname);
                }
                _ => continue,
            }
            on_change();
        }
    });
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_addresses() {
        let t = Target::parse("192.168.1.5", "").unwrap();
        assert_eq!((t.base.as_str(), t.token), ("http://192.168.1.5:7878", None));
        let t = Target::parse(" host:9000/ ", " abc ").unwrap();
        assert_eq!((t.base.as_str(), t.token.as_deref()), ("http://host:9000", Some("abc")));
        let t = Target::parse("http://10.0.0.2:7878/?token=xyz", "").unwrap();
        assert_eq!((t.base.as_str(), t.token.as_deref()), ("http://10.0.0.2:7878", Some("xyz")));
        assert!(Target::parse("  ", "").is_err());
        assert_eq!(new_token().len(), 16);
    }
}
