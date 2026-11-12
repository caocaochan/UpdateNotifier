use crate::config::{CommitRange, Mode, Repo, RepoState, Seen, Update};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
}

#[derive(Debug)]
pub enum ApiError {
    RateLimited(SystemTime),
    NotFound,
    Unauthorized,
    Http(u16, String),
    Net(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::RateLimited(_) => write!(f, "GitHub rate limit reached"),
            ApiError::NotFound => write!(f, "not found"),
            ApiError::Unauthorized => write!(f, "GitHub rejected the token (401)"),
            ApiError::Http(s, m) if m.is_empty() => write!(f, "HTTP {s}"),
            ApiError::Http(s, m) => write!(f, "HTTP {s}: {m}"),
            ApiError::Net(m) => write!(f, "{m}"),
        }
    }
}

enum Got<T> {
    NotModified,
    Fresh(T, Option<String>),
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    name: Option<String>,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    published_at: Option<String>,
    created_at: String,
}

#[derive(Deserialize)]
struct CommitItem {
    sha: String,
    html_url: String,
    commit: CommitInner,
}

#[derive(Deserialize)]
struct CommitInner {
    message: String,
    committer: Option<GitActor>,
    author: Option<GitActor>,
}

#[derive(Deserialize)]
struct GitActor {
    date: Option<String>,
}

#[derive(Deserialize)]
pub struct RepoInfo {
    pub full_name: String,
    pub default_branch: String,
}

#[derive(Deserialize)]
struct Compare {
    status: String,
    ahead_by: u32,
}

/// Result of checking one repo.
pub struct Checked {
    pub state: RepoState,
    pub update: Option<Update>,
}

impl Client {
    pub fn new() -> Self {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .user_agent(concat!("update-notifier/", env!("CARGO_PKG_VERSION")))
            .tls_config(tls)
            .build()
            .into();
        Self { agent }
    }

    fn get<T: DeserializeOwned>(
        &self,
        token: Option<&str>,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Got<T>, ApiError> {
        let url = format!("https://api.github.com{path}");
        let mut req = self
            .agent
            .get(&url)
            .header("Accept", "application/vnd.github+json");
        if let Some(t) = token {
            req = req.header("Authorization", &format!("Bearer {t}"));
        }
        if let Some(e) = etag {
            req = req.header("If-None-Match", e);
        }
        let mut resp = req.call().map_err(|e| ApiError::Net(e.to_string()))?;
        let status = resp.status().as_u16();
        let header = |n: &str| {
            resp.headers()
                .get(n)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let etag = header("etag");
        let remaining = header("x-ratelimit-remaining");
        let reset = header("x-ratelimit-reset").and_then(|v| v.parse::<u64>().ok());
        let retry_after = header("retry-after").and_then(|v| v.parse::<u64>().ok());

        match status {
            304 => Ok(Got::NotModified),
            200..=299 => {
                let v = resp
                    .body_mut()
                    .read_json::<T>()
                    .map_err(|e| ApiError::Net(format!("bad response: {e}")))?;
                Ok(Got::Fresh(v, etag))
            }
            404 => Err(ApiError::NotFound),
            401 => Err(ApiError::Unauthorized),
            403 | 429 if remaining.as_deref() == Some("0") || retry_after.is_some() => {
                let until = match (retry_after, reset) {
                    (Some(s), _) => SystemTime::now() + Duration::from_secs(s),
                    (None, Some(r)) => UNIX_EPOCH + Duration::from_secs(r),
                    _ => SystemTime::now() + Duration::from_secs(60 * 15),
                };
                Err(ApiError::RateLimited(until))
            }
            _ => {
                let msg = resp
                    .body_mut()
                    .read_json::<serde_json::Value>()
                    .ok()
                    .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_owned))
                    .unwrap_or_default();
                Err(ApiError::Http(status, msg))
            }
        }
    }

    fn get_fresh<T: DeserializeOwned>(
        &self,
        token: Option<&str>,
        path: &str,
    ) -> Result<T, ApiError> {
        match self.get(token, path, None)? {
            Got::Fresh(v, _) => Ok(v),
            Got::NotModified => Err(ApiError::Http(304, "unexpected".into())),
        }
    }

    pub fn repo_info(&self, token: Option<&str>, repo: &str) -> Result<RepoInfo, ApiError> {
        self.get_fresh(token, &format!("/repos/{repo}"))
    }

    pub fn branch_exists(
        &self,
        token: Option<&str>,
        repo: &str,
        branch: &str,
    ) -> Result<bool, ApiError> {
        match self.get_fresh::<serde_json::Value>(
            token,
            &format!("/repos/{repo}/branches/{}", encode(branch)),
        ) {
            Ok(_) => Ok(true),
            Err(ApiError::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Branch names, default branch first. Capped at `MAX_BRANCH_PAGES` pages of 100
    /// (each page is one request); the dialog still accepts typed names beyond that.
    pub fn branches(
        &self,
        token: Option<&str>,
        repo: &str,
        default: &str,
    ) -> Result<Vec<String>, ApiError> {
        const MAX_BRANCH_PAGES: u32 = 3;
        #[derive(Deserialize)]
        struct Branch {
            name: String,
        }
        let mut names = vec![default.to_owned()];
        for page in 1..=MAX_BRANCH_PAGES {
            let list: Vec<Branch> = self.get_fresh(
                token,
                &format!("/repos/{repo}/branches?per_page=100&page={page}"),
            )?;
            let n = list.len();
            names.extend(list.into_iter().map(|b| b.name).filter(|b| b != default));
            if n < 100 {
                break;
            }
        }
        Ok(names)
    }

    /// Verifies a token. `/rate_limit` does not count against the rate limit.
    pub fn verify_token(&self, token: &str) -> Result<(), ApiError> {
        self.get_fresh::<serde_json::Value>(Some(token), "/rate_limit")
            .map(|_| ())
    }

    pub fn check(&self, token: Option<&str>, repo: &Repo) -> Result<Checked, ApiError> {
        let prev = repo.state.clone();
        // Only send the ETag when we have a baseline to fall back on.
        let etag = prev
            .as_ref()
            .filter(|s| s.seen.is_some())
            .and_then(|s| s.etag.as_deref());
        let unchanged = || Checked {
            state: prev.clone().unwrap_or_default(),
            update: None,
        };
        let r = &repo.repo;

        match repo.mode {
            Mode::Stable => {
                match self.get::<Release>(token, &format!("/repos/{r}/releases/latest"), etag) {
                    Ok(Got::NotModified) => Ok(unchanged()),
                    Ok(Got::Fresh(rel, etag)) => {
                        Ok(release_result(repo, prev.as_ref(), Some(rel), etag))
                    }
                    Err(ApiError::NotFound) => {
                        // 404 means either "no releases" or "no such repo".
                        self.repo_info(token, r)
                            .map_err(not_found_as("repository not found"))?;
                        Ok(release_result(repo, prev.as_ref(), None, None))
                    }
                    Err(e) => Err(e),
                }
            }
            Mode::Prerelease => {
                match self
                    .get::<Vec<Release>>(token, &format!("/repos/{r}/releases?per_page=20"), etag)
                    .map_err(not_found_as("repository not found"))?
                {
                    Got::NotModified => Ok(unchanged()),
                    Got::Fresh(list, etag) => {
                        // List order is not documented; pick the newest by publish time.
                        let newest = list
                            .into_iter()
                            .filter(|x| !x.draft)
                            .max_by(|a, b| release_time(a).cmp(release_time(b)));
                        Ok(release_result(repo, prev.as_ref(), newest, etag))
                    }
                }
            }
            Mode::Commits => {
                let path = match &repo.branch {
                    Some(b) => format!("/repos/{r}/commits?sha={}&per_page=1", encode(b)),
                    None => format!("/repos/{r}/commits?per_page=1"),
                };
                match self
                    .get::<Vec<CommitItem>>(token, &path, etag)
                    .map_err(not_found_as("repository or branch not found"))?
                {
                    Got::NotModified => Ok(unchanged()),
                    Got::Fresh(list, etag) => Ok(self.commit_result(
                        token,
                        repo,
                        prev.as_ref(),
                        list.into_iter().next(),
                        etag,
                    )),
                }
            }
        }
    }

    fn commit_result(
        &self,
        token: Option<&str>,
        repo: &Repo,
        prev: Option<&RepoState>,
        head: Option<CommitItem>,
        etag: Option<String>,
    ) -> Checked {
        let branch = repo.branch_label().to_owned();
        let seen = head.as_ref().map(|c| Seen {
            id: c.sha.clone(),
            label: format!("{branch} @ {}", short_sha(&c.sha)),
            at: c
                .commit
                .committer
                .as_ref()
                .or(c.commit.author.as_ref())
                .and_then(|a| a.date.clone())
                .unwrap_or_default(),
            url: c.html_url.clone(),
        });

        let mut update = None;
        if let (Some(prev), Some(head), Some(new)) = (prev, head.as_ref(), seen.as_ref()) {
            let old = prev.seen.as_ref().map(|s| s.id.clone());
            if old.as_deref() != Some(new.id.as_str()) {
                let (count, force_pushed, base) = match &old {
                    Some(old) => match self.get_fresh::<Compare>(
                        token,
                        &format!("/repos/{}/compare/{old}...{}", repo.repo, new.id),
                    ) {
                        Ok(c) => (Some(c.ahead_by), c.status != "ahead", old.clone()),
                        Err(_) => (None, true, old.clone()),
                    },
                    None => (None, false, String::new()),
                };
                let range = CommitRange {
                    branch,
                    base,
                    head: new.id.clone(),
                    count,
                    force_pushed,
                    head_url: new.url.clone(),
                };
                let msg = head.commit.message.lines().next().unwrap_or("").to_owned();
                update = Some(commit_update(&repo.repo, range, msg));
            }
        }
        Checked {
            state: RepoState { seen, etag },
            update,
        }
    }
}

/// Builds (or rebuilds, when merging two pending updates) a commit notification.
pub fn commit_update(repo: &str, range: CommitRange, latest_message: String) -> Update {
    let short = repo_name(repo);
    let title = match (range.force_pushed, range.count) {
        (true, _) => format!("{short}: {} was force-pushed", range.branch),
        (false, Some(1)) => format!("{short}: 1 new commit on {}", range.branch),
        (false, Some(n)) => format!("{short}: {n} new commits on {}", range.branch),
        (false, None) => format!("{short}: new commits on {}", range.branch),
    };
    let url = if range.count == Some(1) || range.base.is_empty() || range.force_pushed {
        range.head_url.clone()
    } else {
        format!(
            "https://github.com/{repo}/compare/{}...{}",
            short_sha(&range.base),
            short_sha(&range.head)
        )
    };
    Update {
        repo: repo.to_owned(),
        title,
        body: latest_message,
        url,
        commits: Some(range),
    }
}

fn release_time(r: &Release) -> &str {
    r.published_at.as_deref().unwrap_or(&r.created_at)
}

fn release_result(
    repo: &Repo,
    prev: Option<&RepoState>,
    rel: Option<Release>,
    etag: Option<String>,
) -> Checked {
    let seen = rel.as_ref().map(|r| Seen {
        id: r.tag_name.clone(),
        label: r.tag_name.clone(),
        at: release_time(r).to_owned(),
        url: r.html_url.clone(),
    });
    let mut update = None;
    if let (Some(prev), Some(rel), Some(new)) = (prev, rel.as_ref(), seen.as_ref()) {
        let newer = match &prev.seen {
            None => true, // repo had no releases before: the first one is news
            Some(old) => old.id != new.id && new.at > old.at,
        };
        if newer {
            let mut body = Vec::new();
            if rel.prerelease {
                body.push("Pre-release".to_owned());
            }
            if let Some(n) = rel
                .name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty() && *n != rel.tag_name)
            {
                body.push(n.to_owned());
            }
            update = Some(Update {
                repo: repo.repo.clone(),
                title: format!("{} {}", repo_name(&repo.repo), rel.tag_name),
                body: if body.is_empty() {
                    "New release".into()
                } else {
                    body.join(" · ")
                },
                url: rel.html_url.clone(),
                commits: None,
            });
        }
    }
    Checked {
        state: RepoState { seen, etag },
        update,
    }
}

fn not_found_as(msg: &'static str) -> impl Fn(ApiError) -> ApiError {
    move |e| match e {
        ApiError::NotFound => ApiError::Http(404, msg.into()),
        e => e,
    }
}

/// "owner/name" -> "name" (owner is noise in a 63-char toast title).
pub fn repo_name(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

pub fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Accepts "owner/name", "github.com/owner/name", full URLs, ".git" suffixes.
pub fn parse_repo(input: &str) -> Option<String> {
    let s = input.trim().trim_end_matches('/');
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let s = s.strip_prefix("www.").unwrap_or(s);
    let s = s.strip_prefix("github.com/").unwrap_or(s);
    let mut parts = s.split('/');
    let owner = parts.next()?;
    let name = parts.next()?.trim_end_matches(".git");
    let ok = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    (ok(owner) && ok(name)).then(|| format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repo_forms() {
        assert_eq!(
            parse_repo("rust-lang/rust").as_deref(),
            Some("rust-lang/rust")
        );
        assert_eq!(
            parse_repo(" https://github.com/neovim/neovim/releases ").as_deref(),
            Some("neovim/neovim")
        );
        assert_eq!(parse_repo("github.com/a/b.git").as_deref(), Some("a/b"));
        assert_eq!(parse_repo("nope"), None);
        assert_eq!(parse_repo("a b/c"), None);
    }

    #[test]
    fn encodes_branch() {
        assert_eq!(encode("feature/x y"), "feature%2Fx%20y");
    }

    // ---- live API tests: `cargo test -- --ignored --nocapture` (uses GITHUB_TOKEN if set) ----

    fn token() -> Option<String> {
        std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty())
    }

    fn repo(name: &str, mode: Mode, branch: Option<&str>, state: Option<RepoState>) -> Repo {
        Repo {
            repo: name.into(),
            mode,
            branch: branch.map(Into::into),
            state,
        }
    }

    fn old_seen(id: &str) -> Option<RepoState> {
        Some(RepoState {
            seen: Some(Seen {
                id: id.into(),
                label: id.into(),
                at: "2000-01-01T00:00:00Z".into(),
                url: String::new(),
            }),
            etag: None,
        })
    }

    #[test]
    #[ignore]
    fn live_stable() {
        let (c, t) = (Client::new(), token());
        let first = c
            .check(
                t.as_deref(),
                &repo("BurntSushi/ripgrep", Mode::Stable, None, None),
            )
            .unwrap();
        let seen = first.state.seen.clone().expect("ripgrep has releases");
        println!("baseline: {seen:?} etag={:?}", first.state.etag);
        assert!(first.update.is_none(), "first check must not notify");

        let again = c
            .check(
                t.as_deref(),
                &repo("BurntSushi/ripgrep", Mode::Stable, None, Some(first.state)),
            )
            .unwrap();
        assert!(again.update.is_none());
        assert_eq!(again.state.seen.as_ref(), Some(&seen));

        let newer = c
            .check(
                t.as_deref(),
                &repo("BurntSushi/ripgrep", Mode::Stable, None, old_seen("0.0.1")),
            )
            .unwrap();
        let u = newer.update.expect("older baseline must produce an update");
        println!("update: {} | {} | {}", u.title, u.body, u.url);
    }

    #[test]
    #[ignore]
    fn live_prerelease() {
        let (c, t) = (Client::new(), token());
        let r = c
            .check(
                t.as_deref(),
                &repo("neovim/neovim", Mode::Prerelease, None, old_seen("v0.0.0")),
            )
            .unwrap();
        let u = r.update.expect("update");
        println!(
            "seen: {:?}\nupdate: {} | {} | {}",
            r.state.seen, u.title, u.body, u.url
        );
    }

    #[test]
    #[ignore]
    fn live_no_releases() {
        let (c, t) = (Client::new(), token());
        let r = c
            .check(
                t.as_deref(),
                &repo("torvalds/linux", Mode::Stable, None, None),
            )
            .unwrap();
        assert!(r.state.seen.is_none());
        let e = c
            .check(
                t.as_deref(),
                &repo("nonexistent-owner-xyz/nope", Mode::Stable, None, None),
            )
            .err()
            .unwrap();
        println!("missing repo error: {e}");
    }

    #[test]
    #[ignore]
    fn live_commits() {
        let (c, t) = (Client::new(), token());
        let list: Vec<CommitItem> = c
            .get_fresh(t.as_deref(), "/repos/BurntSushi/ripgrep/commits?per_page=3")
            .unwrap();
        let base = &list[2].sha;
        let r = c
            .check(
                t.as_deref(),
                &repo(
                    "BurntSushi/ripgrep",
                    Mode::Commits,
                    Some("master"),
                    old_seen(base),
                ),
            )
            .unwrap();
        let u = r.update.expect("update");
        println!(
            "seen: {:?}\nupdate: {} | {} | {}",
            r.state.seen, u.title, u.body, u.url
        );
        assert_eq!(u.commits.as_ref().unwrap().count, Some(2));

        let e = c
            .check(
                t.as_deref(),
                &repo(
                    "BurntSushi/ripgrep",
                    Mode::Commits,
                    Some("no-such-branch-xyz"),
                    None,
                ),
            )
            .err()
            .unwrap();
        println!("missing branch error: {e}");

        assert!(
            c.branch_exists(t.as_deref(), "microsoft/vscode", "release/1.95")
                .unwrap()
        );
        assert!(
            !c.branch_exists(t.as_deref(), "microsoft/vscode", "release/0.0")
                .unwrap()
        );
        let info = c.repo_info(t.as_deref(), "burntsushi/RIPGREP").unwrap();
        println!(
            "canonical: {} default: {}",
            info.full_name, info.default_branch
        );
    }
}
