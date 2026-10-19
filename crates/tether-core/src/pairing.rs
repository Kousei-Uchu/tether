//! One-time pairing codes and the proofs derived from them.
//!
//! The code is 80 bits of entropy (16 Crockford base32 characters). That is deliberately
//! long: a man in the middle who talks to the client can capture one proof and brute force
//! it offline, which must be infeasible. Typed entry stays practical and a QR code (later
//! milestone) removes typing entirely.

use crate::permissions::PermissionSet;
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use sha2::Sha256;
use std::time::{Duration, Instant};
use tether_proto::Proof;

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const CODE_LEN: usize = 16;
pub const MAX_ATTEMPTS: u8 = 5;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofRole {
    Client,
    Host,
}

impl ProofRole {
    fn label(self) -> &'static [u8] {
        match self {
            ProofRole::Client => b"tether-pair-v1/client",
            ProofRole::Host => b"tether-pair-v1/host",
        }
    }
}

/// An active pairing window on a host: one code, one use, limited guesses, limited time.
pub struct PairingOffer {
    code: String,
    pub expires_at: Instant,
    pub attempts_left: u8,
    /// Permissions the paired device receives. Least privilege unless the host says otherwise.
    pub grant: PermissionSet,
}

impl PairingOffer {
    pub fn new(ttl: Duration) -> Self {
        Self {
            code: generate_code(),
            expires_at: Instant::now() + ttl,
            attempts_left: MAX_ATTEMPTS,
            grant: PermissionSet::default_grant(),
        }
    }

    /// Sets what the device that pairs with this offer is allowed to do.
    pub fn with_grant(mut self, grant: PermissionSet) -> Self {
        self.grant = grant;
        self
    }

    /// Normalized code (no dashes) used for proofs.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Code formatted for people: XXXX-XXXX-XXXX-XXXX.
    pub fn display_code(&self) -> String {
        format_code(&self.code)
    }
}

fn generate_code() -> String {
    let mut bytes = [0u8; CODE_LEN];
    OsRng.fill_bytes(&mut bytes);
    // 256 is divisible by 32, so masking keeps the distribution uniform.
    bytes
        .iter()
        .map(|b| ALPHABET[(*b & 31) as usize] as char)
        .collect()
}

fn format_code(code: &str) -> String {
    code.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).expect("ascii"))
        .collect::<Vec<_>>()
        .join("-")
}

/// Accepts what a person might type: any case, dashes or spaces, and the Crockford
/// look-alike substitutions (O to 0, I and L to 1). Returns None if it is not a valid code.
pub fn normalize_code(input: &str) -> Option<String> {
    let out: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect();
    let valid = out.len() == CODE_LEN && out.bytes().all(|b| ALPHABET.contains(&b));
    valid.then_some(out)
}

fn mac(code: &str, role: ProofRole, handshake_hash: &[u8]) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(code.as_bytes()).expect("hmac accepts any key length");
    m.update(role.label());
    m.update(handshake_hash);
    m
}

/// HMAC over the Noise handshake hash, so the proof is only valid inside this session.
pub fn proof(code: &str, role: ProofRole, handshake_hash: &[u8]) -> Proof {
    mac(code, role, handshake_hash)
        .finalize()
        .into_bytes()
        .into()
}

/// Constant time check.
pub fn verify(code: &str, role: ProofRole, handshake_hash: &[u8], candidate: &Proof) -> bool {
    mac(code, role, handshake_hash)
        .verify_slice(candidate)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_codes_are_valid_and_unique() {
        let a = generate_code();
        let b = generate_code();
        assert_ne!(a, b);
        assert_eq!(normalize_code(&a).as_deref(), Some(a.as_str()));
        assert_eq!(
            normalize_code(&format_code(&a)).as_deref(),
            Some(a.as_str())
        );
    }

    #[test]
    fn normalize_handles_typing_quirks() {
        assert_eq!(
            normalize_code("abcd efgh-jklm-np0o").as_deref(),
            Some("ABCDEFGHJK1MNP00")
        );
        assert_eq!(normalize_code("short"), None);
        assert_eq!(normalize_code("UUUU-UUUU-UUUU-UUUU"), None);
    }

    #[test]
    fn proofs_bind_role_code_and_session() {
        let code = "0123456789ABCDEF";
        let p = proof(code, ProofRole::Client, b"hash-a");
        assert!(verify(code, ProofRole::Client, b"hash-a", &p));
        assert!(!verify(code, ProofRole::Host, b"hash-a", &p));
        assert!(!verify(code, ProofRole::Client, b"hash-b", &p));
        assert!(!verify(
            "0123456789ABCDEG",
            ProofRole::Client,
            b"hash-a",
            &p
        ));
    }
}
