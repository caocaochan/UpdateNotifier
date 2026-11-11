use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Latest full release (no drafts, no pre-releases).
    Stable,
    /// Any published release, pre-releases included.
    Prerelease,
    /// New commits on a branch.
    Commits,
}

/// The newest thing we have seen for a repo.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Seen {
    /// Tag name or commit SHA.
    pub id: String,
    /// Short display label, e.g. "v1.2.3" or "main @ abc1234".
    pub label: String,
    /// RFC 3339 timestamp (GitHub always uses UTC "Z", so string order == time order).
    pub at: String,
    pub url: String,
}

/// Per-repo check state. `None` on the repo means "never checked yet": the
/// first check only records a baseline and never notifies.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RepoState {
    #[serde(default)]
    pub seen: Option<Seen>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repo {
    /// "owner/name", canonical case from the API.
    pub repo: String,
    pub mode: Mode,
    /// Branch for commit mode. `None` = repository default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default)]
    pub state: Option<RepoState>,
}

impl Repo {
    pub fn branch_label(&self) -> &str {
        self.branch.as_deref().unwrap_or("default branch")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CommitRange {
    pub branch: String,
    pub base: String,
    pub head: String,
    /// `None` when the compare call failed (e.g. after a force-push).
    pub count: Option<u32>,
    pub force_pushed: bool,
    pub head_url: String,
}

/// An update the user has not looked at yet.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Update {
    pub repo: String,
    pub title: String,
    pub body: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commits: Option<CommitRange>,
}

fn default_interval() -> u32 {
    30
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_interval")]
    pub interval_minutes: u32,
    #[serde(default)]
    pub repos: Vec<Repo>,
    #[serde(default)]
    pub pending: Vec<Update>,
}

impl Default for Config {
    fn default() -> Self {
        Self { interval_minutes: default_interval(), repos: Vec::new(), pending: Vec::new() }
    }
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("UpdateNotifier")
}

pub fn path() -> PathBuf {
    dir().join("config.json")
}

/// Loads the config. A corrupt file is moved aside (never overwritten) and a
/// warning is returned for the UI to show.
pub fn load() -> (Config, Option<String>) {
    let p = path();
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Config::default(), None),
        Err(e) => return (Config::default(), Some(format!("Could not read {}: {e}", p.display()))),
    };
    match serde_json::from_str(&text) {
        Ok(c) => (c, None),
        Err(e) => {
            let bad = p.with_extension("json.bad");
            let _ = std::fs::rename(&p, &bad);
            (Config::default(), Some(format!("{} was invalid ({e}).\nIt was moved to {}.", p.display(), bad.display())))
        }
    }
}

impl Config {
    /// Atomic write: temp file + rename (MoveFileEx with REPLACE_EXISTING on Windows).
    pub fn save(&self) -> std::io::Result<()> {
        let p = path();
        std::fs::create_dir_all(dir())?;
        let tmp = p.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &p)
    }

    pub fn find(&self, repo: &str) -> Option<usize> {
        self.repos.iter().position(|r| r.repo.eq_ignore_ascii_case(repo))
    }
}
