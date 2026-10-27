//! Announcing this host on the local network (mDNS / DNS-SD), so viewers list it without
//! anyone typing an address.

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use vmon_proto::MDNS_SERVICE;

/// Announces the server until the returned daemon is dropped.
pub fn advertise(port: u16, token_required: bool) -> Result<ServiceDaemon> {
    let mdns = ServiceDaemon::new()?;
    let name = crate::host_name();
    let host = format!("{}.local.", name.replace(' ', "-"));
    let properties = [
        ("token", if token_required { "required" } else { "none" }),
        ("version", env!("CARGO_PKG_VERSION")),
    ];
    let info = ServiceInfo::new(MDNS_SERVICE, &name, &host, "", port, &properties[..])?.enable_addr_auto();
    mdns.register(info)?;
    Ok(mdns)
}
