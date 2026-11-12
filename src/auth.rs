//! Where the GitHub token comes from, in order:
//!   1. a token saved in the app (Windows Credential Manager)
//!   2. GITHUB_TOKEN / GH_TOKEN environment variables
//!   3. the GitHub CLI login (`gh auth token`)
//!   4. Git's credential helpers (`git credential fill`, e.g. Git Credential Manager)
//!
//! External lookups never prompt: they fail instead, with a 10 s timeout.

use crate::win;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Saved,
    Env,
    Gh,
    Git,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Saved => "saved",
            Source::Env => "from GITHUB_TOKEN",
            Source::Gh => "from gh",
            Source::Git => "from git",
        }
    }
}

/// Saved token or environment variable only: no child processes, safe on the UI thread.
pub fn resolve_fast() -> Option<(String, Source)> {
    if let Some(t) = win::token_read() {
        return Some((t, Source::Saved));
    }
    ["GITHUB_TOKEN", "GH_TOKEN"]
        .iter()
        .find_map(|v| {
            std::env::var(v)
                .ok()
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
        })
        .map(|t| (t, Source::Env))
}

/// Full lookup including gh and git. Spawns processes: call from the worker thread.
pub fn resolve() -> Option<(String, Source)> {
    resolve_fast()
        .or_else(|| from_gh().map(|t| (t, Source::Gh)))
        .or_else(|| from_git().map(|t| (t, Source::Git)))
}

fn from_gh() -> Option<String> {
    let out = run(
        Command::new("gh").args(["auth", "token", "--hostname", "github.com"]),
        None,
    )?;
    Some(out.trim().to_owned()).filter(|t| !t.is_empty())
}

fn from_git() -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "credential.interactive=false", "credential", "fill"])
        // Never prompt. An *empty* GIT_ASKPASS disables askpass outright (it does not
        // fall through to core.askPass / SSH_ASKPASS; see git's prompt.c), and
        // GIT_TERMINAL_PROMPT=0 disables the terminal fallback.
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
    let out = run(&mut cmd, Some("protocol=https\nhost=github.com\n\n"))?;
    out.lines()
        .find_map(|l| l.strip_prefix("password="))
        .map(str::to_owned)
        .filter(|t| !t.is_empty())
}

/// Runs a console tool without a window; returns stdout if it exits 0 within the timeout.
fn run(cmd: &mut Command, stdin: Option<&str>) -> Option<String> {
    cmd.creation_flags(CREATE_NO_WINDOW)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?; // not installed -> None
    if let Some(input) = stdin {
        // Dropping the handle closes the pipe so the tool sees EOF.
        child.stdin.take()?.write_all(input.as_bytes()).ok()?;
    }
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Prints only the source and length, never the token.
    #[test]
    #[ignore]
    fn live_lookups() {
        for (name, t) in [("gh", from_gh()), ("git", from_git())] {
            println!("{name}: {:?}", t.as_ref().map(|t| (t.len(), &t[..4])));
        }
        println!("resolve: {:?}", resolve().map(|(t, s)| (s, t.len())));
    }
}
