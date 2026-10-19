use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io, path::Path};

/// Long-term X25519 key pair for this device. The public half is the device identity.
pub struct Identity {
    private: Vec<u8>,
    pub public: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct IdentityFile {
    version: u8,
    private: String,
    public: String,
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("identity file is corrupt: {0}")]
    Corrupt(String),
    #[error("key generation failed: {0}")]
    Generate(snow::Error),
}

impl Identity {
    pub fn generate() -> Result<Self, IdentityError> {
        let params = tether_proto::NOISE_PARAMS
            .parse()
            .expect("static noise params");
        let kp = snow::Builder::new(params)
            .generate_keypair()
            .map_err(IdentityError::Generate)?;
        let public: [u8; 32] = kp
            .public
            .try_into()
            .map_err(|_| IdentityError::Corrupt("unexpected public key length".into()))?;
        Ok(Self {
            private: kp.private,
            public,
        })
    }

    pub fn load_or_create(path: &Path) -> Result<Self, IdentityError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_json(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let id = Self::generate()?;
                id.save(path)?;
                Ok(id)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn from_json(text: &str) -> Result<Self, IdentityError> {
        let f: IdentityFile =
            serde_json::from_str(text).map_err(|e| IdentityError::Corrupt(e.to_string()))?;
        let private = hex::decode(&f.private).map_err(|e| IdentityError::Corrupt(e.to_string()))?;
        let public = hex::decode(&f.public).map_err(|e| IdentityError::Corrupt(e.to_string()))?;
        let public: [u8; 32] = public
            .try_into()
            .map_err(|_| IdentityError::Corrupt("public key length".into()))?;
        if private.len() != 32 {
            return Err(IdentityError::Corrupt("private key length".into()));
        }
        Ok(Self { private, public })
    }

    fn save(&self, path: &Path) -> Result<(), IdentityError> {
        let f = IdentityFile {
            version: 1,
            private: hex::encode(&self.private),
            public: hex::encode(self.public),
        };
        let json = serde_json::to_string_pretty(&f).expect("serialize identity");
        write_private(path, json.as_bytes())?;
        Ok(())
    }

    pub fn private_key(&self) -> &[u8] {
        &self.private
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public)
    }
}

/// Human comparable device id: SHA-256 of the public key, first 16 bytes, in groups of 4 hex.
pub fn fingerprint(public: &[u8]) -> String {
    let digest = Sha256::digest(public);
    let hexed = hex::encode(&digest[..16]);
    hexed
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).expect("hex is ascii"))
        .collect::<Vec<_>>()
        .join("-")
}

/// Writes via a temp file and rename. On Unix the file is created with mode 0600.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_then_reload_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        let a = Identity::load_or_create(&path).unwrap();
        let b = Identity::load_or_create(&path).unwrap();
        assert_eq!(a.public, b.public);
        assert_eq!(a.private_key(), b.private_key());
    }

    #[test]
    fn fingerprint_shape() {
        let fp = fingerprint(&[1u8; 32]);
        assert_eq!(fp.len(), 8 * 4 + 7);
        assert_eq!(fp.matches('-').count(), 7);
    }

    #[cfg(unix)]
    #[test]
    fn identity_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        Identity::load_or_create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
