//! Finding Tether devices on the local network.
//!
//! A host advertises `_tether._tcp` over mDNS (Bonjour) with its public key in the TXT
//! record. A client scans, then *identifies* what it sees by matching that key against its
//! trust store. The advertisement is not authenticated and is never trusted: connecting to
//! an address found this way still runs the full Noise handshake, which checks the key the
//! device really holds. A spoofed advertisement can only make a connection fail.
//!
//! This is what lets quick connect find a computer whose address has changed.
//!
//! iPhones use the system's Bonjour API instead (raw multicast sockets need an Apple
//! entitlement), so on iOS [`Advertiser::start`] and [`scan`] return
//! [`DiscoveryError::Unsupported`]. The record format below is what the Swift side reads.

use crate::{identity::fingerprint, trust::TrustStore};
use std::{
    io,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tether_proto::{Platform, PROTOCOL_VERSION};
use tokio::net::TcpStream;

pub const SERVICE_TYPE: &str = "_tether._tcp.local.";

/// TXT keys: `v` protocol version, `pk` hex public key, `fp` fingerprint (for readers that
/// cannot compute it), `plat` platform, `name` device name.
pub const TXT_VERSION: &str = "v";
pub const TXT_PUBLIC_KEY: &str = "pk";
pub const TXT_FINGERPRINT: &str = "fp";
pub const TXT_PLATFORM: &str = "plat";
pub const TXT_NAME: &str = "name";

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("device discovery failed: {0}")]
    Mdns(String),
    #[error("device discovery is not available here; use the system's Bonjour API")]
    Unsupported,
}

/// A device seen on the network. Nothing in it is authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub name: String,
    pub public_key: [u8; 32],
    pub fingerprint: String,
    pub platform: String,
    pub version: u16,
    /// Reachable addresses, IPv4 first. Link-local IPv6 is left out because it needs a
    /// scope id that the advertisement does not carry.
    pub addresses: Vec<SocketAddr>,
}

impl Discovered {
    /// Builds one from an advertisement. `None` if the key is missing or malformed, or
    /// there is no usable address.
    pub fn from_txt(
        instance: &str,
        addresses: impl IntoIterator<Item = IpAddr>,
        port: u16,
        txt: impl Fn(&str) -> Option<String>,
    ) -> Option<Self> {
        let key = hex::decode(txt(TXT_PUBLIC_KEY)?).ok()?;
        let public_key: [u8; 32] = key.try_into().ok()?;
        let mut addresses: Vec<SocketAddr> = addresses
            .into_iter()
            .filter(|ip| usable(ip))
            .map(|ip| SocketAddr::new(ip, port))
            .collect();
        addresses.sort_by_key(|a| (a.is_ipv6(), *a));
        addresses.dedup();
        if addresses.is_empty() || port == 0 {
            return None;
        }
        Some(Self {
            name: txt(TXT_NAME)
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| instance.to_owned()),
            fingerprint: fingerprint(&public_key),
            public_key,
            platform: txt(TXT_PLATFORM).unwrap_or_default(),
            version: txt(TXT_VERSION).and_then(|v| v.parse().ok()).unwrap_or(0),
            addresses,
        })
    }

    /// Whether the device speaks this build's protocol version.
    pub fn compatible(&self) -> bool {
        self.version == PROTOCOL_VERSION
    }

    /// Whether `query` is this device's fingerprint (dashes and case ignored, a prefix is
    /// enough) or exact name.
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase().replace('-', "");
        let fp = self.fingerprint.to_lowercase().replace('-', "");
        self.name.to_lowercase() == q || (!q.is_empty() && fp.starts_with(&q))
    }
}

fn usable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !(v4.is_unspecified() || v4.is_multicast() || v4.is_loopback()),
        IpAddr::V6(v6) => {
            !(v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_loopback()
                || (v6.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

/// The TXT records a host advertises.
pub fn txt_records(name: &str, public: &[u8; 32], platform: Platform) -> Vec<(String, String)> {
    let mut name = name.trim().to_owned();
    while name.len() > 100 {
        name.pop();
    }
    vec![
        (TXT_VERSION.into(), PROTOCOL_VERSION.to_string()),
        (TXT_PUBLIC_KEY.into(), hex::encode(public)),
        (TXT_FINGERPRINT.into(), fingerprint(public)),
        (TXT_PLATFORM.into(), platform.to_string()),
        (TXT_NAME.into(), name),
    ]
}

/// The mDNS instance name: the device name with a short key suffix, so two computers with
/// the same hostname do not collide.
pub fn instance_name(name: &str, public: &[u8; 32]) -> String {
    let clean: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '.')
        .take(40)
        .collect();
    let clean = clean.trim();
    let clean = if clean.is_empty() { "tether" } else { clean };
    format!("{clean} {}", hex::encode(&public[..3]))
}

/// Finds the entry of `trust` that `device` is, by public key.
pub fn identify<'a>(
    device: &Discovered,
    trust: &'a TrustStore,
) -> Option<&'a crate::trust::TrustedPeer> {
    trust.get(&device.public_key)
}

/// Opens a TCP connection to the first address of `device` that answers.
pub async fn dial(device: &Discovered, per_try: Duration) -> io::Result<(TcpStream, SocketAddr)> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "the device advertised no address");
    for addr in &device.addresses {
        match tokio::time::timeout(per_try, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                let _ = stream.set_nodelay(true);
                return Ok((stream, *addr));
            }
            Ok(Err(e)) => last = e,
            Err(_) => last = io::Error::new(io::ErrorKind::TimedOut, format!("{addr} timed out")),
        }
    }
    Err(last)
}

/// Looks for devices for `duration`. Pass this device's own key as `own` to leave it out.
pub async fn scan(
    duration: Duration,
    own: Option<[u8; 32]>,
) -> Result<Vec<Discovered>, DiscoveryError> {
    tokio::task::spawn_blocking(move || imp::scan_blocking(duration, own.as_ref(), |_| false))
        .await
        .map_err(|e| DiscoveryError::Mdns(e.to_string()))?
}

/// Looks for one device by fingerprint (or name), returning as soon as it shows up.
pub async fn find(
    query: &str,
    duration: Duration,
    own: Option<[u8; 32]>,
) -> Result<Option<Discovered>, DiscoveryError> {
    let query = query.to_owned();
    let query2 = query.clone();
    let found = tokio::task::spawn_blocking(move || {
        imp::scan_blocking(duration, own.as_ref(), |d| d.matches(&query))
    })
    .await
    .map_err(|e| DiscoveryError::Mdns(e.to_string()))??;
    // `scan_blocking` stops at the match, so a match is the last one collected.
    Ok(found.into_iter().last().filter(|d| d.matches(&query2)))
}

pub use imp::Advertiser;

#[cfg(not(target_os = "ios"))]
mod imp {
    use super::*;
    use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
    use std::{collections::HashMap, time::Instant};

    fn mdns(e: impl std::fmt::Display) -> DiscoveryError {
        DiscoveryError::Mdns(e.to_string())
    }

    /// Keeps this device visible to others until dropped.
    pub struct Advertiser {
        daemon: ServiceDaemon,
        fullname: String,
        instance: String,
    }

    impl Advertiser {
        pub fn start(
            name: &str,
            public: &[u8; 32],
            platform: Platform,
            port: u16,
        ) -> Result<Self, DiscoveryError> {
            let daemon = ServiceDaemon::new().map_err(mdns)?;
            let instance = instance_name(name, public);
            let host = format!("tether-{}.local.", hex::encode(&public[..6]));
            let props: HashMap<String, String> =
                txt_records(name, public, platform).into_iter().collect();
            let info = ServiceInfo::new(SERVICE_TYPE, &instance, &host, "", port, props)
                .map_err(mdns)?
                .enable_addr_auto();
            let fullname = info.get_fullname().to_owned();
            daemon.register(info).map_err(mdns)?;
            Ok(Self {
                daemon,
                fullname,
                instance,
            })
        }

        /// The name other devices see this one under.
        pub fn instance(&self) -> &str {
            &self.instance
        }
    }

    impl Drop for Advertiser {
        fn drop(&mut self) {
            let _ = self.daemon.unregister(&self.fullname);
            let _ = self.daemon.shutdown();
        }
    }

    /// Blocks for up to `duration`, or until `stop` accepts a device. When it stops early,
    /// that device is the last in the list.
    pub fn scan_blocking(
        duration: Duration,
        own: Option<&[u8; 32]>,
        stop: impl Fn(&Discovered) -> bool,
    ) -> Result<Vec<Discovered>, DiscoveryError> {
        let daemon = ServiceDaemon::new().map_err(mdns)?;
        let rx = daemon.browse(SERVICE_TYPE).map_err(mdns)?;
        let deadline = Instant::now() + duration;
        let mut found: Vec<Discovered> = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let Ok(event) = rx.recv_timeout(left) else {
                break;
            };
            let ServiceEvent::ServiceResolved(info) = event else {
                continue;
            };
            let instance = info
                .get_fullname()
                .trim_end_matches(SERVICE_TYPE)
                .trim_end_matches('.')
                .to_owned();
            let Some(device) = Discovered::from_txt(
                &instance,
                info.get_addresses().iter().copied(),
                info.get_port(),
                |k| info.get_property_val_str(k).map(str::to_owned),
            ) else {
                continue;
            };
            if own == Some(&device.public_key) {
                continue;
            }
            found.retain(|d| d.public_key != device.public_key);
            let done = stop(&device);
            found.push(device);
            if done {
                break;
            }
        }
        let _ = daemon.stop_browse(SERVICE_TYPE);
        let _ = daemon.shutdown();
        Ok(found)
    }
}

#[cfg(target_os = "ios")]
mod imp {
    use super::*;

    pub struct Advertiser;

    impl Advertiser {
        pub fn start(
            _name: &str,
            _public: &[u8; 32],
            _platform: Platform,
            _port: u16,
        ) -> Result<Self, DiscoveryError> {
            Err(DiscoveryError::Unsupported)
        }

        pub fn instance(&self) -> &str {
            ""
        }
    }

    pub fn scan_blocking(
        _duration: Duration,
        _own: Option<&[u8; 32]>,
        _stop: impl Fn(&Discovered) -> bool,
    ) -> Result<Vec<Discovered>, DiscoveryError> {
        Err(DiscoveryError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, net::Ipv4Addr};

    fn advert(public: &[u8; 32]) -> HashMap<String, String> {
        txt_records("Studio PC", public, Platform::Windows)
            .into_iter()
            .collect()
    }

    fn parse(txt: &HashMap<String, String>, ips: Vec<IpAddr>) -> Option<Discovered> {
        Discovered::from_txt("inst", ips, 7420, |k| txt.get(k).cloned())
    }

    #[test]
    fn advertisement_round_trips() {
        let key = [7u8; 32];
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
        let d = parse(&advert(&key), vec![ip]).unwrap();
        assert_eq!(d.name, "Studio PC");
        assert_eq!(d.public_key, key);
        assert_eq!(d.fingerprint, fingerprint(&key));
        assert_eq!(d.platform, "Windows");
        assert!(d.compatible());
        assert_eq!(d.addresses, vec![SocketAddr::new(ip, 7420)]);
        assert!(d.matches(&d.fingerprint[..9]));
        assert!(d.matches("studio pc"));
        assert!(!d.matches("other"));
    }

    #[test]
    fn bad_keys_and_unusable_addresses_are_dropped() {
        let ok = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        let mut txt = advert(&[1; 32]);
        txt.insert(TXT_PUBLIC_KEY.into(), "abcd".into());
        assert!(parse(&txt, vec![ok]).is_none(), "short key");
        txt.insert(TXT_PUBLIC_KEY.into(), "zz".repeat(32));
        assert!(parse(&txt, vec![ok]).is_none(), "not hex");
        txt.remove(TXT_PUBLIC_KEY);
        assert!(parse(&txt, vec![ok]).is_none(), "missing key");

        let good = advert(&[1; 32]);
        let link_local: IpAddr = "fe80::1".parse().unwrap();
        assert!(parse(&good, vec![link_local, Ipv4Addr::LOCALHOST.into()]).is_none());
    }

    #[test]
    fn ipv4_comes_first_and_duplicates_collapse() {
        let v6: IpAddr = "2001:db8::5".parse().unwrap();
        let a = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9));
        let d = parse(&advert(&[2; 32]), vec![v6, a, a]).unwrap();
        assert_eq!(d.addresses.len(), 2);
        assert!(d.addresses[0].is_ipv4());
    }

    #[test]
    fn instance_names_are_safe_and_distinct() {
        let a = instance_name("my.host\n", &[1; 32]);
        assert_eq!(a, "myhost 010101");
        assert_ne!(a, instance_name("my.host", &[2; 32]));
        assert_eq!(instance_name("  ", &[0; 32]), "tether 000000");
        assert!(instance_name(&"x".repeat(200), &[0; 32]).len() < 63);
    }

    #[test]
    fn old_protocol_versions_are_flagged() {
        let mut txt = advert(&[3; 32]);
        txt.insert(TXT_VERSION.into(), "1".into());
        let d = parse(&txt, vec![Ipv4Addr::new(10, 0, 0, 3).into()]).unwrap();
        assert!(!d.compatible());
    }
}
