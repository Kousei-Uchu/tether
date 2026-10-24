//! Runs `tether listen` in the background, started at login.
//!
//! It is a per user service on every platform on purpose. Screen capture and input injection
//! need the logged in user's desktop session: a Windows service in session 0 or a macOS
//! launch daemon cannot see the screen, and the permission prompts (Screen Recording,
//! Accessibility) belong to the user anyway. So: a systemd user unit on Linux, a launch
//! agent on macOS, and a logon task on Windows.

use anyhow::{bail, Context, Result};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub const LABEL: &str = "dev.kousei.tether";
const SYSTEMD_UNIT: &str = "tether.service";
const WINDOWS_TASK: &str = "Tether";

/// What the background host runs.
pub struct Spec {
    pub exe: PathBuf,
    pub home: Option<PathBuf>,
    pub name: Option<String>,
    pub bind: String,
    pub video: Option<String>,
    pub input: bool,
    pub clipboard: bool,
    pub wake: Vec<String>,
}

impl Spec {
    /// The arguments after the executable. No `--pair`: a code that appears at login with
    /// nobody looking is no use. Pair once with `tether listen --pair` or the desktop app.
    pub fn args(&self) -> Vec<String> {
        let mut a = Vec::new();
        if let Some(h) = &self.home {
            a.push("--home".into());
            a.push(h.display().to_string());
        }
        if let Some(n) = &self.name {
            a.push("--name".into());
            a.push(n.clone());
        }
        a.push("listen".into());
        a.push("--bind".into());
        a.push(self.bind.clone());
        if let Some(v) = &self.video {
            a.push("--video".into());
            a.push(v.clone());
        }
        if self.input {
            a.push("--input".into());
        }
        if self.clipboard {
            a.push("--clipboard".into());
        }
        for w in &self.wake {
            a.push("--wake".into());
            a.push(w.clone());
        }
        a
    }
}

fn systemd_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn systemd_unit(spec: &Spec) -> String {
    let mut exec = systemd_quote(&spec.exe.display().to_string());
    for a in spec.args() {
        exec.push(' ');
        exec.push_str(&systemd_quote(&a));
    }
    format!(
        "[Unit]\nDescription=Tether host\nAfter=graphical-session.target\n\n\
         [Service]\nExecStart={exec}\nRestart=on-failure\nRestartSec=5\n\n\
         [Install]\nWantedBy=default.target\n"
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn launchd_plist(spec: &Spec, log: &Path) -> String {
    let mut items = format!(
        "    <string>{}</string>\n",
        xml_escape(&spec.exe.display().to_string())
    );
    for a in spec.args() {
        items.push_str(&format!("    <string>{}</string>\n", xml_escape(&a)));
    }
    let log = xml_escape(&log.display().to_string());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{LABEL}</string>\n  \
         <key>ProgramArguments</key>\n  <array>\n{items}  </array>\n  \
         <key>RunAtLoad</key>\n  <true/>\n  <key>KeepAlive</key>\n  <true/>\n  \
         <key>ProcessType</key>\n  <string>Interactive</string>\n  \
         <key>StandardOutPath</key>\n  <string>{log}</string>\n  \
         <key>StandardErrorPath</key>\n  <string>{log}</string>\n</dict>\n</plist>\n"
    )
}

fn windows_quote(s: &str) -> String {
    if !s.is_empty() && !s.contains([' ', '\t', '"']) {
        return s.to_owned();
    }
    format!("\"{}\"", s.replace('"', "\\\""))
}

/// The command line the logon task runs (the value of `schtasks /TR`).
pub fn windows_command(spec: &Spec) -> String {
    let mut c = windows_quote(&spec.exe.display().to_string());
    for a in spec.args() {
        c.push(' ');
        c.push_str(&windows_quote(&a));
    }
    c
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn systemd_path() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => home_dir()?.join(".config"),
    };
    Ok(base.join("systemd/user").join(SYSTEMD_UNIT))
}

fn launchd_path() -> Result<PathBuf> {
    Ok(home_dir()?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn launchd_log() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/Logs/tether.log"))
}

fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .with_context(|| format!("could not run {cmd}"))?;
    if !status.success() {
        bail!("{cmd} {} failed ({status})", args.join(" "));
    }
    Ok(())
}

fn uid() -> Result<String> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .context("could not run id")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn write(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("could not write {}", path.display()))
}

/// Prints what would be installed, without touching the system.
pub fn show(spec: &Spec) -> Result<()> {
    if cfg!(target_os = "macos") {
        println!("# {}", launchd_path()?.display());
        print!("{}", launchd_plist(spec, &launchd_log()?));
    } else if cfg!(windows) {
        println!("schtasks /TR {}", windows_command(spec));
    } else {
        println!("# {}", systemd_path()?.display());
        print!("{}", systemd_unit(spec));
    }
    Ok(())
}

pub fn install(spec: &Spec, start: bool) -> Result<()> {
    if cfg!(target_os = "macos") {
        let path = launchd_path()?;
        write(&path, &launchd_plist(spec, &launchd_log()?))?;
        println!("wrote {}", path.display());
        if start {
            let domain = format!("gui/{}", uid()?);
            let _ = run(
                "launchctl",
                &["bootout", &domain, &path.display().to_string()],
            );
            run(
                "launchctl",
                &["bootstrap", &domain, &path.display().to_string()],
            )?;
            println!("started; log at {}", launchd_log()?.display());
        }
    } else if cfg!(windows) {
        run(
            "schtasks",
            &[
                "/Create",
                "/TN",
                WINDOWS_TASK,
                "/SC",
                "ONLOGON",
                "/RL",
                "LIMITED",
                "/F",
                "/TR",
                &windows_command(spec),
            ],
        )?;
        println!("created the logon task {WINDOWS_TASK}");
        if start {
            run("schtasks", &["/Run", "/TN", WINDOWS_TASK])?;
            println!("started");
        }
    } else {
        let path = systemd_path()?;
        write(&path, &systemd_unit(spec))?;
        println!("wrote {}", path.display());
        run("systemctl", &["--user", "daemon-reload"])?;
        // Screen capture and input need to know which display to use. A user unit does not
        // inherit these, so hand over the ones from the session `install` runs in.
        let _ = run(
            "systemctl",
            &[
                "--user",
                "import-environment",
                "DISPLAY",
                "XAUTHORITY",
                "WAYLAND_DISPLAY",
            ],
        );
        if start {
            run("systemctl", &["--user", "enable", "--now", SYSTEMD_UNIT])?;
            println!("started; follow it with: journalctl --user -u tether -f");
        } else {
            run("systemctl", &["--user", "enable", SYSTEMD_UNIT])?;
        }
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    if cfg!(target_os = "macos") {
        let path = launchd_path()?;
        let _ = run(
            "launchctl",
            &[
                "bootout",
                &format!("gui/{}", uid()?),
                &path.display().to_string(),
            ],
        );
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    } else if cfg!(windows) {
        let _ = run("schtasks", &["/End", "/TN", WINDOWS_TASK]);
        run("schtasks", &["/Delete", "/TN", WINDOWS_TASK, "/F"])?;
    } else {
        let _ = run("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT]);
        let path = systemd_path()?;
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    println!("removed");
    Ok(())
}

pub fn status() -> Result<()> {
    if cfg!(target_os = "macos") {
        run("launchctl", &["print", &format!("gui/{}/{LABEL}", uid()?)])
    } else if cfg!(windows) {
        run(
            "schtasks",
            &["/Query", "/TN", WINDOWS_TASK, "/V", "/FO", "LIST"],
        )
    } else {
        run(
            "systemctl",
            &["--user", "status", SYSTEMD_UNIT, "--no-pager"],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            exe: PathBuf::from("/opt/my apps/tether"),
            home: Some(PathBuf::from("/data/te ther")),
            name: Some("Kou's \"PC\"".into()),
            bind: "0.0.0.0:7420".into(),
            video: Some("screen".into()),
            input: true,
            clipboard: true,
            wake: vec!["desk=aa:bb:cc:dd:ee:ff".into()],
        }
    }

    #[test]
    fn args_never_ask_for_pairing() {
        let a = spec().args();
        assert_eq!(
            a,
            [
                "--home",
                "/data/te ther",
                "--name",
                "Kou's \"PC\"",
                "listen",
                "--bind",
                "0.0.0.0:7420",
                "--video",
                "screen",
                "--input",
                "--clipboard",
                "--wake",
                "desk=aa:bb:cc:dd:ee:ff"
            ]
        );
        assert!(!a.iter().any(|x| x == "--pair"));
    }

    #[test]
    fn minimal_args() {
        let s = Spec {
            exe: "t".into(),
            home: None,
            name: None,
            bind: "127.0.0.1:1".into(),
            video: None,
            input: false,
            clipboard: false,
            wake: vec![],
        };
        assert_eq!(s.args(), ["listen", "--bind", "127.0.0.1:1"]);
    }

    #[test]
    fn systemd_quotes_spaces_quotes_and_percent() {
        let unit = systemd_unit(&spec());
        assert!(unit.contains("ExecStart=\"/opt/my apps/tether\" \"--home\" \"/data/te ther\""));
        assert!(unit.contains("\"Kou's \\\"PC\\\"\""));
        assert!(unit.contains("Restart=on-failure"));
        assert_eq!(systemd_quote("50%$"), "\"50%%$$\"");
    }

    #[test]
    fn plist_is_escaped_and_keeps_alive() {
        let p = launchd_plist(&spec(), Path::new("/tmp/t&t.log"));
        assert!(p.contains("<string>Kou's &quot;PC&quot;</string>"));
        assert!(p.contains("<string>/tmp/t&amp;t.log</string>"));
        assert!(p.contains("<key>KeepAlive</key>"));
        assert!(p.contains(&format!("<string>{LABEL}</string>")));
    }

    #[test]
    fn windows_command_quotes_only_when_needed() {
        let c = windows_command(&spec());
        assert!(c.starts_with("\"/opt/my apps/tether\" --home \"/data/te ther\" --name "));
        assert!(c.contains("\"Kou's \\\"PC\\\"\""));
        assert!(c.ends_with("--input --clipboard --wake desk=aa:bb:cc:dd:ee:ff"));
    }
}
