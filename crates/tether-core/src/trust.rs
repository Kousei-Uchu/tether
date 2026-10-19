use crate::identity::{fingerprint, write_private};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tether_proto::Platform;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedPeer {
    pub name: String,
    /// Hex encoded X25519 public key.
    pub public_key: String,
    pub platform: Platform,
    pub added_unix: u64,
}

impl TrustedPeer {
    pub fn new(name: String, public_key: &[u8], platform: Platform) -> Self {
        let added_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            name,
            public_key: hex::encode(public_key),
            platform,
            added_unix,
        }
    }

    pub fn fingerprint(&self) -> String {
        hex::decode(&self.public_key)
            .map(|k| fingerprint(&k))
            .unwrap_or_default()
    }
}

/// The set of devices allowed to connect (host side) or to be connected to (client side).
#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    peers: Vec<TrustedPeer>,
}

impl TrustStore {
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let peers = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path, peers })
    }

    pub fn peers(&self) -> &[TrustedPeer] {
        &self.peers
    }

    pub fn contains(&self, public: &[u8]) -> bool {
        let key = hex::encode(public);
        self.peers.iter().any(|p| p.public_key == key)
    }

    /// Adds or replaces the peer with the same key, then persists.
    pub fn add(&mut self, peer: TrustedPeer) -> io::Result<()> {
        self.peers.retain(|p| p.public_key != peer.public_key);
        self.peers.push(peer);
        self.save()
    }

    /// Removes peers whose name or fingerprint (dashes optional) matches `query` exactly,
    /// or whose fingerprint starts with it. Returns how many were removed.
    pub fn remove(&mut self, query: &str) -> io::Result<usize> {
        let q = query.trim().to_lowercase().replace('-', "");
        let before = self.peers.len();
        self.peers.retain(|p| {
            let fp = p.fingerprint().replace('-', "");
            let hit = p.name.to_lowercase() == q || (!q.is_empty() && fp.starts_with(&q));
            !hit
        });
        let removed = before - self.peers.len();
        if removed > 0 {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> io::Result<()> {
        let json = serde_json::to_string_pretty(&self.peers).expect("serialize trust store");
        write_private(&self.path, json.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, TrustStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = TrustStore::load(dir.path().join("trusted.json")).unwrap();
        (dir, s)
    }

    #[test]
    fn add_persist_remove() {
        let (dir, mut s) = store();
        s.add(TrustedPeer::new("pc".into(), &[1; 32], Platform::Windows))
            .unwrap();
        s.add(TrustedPeer::new(
            "pc-renamed".into(),
            &[1; 32],
            Platform::Windows,
        ))
        .unwrap();
        assert_eq!(s.peers().len(), 1);
        assert!(s.contains(&[1; 32]));
        assert!(!s.contains(&[2; 32]));

        let reloaded = TrustStore::load(dir.path().join("trusted.json")).unwrap();
        assert_eq!(reloaded.peers().len(), 1);

        let fp = reloaded.peers()[0].fingerprint();
        let mut s = reloaded;
        assert_eq!(s.remove(&fp[..9]).unwrap(), 1);
        assert!(s.peers().is_empty());
    }
}
