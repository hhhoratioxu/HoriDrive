//! mDNS is an untrusted address hint. It NEVER establishes trust.
use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    time::{Duration, Instant},
};
const SERVICE: &str = "_horidrive._udp.local.";
pub struct Advertisement {
    daemon: ServiceDaemon,
    full_name: String,
}
impl Advertisement {
    pub fn start(address: SocketAddr, fingerprint: &str) -> Result<Self> {
        anyhow::ensure!(
            fingerprint.len() == 64 && fingerprint.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid discovery fingerprint"
        );
        let daemon = ServiceDaemon::new()?;
        let name = format!("HoriDrive-{}", &fingerprint[..12]);
        let host = format!("horidrive-{}.local.", &fingerprint[..12]);
        let ip = if address.ip().is_unspecified() {
            String::new()
        } else {
            address.ip().to_string()
        };
        let mut service = ServiceInfo::new(
            SERVICE,
            &name,
            &host,
            ip,
            address.port(),
            &[("fingerprint", fingerprint), ("protocol", "1")][..],
        )?;
        if address.ip().is_unspecified() {
            service = service.enable_addr_auto();
        }
        let full_name = service.get_fullname().to_string();
        daemon.register(service)?;
        Ok(Self { daemon, full_name })
    }
}
impl Drop for Advertisement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.full_name);
        let _ = self.daemon.shutdown();
    }
}
#[derive(Serialize)]
pub struct Discovered {
    pub name: String,
    pub addresses: Vec<String>,
    pub fingerprint: String,
}
pub fn browse(duration: Duration) -> Result<Vec<Discovered>> {
    let daemon = ServiceDaemon::new()?;
    let events = daemon.browse(SERVICE)?;
    let deadline = Instant::now() + duration;
    let mut found = BTreeMap::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let fingerprint = info
                    .get_property_val_str("fingerprint")
                    .unwrap_or_default()
                    .to_string();
                if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
                    continue;
                }
                let addresses = info
                    .get_addresses_v4()
                    .iter()
                    .map(|ip| format!("{ip}:{}", info.get_port()))
                    .collect();
                found.insert(
                    info.get_fullname().to_string(),
                    Discovered {
                        name: info
                            .get_fullname()
                            .trim_end_matches(SERVICE)
                            .trim_end_matches('.')
                            .to_string(),
                        addresses,
                        fingerprint,
                    },
                );
            }
            Ok(ServiceEvent::ServiceRemoved(_, name)) => {
                found.remove(&name);
            }
            Err(_) => break,
            _ => (),
        }
    }
    let _ = daemon.stop_browse(SERVICE);
    let _ = daemon.shutdown();
    Ok(found.into_values().collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manual_discovery_is_bounded() -> Result<()> {
        let start = Instant::now();
        let _ = browse(Duration::from_millis(100))?;
        assert!(start.elapsed() < Duration::from_secs(3));
        Ok(())
    }
    #[test]
    #[ignore = "Requires a local interface with working multicast"]
    fn advertises_and_resolves_real_mdns_service() -> Result<()> {
        let fingerprint = blake3::hash(format!("{:?}", Instant::now()).as_bytes())
            .to_hex()
            .to_string();
        let _advertisement = Advertisement::start("0.0.0.0:44330".parse()?, &fingerprint)?;
        let devices = browse(Duration::from_secs(5))?;
        anyhow::ensure!(
            devices
                .iter()
                .any(|d| d.fingerprint == fingerprint && !d.addresses.is_empty()),
            "mDNS service did not resolve on this network"
        );
        Ok(())
    }
}
