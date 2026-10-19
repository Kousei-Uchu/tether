use directories::ProjectDirs;
use std::path::PathBuf;

/// Where this device keeps its identity and trust store.
#[derive(Debug, Clone)]
pub struct Paths {
    pub dir: PathBuf,
}

impl Paths {
    /// `override_dir` wins (used for `--home` / `TETHER_HOME`, handy for running two
    /// instances on one machine). Otherwise the per-user data directory is used.
    pub fn resolve(override_dir: Option<PathBuf>) -> std::io::Result<Self> {
        let dir = match override_dir {
            Some(d) => d,
            None => ProjectDirs::from("", "", "tether")
                .map(|p| p.data_dir().to_path_buf())
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory found")
                })?,
        };
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn identity_file(&self) -> PathBuf {
        self.dir.join("identity.json")
    }

    pub fn trust_file(&self) -> PathBuf {
        self.dir.join("trusted.json")
    }
}

pub fn default_device_name() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}
