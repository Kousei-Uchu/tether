//! A folder shared with paired devices that hold the `files` permission.
//!
//! The host names one folder. Devices can list it, download files from it and upload files
//! into it, and cannot reach anything outside it: every path is checked name by name, then
//! resolved on disk (so a symlink pointing out of the folder is refused too). Uploads are
//! written to a hidden partial file, checked against the sender's SHA-256, and only then given
//! their final name. An upload never replaces a file that is already there.

use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};
use tether_proto::FileEntry;

/// Most entries one listing returns.
pub const MAX_LIST_ENTRIES: usize = 5000;
/// Largest upload a host accepts unless told otherwise.
pub const DEFAULT_MAX_UPLOAD: u64 = 16 * 1024 * 1024 * 1024;

pub struct FileShare {
    root: PathBuf,
    max_upload: u64,
}

impl FileShare {
    /// Shares `root`, which must be an existing folder.
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the shared path is not a folder",
            ));
        }
        Ok(Self {
            root,
            max_upload: DEFAULT_MAX_UPLOAD,
        })
    }

    pub fn with_max_upload(mut self, bytes: u64) -> Self {
        self.max_upload = bytes;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The entries of the folder at `rel` (empty means the shared folder itself), folders
    /// first. Symbolic links and names that could not be asked for again are left out.
    pub fn list(&self, rel: &str) -> Result<Vec<FileEntry>, String> {
        let dir = self.resolve(&split(rel)?)?;
        if !dir.is_dir() {
            return Err("that is not a folder".into());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir).map_err(read_err)? {
            let Ok(entry) = entry else { continue };
            let Ok(kind) = entry.file_type() else { continue };
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if kind.is_symlink() || check_name(&name).is_err() || name.starts_with(".tether-") {
                continue;
            }
            let size = if kind.is_file() {
                entry.metadata().map(|m| m.len()).unwrap_or(0)
            } else {
                0
            };
            out.push(FileEntry {
                name,
                size,
                dir: kind.is_dir(),
            });
            if out.len() >= MAX_LIST_ENTRIES {
                break;
            }
        }
        out.sort_by(|a, b| {
            b.dir
                .cmp(&a.dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }

    /// Opens the file at `rel` for reading, with its size.
    pub fn open_read(&self, rel: &str) -> Result<(File, u64), String> {
        let parts = split(rel)?;
        if parts.is_empty() {
            return Err("that is a folder, not a file".into());
        }
        let path = self.resolve(&parts)?;
        let meta = fs::metadata(&path).map_err(read_err)?;
        if !meta.is_file() {
            return Err("that is not a file".into());
        }
        let file = File::open(&path).map_err(read_err)?;
        Ok((file, meta.len()))
    }

    /// Starts receiving a file of `size` bytes that will be saved as `rel`, which names a file
    /// in the shared folder or in a folder inside it that already exists.
    pub fn begin_upload(&self, id: u32, rel: &str, size: u64) -> Result<Upload, String> {
        if size > self.max_upload {
            return Err(format!(
                "that file is larger than this host accepts ({} GiB)",
                self.max_upload >> 30
            ));
        }
        let mut parts = split(rel)?;
        let Some(name) = parts.pop() else {
            return Err("a file name is needed".into());
        };
        let dir = self.resolve(&parts)?;
        if !dir.is_dir() {
            return Err("the folder to save into does not exist".into());
        }
        let tmp = dir.join(format!(".tether-{id}-{:08x}.part", rand::random::<u32>()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| format!("could not create the file: {e}"))?;
        Ok(Upload {
            id,
            file: Some(file),
            tmp,
            dir,
            name: name.to_owned(),
            expected: size,
            received: 0,
            hasher: Sha256::new(),
        })
    }

    /// The path of an existing entry, which must be inside the shared folder after links are
    /// followed.
    fn resolve(&self, parts: &[&str]) -> Result<PathBuf, String> {
        let mut path = self.root.clone();
        path.extend(parts);
        let real = fs::canonicalize(&path).map_err(read_err)?;
        if !real.starts_with(&self.root) {
            return Err("that path is outside the shared folder".into());
        }
        Ok(real)
    }
}

fn read_err(e: io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "not found".into(),
        io::ErrorKind::PermissionDenied => "the host is not allowed to read that".into(),
        _ => format!("could not read that: {e}"),
    }
}

/// Splits a path from the other device into names, accepting `/` or `\` and ignoring empty
/// parts. Every name must be one this host is willing to use.
fn split(rel: &str) -> Result<Vec<&str>, String> {
    let parts: Vec<&str> = rel.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    for p in &parts {
        check_name(p)?;
    }
    Ok(parts)
}

/// Refuses names that mean something special somewhere: `..`, drive letters and alternate
/// data streams (`:`), Windows device names, names Windows would quietly rewrite (a trailing
/// dot or space), and control characters.
fn check_name(name: &str) -> Result<(), String> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > 255
        || name.ends_with('.')
        || name.ends_with(' ')
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/' | '\\'));
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if bad || device {
        Err(format!("{name:?} is not a name that can be used"))
    } else {
        Ok(())
    }
}

/// A file being received. Dropping it before `finish` deletes the partial file.
pub struct Upload {
    pub id: u32,
    file: Option<File>,
    tmp: PathBuf,
    dir: PathBuf,
    name: String,
    expected: u64,
    received: u64,
    hasher: Sha256,
}

impl Upload {
    pub fn write(&mut self, data: &[u8]) -> Result<(), String> {
        self.received += data.len() as u64;
        if self.received > self.expected {
            return Err("the device sent more data than it announced".into());
        }
        self.hasher.update(data);
        self.file
            .as_mut()
            .expect("upload is open")
            .write_all(data)
            .map_err(|e| format!("could not write the file: {e}"))
    }

    /// Checks the whole file against the sender's hash, then gives it its final name. Returns
    /// that name, which differs from the one asked for if that was taken.
    pub fn finish(mut self, sha256: &[u8; 32]) -> Result<String, String> {
        if self.received != self.expected {
            return Err("the file ended before all of it arrived".into());
        }
        let digest: [u8; 32] = self.hasher.clone().finalize().into();
        if &digest != sha256 {
            return Err("the file was damaged on the way (checksum mismatch)".into());
        }
        let mut file = self.file.take().expect("upload is open");
        file.flush().and_then(|()| file.sync_all()).map_err(|e| format!("could not save the file: {e}"))?;
        drop(file);
        for n in 0..1000u32 {
            let name = numbered(&self.name, n);
            let target = self.dir.join(&name);
            match fs::hard_link(&self.tmp, &target) {
                Ok(()) => {
                    let _ = fs::remove_file(&self.tmp);
                    return Ok(name);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                // Some file systems have no hard links; fall back to a plain rename.
                Err(_) if !target.exists() => {
                    return fs::rename(&self.tmp, &target)
                        .map(|()| name)
                        .map_err(|e| format!("could not save the file: {e}"));
                }
                Err(_) => continue,
            }
        }
        Err("there are too many files with that name".into())
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = fs::remove_file(&self.tmp);
        }
    }
}

/// `report.pdf` for 0, then `report (1).pdf`, `report (2).pdf`...
fn numbered(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_owned();
    }
    match name.rfind('.').filter(|i| *i > 0) {
        Some(i) => format!("{} ({n}){}", &name[..i], &name[i..]),
        None => format!("{name} ({n})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn share() -> (tempfile::TempDir, FileShare) {
        let dir = tempfile::tempdir().unwrap();
        let share = FileShare::new(dir.path()).unwrap();
        (dir, share)
    }

    #[test]
    fn names_that_escape_or_confuse_are_refused() {
        for bad in [
            "..", ".", "a:b", "C:", "x/../y", "con", "NUL.txt", "com1", "lpt9.log", "trail.",
            "space ", "a|b", "q?", "star*", "tab\there",
        ] {
            let result = split(bad).and_then(|p| p.iter().try_for_each(|n| check_name(n)));
            assert!(result.is_err(), "{bad:?} should be refused");
        }
        for good in ["a.txt", "My Photos", ".hidden", "com10", "console", "résumé.pdf"] {
            assert!(check_name(good).is_ok(), "{good:?} should be fine");
        }
        assert_eq!(split("a\\b//c/").unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    fn numbering_keeps_the_extension() {
        assert_eq!(numbered("a.txt", 0), "a.txt");
        assert_eq!(numbered("a.txt", 2), "a (2).txt");
        assert_eq!(numbered("noext", 1), "noext (1)");
        assert_eq!(numbered(".bashrc", 1), ".bashrc (1)");
        assert_eq!(numbered("x.tar.gz", 1), "x.tar (1).gz");
    }

    #[test]
    fn listing_puts_folders_first_and_hides_internals() {
        let (dir, share) = share();
        fs::write(dir.path().join("b.txt"), b"hello").unwrap();
        fs::write(dir.path().join("A.txt"), b"x").unwrap();
        fs::create_dir(dir.path().join("zdir")).unwrap();
        fs::write(dir.path().join(".tether-1-0.part"), b"partial").unwrap();
        let names: Vec<_> = share.list("").unwrap().into_iter().map(|e| (e.name, e.dir)).collect();
        assert_eq!(
            names,
            vec![("zdir".into(), true), ("A.txt".into(), false), ("b.txt".into(), false)]
        );
        assert!(share.list("missing").is_err());
        assert!(share.list("b.txt").is_err());
        assert!(share.list("..").is_err());
    }

    #[test]
    fn reading_stays_inside_the_folder() {
        let (dir, share) = share();
        fs::write(dir.path().join("f.bin"), vec![7u8; 10]).unwrap();
        let (_, size) = share.open_read("f.bin").unwrap();
        assert_eq!(size, 10);
        assert!(share.open_read("../f.bin").is_err());
        assert!(share.open_read("").is_err());
        assert!(share.open_read("nope").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn links_out_of_the_folder_are_refused() {
        let (dir, share) = share();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"s").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("door")).unwrap();
        assert!(share.open_read("door/secret").is_err());
        assert!(share.list("door").is_err());
        assert!(share.list("").unwrap().is_empty(), "links are not listed");
    }

    fn send(share: &FileShare, rel: &str, data: &[u8]) -> Result<String, String> {
        let mut up = share.begin_upload(1, rel, data.len() as u64)?;
        for chunk in data.chunks(1000) {
            up.write(chunk)?;
        }
        let sha: [u8; 32] = Sha256::digest(data).into();
        up.finish(&sha)
    }

    #[test]
    fn uploads_never_replace_and_check_the_hash() {
        let (dir, share) = share();
        assert_eq!(send(&share, "a.txt", b"one").unwrap(), "a.txt");
        assert_eq!(send(&share, "a.txt", b"two").unwrap(), "a (1).txt");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"one");
        assert_eq!(fs::read(dir.path().join("a (1).txt")).unwrap(), b"two");

        let mut up = share.begin_upload(2, "bad.txt", 3).unwrap();
        up.write(b"abc").unwrap();
        assert!(up.finish(&[0; 32]).unwrap_err().contains("checksum"));
        assert!(!dir.path().join("bad.txt").exists());

        let mut short = share.begin_upload(3, "short.txt", 10).unwrap();
        short.write(b"abc").unwrap();
        assert!(short.finish(&Sha256::digest(b"abc").into()).is_err());
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "failed uploads leave nothing behind");
    }

    #[test]
    fn upload_limits_and_destinations() {
        let (dir, share) = share();
        let small = FileShare::new(dir.path()).unwrap().with_max_upload(5);
        assert!(small.begin_upload(1, "big", 6).is_err());
        assert!(share.begin_upload(1, "../x", 1).is_err());
        assert!(share.begin_upload(1, "nodir/x", 1).is_err());
        assert!(share.begin_upload(1, "", 1).is_err());
        fs::create_dir(dir.path().join("sub")).unwrap();
        assert_eq!(send(&share, "sub/in.txt", b"hi").unwrap(), "in.txt");
        assert!(dir.path().join("sub/in.txt").exists());

        let mut up = share.begin_upload(4, "over", 2).unwrap();
        assert!(up.write(b"abc").is_err());
    }
}
