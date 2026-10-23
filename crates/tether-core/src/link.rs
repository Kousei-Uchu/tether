//! The pairing link a host shows as a QR code and a phone scans:
//!
//! `tether://pair?addr=192.168.1.20%3A7420&code=ABCD-EFGH-IJKL-MNOP&name=studio-pc`
//!
//! It carries what a new device needs to pair: where to connect and the one-time code. It
//! adds nothing to security, since the code is what authenticates the pairing; the link only
//! saves typing.

use crate::pairing::normalize_code;

const SCHEME: &str = "tether://pair?";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingLink {
    /// `host:port` or `host`, as the host showed it.
    pub address: String,
    /// The pairing code, formatted XXXX-XXXX-XXXX-XXXX.
    pub code: String,
    /// The host's device name, if it included one. For display only.
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("this is not a Tether pairing link")]
    NotALink,
    #[error("the link has no address")]
    MissingAddress,
    #[error("the link has no pairing code")]
    MissingCode,
    #[error("the pairing code in the link is not valid")]
    BadCode,
}

impl PairingLink {
    /// Builds a link. `code` may be typed in any case, with or without dashes.
    pub fn new(address: &str, code: &str, name: Option<&str>) -> Result<Self, LinkError> {
        if address.trim().is_empty() {
            return Err(LinkError::MissingAddress);
        }
        let normalized = normalize_code(code).ok_or(LinkError::BadCode)?;
        Ok(Self {
            address: address.trim().to_owned(),
            code: format_code(&normalized),
            name: name.map(str::to_owned).filter(|n| !n.is_empty()),
        })
    }

    pub fn parse(link: &str) -> Result<Self, LinkError> {
        let query = link
            .trim()
            .strip_prefix(SCHEME)
            .ok_or(LinkError::NotALink)?;
        let (mut address, mut code, mut name) = (None, None, None);
        for pair in query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let value = decode(value);
            match key {
                "addr" => address = Some(value),
                "code" => code = Some(value),
                "name" => name = Some(value),
                _ => {}
            }
        }
        let address = address
            .filter(|a| !a.trim().is_empty())
            .ok_or(LinkError::MissingAddress)?;
        let code = code.ok_or(LinkError::MissingCode)?;
        Self::new(&address, &code, name.as_deref())
    }
}

impl std::fmt::Display for PairingLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{SCHEME}addr={}&code={}",
            encode(&self.address),
            encode(&self.code)
        )?;
        if let Some(name) = &self.name {
            write!(f, "&name={}", encode(name))?;
        }
        Ok(())
    }
}

fn format_code(normalized: &str) -> String {
    normalized
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).expect("ascii"))
        .collect::<Vec<_>>()
        .join("-")
}

/// Adds the default port (7420) to an address that has none. Accepts `host`, `host:port`,
/// bare IPv4 and IPv6 addresses, and `[v6]:port`.
pub fn with_default_port(addr: &str) -> String {
    let addr = addr.trim();
    let port = tether_proto::DEFAULT_PORT;
    if addr.parse::<std::net::IpAddr>().is_ok() {
        return if addr.contains(':') {
            format!("[{addr}]:{port}")
        } else {
            format!("{addr}:{port}")
        };
    }
    if addr.starts_with('[') || addr.contains(':') {
        addr.to_owned()
    } else {
        format!("{addr}:{port}")
    }
}

/// Percent-encodes everything except letters, digits and `-._~`.
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_awkward_characters() {
        let link = PairingLink::new(
            "[fe80::1]:7420",
            "abcd efgh-jklm-np0o",
            Some("Sorren's Mac, 2"),
        )
        .unwrap();
        assert_eq!(link.code, "ABCD-EFGH-JK1M-NP00");
        let text = link.to_string();
        assert!(text
            .starts_with("tether://pair?addr=%5Bfe80%3A%3A1%5D%3A7420&code=ABCD-EFGH-JK1M-NP00"));
        assert!(!text.contains(' '));
        assert_eq!(PairingLink::parse(&text).unwrap(), link);
    }

    #[test]
    fn name_is_optional() {
        let text = "tether://pair?addr=192.168.1.20%3A7420&code=0123456789ABCDEF";
        let link = PairingLink::parse(text).unwrap();
        assert_eq!(link.address, "192.168.1.20:7420");
        assert_eq!(link.code, "0123-4567-89AB-CDEF");
        assert_eq!(link.name, None);
    }

    #[test]
    fn rejects_bad_input_with_a_reason() {
        assert_eq!(
            PairingLink::parse("https://example.com"),
            Err(LinkError::NotALink)
        );
        assert_eq!(
            PairingLink::parse("tether://pair?code=0123456789ABCDEF"),
            Err(LinkError::MissingAddress)
        );
        assert_eq!(
            PairingLink::parse("tether://pair?addr=h"),
            Err(LinkError::MissingCode)
        );
        assert_eq!(
            PairingLink::parse("tether://pair?addr=h&code=nope"),
            Err(LinkError::BadCode)
        );
        assert_eq!(
            PairingLink::new("  ", "0123456789ABCDEF", None),
            Err(LinkError::MissingAddress)
        );
    }

    #[test]
    fn adds_the_default_port_only_when_missing() {
        assert_eq!(with_default_port("my-pc"), "my-pc:7420");
        assert_eq!(with_default_port("192.168.1.5"), "192.168.1.5:7420");
        assert_eq!(with_default_port("192.168.1.5:9000"), "192.168.1.5:9000");
        assert_eq!(with_default_port("::1"), "[::1]:7420");
        assert_eq!(with_default_port("[::1]:9000"), "[::1]:9000");
        assert_eq!(with_default_port(" host "), "host:7420");
    }

    #[test]
    fn broken_percent_escapes_do_not_panic() {
        assert!(PairingLink::parse("tether://pair?addr=%&code=%G").is_err());
        assert_eq!(decode("%4"), "%4");
        assert_eq!(decode("100%"), "100%");
    }
}
