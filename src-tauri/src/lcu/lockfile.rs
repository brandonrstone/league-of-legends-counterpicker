use anyhow::{Context, Result};
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static PORT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"--app-port=(\d+)").expect("port regex"));
static TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"--remoting-auth-token=([\w-]+)").expect("token regex"));
static INSTALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"--install-directory=("[^"]+"|\S+)"#).expect("install regex"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockfileInfo {
    pub port: u16,
    pub password: String,
}

pub fn discover_lockfile() -> Option<LockfileInfo> {
    if let Some(info) = from_process_command_line() {
        return Some(info);
    }
    from_common_paths()
}

fn from_process_command_line() -> Option<LockfileInfo> {
    let cmdline = league_ux_command_line()?;
    parse_command_line(&cmdline)
}

fn parse_command_line(cmdline: &str) -> Option<LockfileInfo> {
    let port = PORT_RE
        .captures(cmdline)?
        .get(1)?
        .as_str()
        .parse::<u16>()
        .ok()?;
    let password = TOKEN_RE.captures(cmdline)?.get(1)?.as_str().to_string();
    if password.is_empty() {
        return None;
    }
    Some(LockfileInfo { port, password })
}

fn from_common_paths() -> Option<LockfileInfo> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(cmdline) = league_ux_command_line() {
        if let Some(dir) = INSTALL_RE.captures(&cmdline).and_then(|c| c.get(1)) {
            let dir = dir.as_str().trim_matches('"');
            paths.push(Path::new(dir).join("lockfile"));
        }
    }
    paths.push(PathBuf::from(r"C:\Riot Games\League of Legends\lockfile"));
    paths.push(PathBuf::from(r"D:\Riot Games\League of Legends\lockfile"));
    paths.push(PathBuf::from(r"E:\Riot Games\League of Legends\lockfile"));
    if let Ok(home) = std::env::var("USERPROFILE") {
        paths.push(
            PathBuf::from(home)
                .join("Riot Games")
                .join("League of Legends")
                .join("lockfile"),
        );
    }
    for path in paths {
        if let Ok(info) = read_lockfile(&path) {
            return Some(info);
        }
    }
    None
}

fn read_lockfile(path: &Path) -> Result<LockfileInfo> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_lockfile_contents(&raw).context("invalid lockfile")
}

fn parse_lockfile_contents(raw: &str) -> Option<LockfileInfo> {
    let line = raw.lines().next()?.trim();
    let parts: Vec<&str> = line.split(':').collect();
    if parts.len() < 5 {
        return None;
    }
    let port = parts[2].parse::<u16>().ok()?;
    let password = parts[3].to_string();
    Some(LockfileInfo { port, password })
}

fn league_ux_command_line() -> Option<String> {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_Process -Filter \"Name = 'LeagueClientUx.exe'\" | Select-Object -ExpandProperty CommandLine",
        ]);
        cmd.creation_flags(CREATE_NO_WINDOW);
        let output = cmd.output().ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lockfile_line() {
        let info = parse_lockfile_contents("LeagueClient:12345:54321:s3cret-token:https")
            .expect("lockfile");
        assert_eq!(info.port, 54321);
        assert_eq!(info.password, "s3cret-token");
    }

    #[test]
    fn parses_ux_command_line() {
        let cmd = r#""C:\Riot Games\League of Legends\LeagueClientUx.exe" --app-port=1337 --remoting-auth-token=abc_DEF-123 --install-directory=C:\Riot Games\League of Legends"#;
        let info = parse_command_line(cmd).expect("cmdline");
        assert_eq!(info.port, 1337);
        assert_eq!(info.password, "abc_DEF-123");
    }
}
