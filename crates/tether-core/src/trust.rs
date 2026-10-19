use crate::{
    identity::{fingerprint, write_private},
    permissions::PermissionSet,
};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
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
    /// What this device may do on the machine that holds this entry. On a client, the
    /// entry for a host is identity only and carries no permissions.
    #[serde(default = "PermissionSet::default_grant")]
    pub permissions: PermissionSet,
}

impl TrustedPeer {
    pub fn new(
        name: String,
        public_key: &[u8],
        platform: Platform,
        permissions: PermissionSet,
    ) -> Self {
        let added_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            name,
            public_key: hex::encode(public_key),
            platform,
            added_unix,
            permissions,
        }
    }

    /// Whether `query` names this peer: its name, or the start of its fingerprint.
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase().replace('-', "");
        let fp = self.fingerprint().replace('-', "");
        self.name.to_lowercase() == q || (!q.is_empty() && fp.starts_with(&q))
    }

    pub fn fingerprint(&self) -> String {
        hex::decode(&self.public_key)
            .map(|k| fingerprint(&k))
            .unwrap_or_default()
    }
}

/// Modification time and size, used to notice edits made by another process (for example
/// `tether peers revoke` while `tether listen` is running).
type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

fn read_peers(path: &Path) -> io::Result<Vec<TrustedPeer>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// The set of devices allowed to connect (host side) or to be connected to (client side).
///
/// The file on disk is the source of truth. Every mutation first calls [`refresh`], so a
/// long running process never overwrites an edit made elsewhere.
///
/// [`refresh`]: TrustStore::refresh
#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    peers: Vec<TrustedPeer>,
    stamp: Stamp,
}

impl TrustStore {
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let peers = read_peers(&path)?;
        let stamp = stamp(&path);
        Ok(Self { path, peers, stamp })
    }

    /// Reloads from disk if another process changed the file. Returns true if it did.
    pub fn refresh(&mut self) -> io::Result<bool> {
        let now = stamp(&self.path);
        if now == self.stamp {
            return Ok(false);
        }
        self.peers = read_peers(&self.path)?;
        self.stamp = now;
        Ok(true)
    }

    pub fn peers(&self) -> &[TrustedPeer] {
        &self.peers
    }

    pub fn get(&self, public: &[u8]) -> Option<&TrustedPeer> {
        let key = hex::encode(public);
        self.peers.iter().find(|p| p.public_key == key)
    }

    pub fn contains(&self, public: &[u8]) -> bool {
        self.get(public).is_some()
    }

    /// Adds or replaces the peer with the same key, then persists.
    pub fn add(&mut self, peer: TrustedPeer) -> io::Result<()> {
        self.refresh()?;
        self.peers.retain(|p| p.public_key != peer.public_key);
        self.peers.push(peer);
        self.save()
    }

    /// Removes peers whose name or fingerprint (dashes optional) matches `query` exactly,
    /// or whose fingerprint starts with it. Returns how many were removed.
    pub fn remove(&mut self, query: &str) -> io::Result<usize> {
        self.refresh()?;
        let before = self.peers.len();
        self.peers.retain(|p| !p.matches(query));
        let removed = before - self.peers.len();
        if removed > 0 {
            self.save()?;
        }
        Ok(removed)
    }

    /// Applies `change` to the permissions of every peer matching `query` (same matching
    /// as [`remove`](Self::remove)). Returns how many peers matched.
    pub fn change_permissions(
        &mut self,
        query: &str,
        change: impl Fn(PermissionSet) -> PermissionSet,
    ) -> io::Result<usize> {
        self.refresh()?;
        let mut matched = 0;
        for p in self.peers.iter_mut().filter(|p| p.matches(query)) {
            p.permissions = change(p.permissions);
            matched += 1;
        }
        if matched > 0 {
            self.save()?;
        }
        Ok(matched)
    }

    fn save(&mut self) -> io::Result<()> {
        let json = serde_json::to_string_pretty(&self.peers).expect("serialize trust store");
        write_private(&self.path, json.as_bytes())?;
        self.stamp = stamp(&self.path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tether_proto::Permission;

    fn store() -> (tempfile::TempDir, TrustStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = TrustStore::load(dir.path().join("trusted.json")).unwrap();
        (dir, s)
    }

    #[test]
    fn add_persist_remove() {
        let (dir, mut s) = store();
        let view = PermissionSet::default_grant();
        s.add(TrustedPeer::new(
            "pc".into(),
            &[1; 32],
            Platform::Windows,
            view,
        ))
        .unwrap();
        s.add(TrustedPeer::new(
            "pc-renamed".into(),
            &[1; 32],
            Platform::Windows,
            view,
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

    #[test]
    fn external_edits_are_picked_up_and_never_clobbered() {
        let (dir, mut a) = store();
        let path = dir.path().join("trusted.json");
        let view = PermissionSet::default_grant();
        a.add(TrustedPeer::new(
            "one".into(),
            &[1; 32],
            Platform::MacOs,
            view,
        ))
        .unwrap();

        // Another process (the CLI) adds a peer and widens the first one.
        let mut b = TrustStore::load(path).unwrap();
        b.add(TrustedPeer::new(
            "two".into(),
            &[2; 32],
            Platform::Linux,
            view,
        ))
        .unwrap();
        b.change_permissions("one", |p| p.with(Permission::Input))
            .unwrap();

        assert!(!a.contains(&[2; 32]), "not seen until refreshed");
        assert!(a.refresh().unwrap());
        assert!(a.contains(&[2; 32]));
        assert!(a
            .get(&[1; 32])
            .unwrap()
            .permissions
            .contains(Permission::Input));
        assert!(!a.refresh().unwrap(), "no change, no reload");

        // A mutation in `a` must keep what `b` wrote after `a` last looked.
        b.add(TrustedPeer::new(
            "three".into(),
            &[3; 32],
            Platform::Linux,
            view,
        ))
        .unwrap();
        a.add(TrustedPeer::new(
            "four".into(),
            &[4; 32],
            Platform::Linux,
            view,
        ))
        .unwrap();
        assert_eq!(a.peers().len(), 4);
    }

    #[test]
    fn legacy_entries_without_permissions_get_view_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted.json");
        let key = hex::encode([5u8; 32]);
        let legacy =
            format!(r#"[{{"name":"old","public_key":"{key}","platform":"Linux","added_unix":1}}]"#);
        std::fs::write(&path, legacy).unwrap();
        let s = TrustStore::load(path).unwrap();
        assert_eq!(s.peers()[0].permissions, PermissionSet::default_grant());
    }
}
