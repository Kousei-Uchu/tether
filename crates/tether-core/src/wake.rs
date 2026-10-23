//! Wake-on-LAN. A host that stays on (a Raspberry Pi, say) can be given wake targets and
//! will broadcast the magic packet when a device holding `power` asks. Magic packets do not
//! cross the internet, so the always-on device has to live on the same network as the
//! computer being woken.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

pub type Mac = [u8; 6];

/// Sends wake signals on behalf of a connected device. `target` is the name the device asked
/// for. The reply is shown to the person, so keep it readable.
pub trait WakeProvider: Send + Sync {
    fn wake(&self, target: &str) -> Result<String, String>;
}

/// `aa:bb:cc:dd:ee:ff` or `aa-bb-cc-dd-ee-ff`.
pub fn parse_mac(s: &str) -> Result<Mac, String> {
    let bad = || format!("{s:?} is not a MAC address like aa:bb:cc:dd:ee:ff");
    let parts: Vec<&str> = s.trim().split([':', '-']).collect();
    if parts.len() != 6 {
        return Err(bad());
    }
    let mut mac = [0u8; 6];
    for (slot, part) in mac.iter_mut().zip(parts) {
        if part.len() != 2 {
            return Err(bad());
        }
        *slot = u8::from_str_radix(part, 16).map_err(|_| bad())?;
    }
    Ok(mac)
}

/// Six bytes of 0xFF, then the MAC sixteen times.
pub fn magic_packet(mac: Mac) -> [u8; 102] {
    let mut p = [0xFFu8; 102];
    for i in 0..16 {
        p[6 + i * 6..12 + i * 6].copy_from_slice(&mac);
    }
    p
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeTarget {
    pub name: String,
    pub mac: Mac,
    /// Where the packet is broadcast. Use the subnet broadcast address (for example
    /// 192.168.1.255) if the all-ones default does not reach the machine.
    pub broadcast: SocketAddr,
}

impl WakeTarget {
    /// `name=MAC`, or `name=MAC@broadcast` / `name=MAC@broadcast:port`. Port 9 by default.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let (name, rest) = spec
            .split_once('=')
            .ok_or_else(|| format!("{spec:?} should look like name=aa:bb:cc:dd:ee:ff"))?;
        let name = name.trim();
        if name.is_empty() {
            return Err("a wake target needs a name".into());
        }
        let (mac, broadcast) = match rest.split_once('@') {
            Some((mac, b)) => (mac, Some(b.trim())),
            None => (rest, None),
        };
        let broadcast = match broadcast {
            None => SocketAddr::from((Ipv4Addr::BROADCAST, 9)),
            Some(b) => b
                .parse::<SocketAddr>()
                .or_else(|_| b.parse::<Ipv4Addr>().map(|ip| SocketAddr::from((ip, 9))))
                .map_err(|_| format!("{b:?} is not a broadcast address like 192.168.1.255"))?,
        };
        Ok(Self {
            name: name.to_owned(),
            mac: parse_mac(mac)?,
            broadcast,
        })
    }
}

/// The wake provider for a fixed list of targets.
pub struct WolRelay {
    targets: Vec<WakeTarget>,
}

impl WolRelay {
    pub fn new(targets: Vec<WakeTarget>) -> Self {
        Self { targets }
    }

    pub fn from_specs<S: AsRef<str>>(specs: &[S]) -> Result<Self, String> {
        let targets = specs
            .iter()
            .map(|s| WakeTarget::parse(s.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::new(targets))
    }

    pub fn names(&self) -> Vec<&str> {
        self.targets.iter().map(|t| t.name.as_str()).collect()
    }

    fn find(&self, name: &str) -> Option<&WakeTarget> {
        self.targets
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name.trim()))
    }
}

impl WakeProvider for WolRelay {
    fn wake(&self, target: &str) -> Result<String, String> {
        let Some(t) = self.find(target) else {
            let known = self.names().join(", ");
            return Err(format!(
                "This host does not know a computer called {target:?}. It can wake: {known}."
            ));
        };
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .map_err(|e| format!("could not open a socket: {e}"))?;
        socket
            .set_broadcast(true)
            .map_err(|e| format!("could not enable broadcast: {e}"))?;
        socket
            .send_to(&magic_packet(t.mac), t.broadcast)
            .map_err(|e| format!("could not send the wake packet: {e}"))?;
        Ok(format!("Sent a wake signal to {}.", t.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_macs() {
        assert_eq!(
            parse_mac("aa:BB:cc:00:ee:0f"),
            Ok([0xaa, 0xbb, 0xcc, 0, 0xee, 0x0f])
        );
        assert_eq!(
            parse_mac("aa-bb-cc-dd-ee-ff"),
            Ok([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff])
        );
        for bad in [
            "",
            "aa:bb",
            "aa:bb:cc:dd:ee:fg",
            "aabbccddeeff",
            "a:b:c:d:e:f",
            "aa:bb:cc:dd:ee:ff:00",
        ] {
            assert!(parse_mac(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn magic_packet_layout() {
        let mac = [1, 2, 3, 4, 5, 6];
        let p = magic_packet(mac);
        assert_eq!(&p[..6], &[0xFF; 6]);
        for i in 0..16 {
            assert_eq!(&p[6 + i * 6..12 + i * 6], &mac);
        }
        assert_eq!(p.len(), 102);
    }

    #[test]
    fn parses_targets() {
        let t = WakeTarget::parse("desk=aa:bb:cc:dd:ee:ff").unwrap();
        assert_eq!(t.name, "desk");
        assert_eq!(t.broadcast, "255.255.255.255:9".parse().unwrap());
        let t = WakeTarget::parse("desk=aa:bb:cc:dd:ee:ff@192.168.1.255").unwrap();
        assert_eq!(t.broadcast, "192.168.1.255:9".parse().unwrap());
        let t = WakeTarget::parse("desk=aa:bb:cc:dd:ee:ff@10.0.0.255:7").unwrap();
        assert_eq!(t.broadcast, "10.0.0.255:7".parse().unwrap());
        for bad in [
            "desk",
            "=aa:bb:cc:dd:ee:ff",
            "desk=nope",
            "desk=aa:bb:cc:dd:ee:ff@x",
        ] {
            assert!(WakeTarget::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sends_the_packet_to_the_target() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let relay =
            WolRelay::from_specs(&[format!("Desk=aa:bb:cc:dd:ee:ff@127.0.0.1:{port}")]).unwrap();
        // names match without regard to case
        assert!(relay.wake("desk").unwrap().contains("Desk"));
        let mut buf = [0u8; 200];
        let n = listener.recv(&mut buf).unwrap();
        assert_eq!(
            &buf[..n],
            &magic_packet([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff])[..]
        );
        let err = relay.wake("laptop").unwrap_err();
        assert!(err.contains("Desk"), "{err}");
    }
}
