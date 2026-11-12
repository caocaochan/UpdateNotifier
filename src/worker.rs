use crate::auth;
use crate::config::{Config, Update};
use crate::github::{self, ApiError, Client};
use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

/// State shared between the UI thread and the checker thread.
pub struct Shared {
    pub config: Config,
    pub token: Option<String>,
    pub token_source: Option<auth::Source>,
    /// Lower-cased repo -> last error message.
    pub errors: HashMap<String, String>,
    pub last_check: Option<SystemTime>,
    pub checking: bool,
    pub rate_limited_until: Option<SystemTime>,
    /// Updates found by the last check that have not been shown as a toast yet.
    pub fresh: Vec<Update>,
    pub save_error: Option<String>,
}

impl Shared {
    pub fn new(config: Config, token: Option<(String, auth::Source)>) -> Self {
        let (token, token_source) = token.map_or((None, None), |(t, s)| (Some(t), Some(s)));
        Self {
            config,
            token,
            token_source,
            errors: HashMap::new(),
            last_check: None,
            checking: false,
            rate_limited_until: None,
            fresh: Vec::new(),
            save_error: None,
        }
    }

    pub fn save(&mut self) {
        self.save_error = self
            .config
            .save()
            .err()
            .map(|e| format!("Could not save config: {e}"));
    }

    /// Adds an update to the unseen list, merging with an existing entry for the same repo.
    pub fn add_pending(&mut self, u: Update) {
        let pending = &mut self.config.pending;
        if let Some(i) = pending
            .iter()
            .position(|p| p.repo.eq_ignore_ascii_case(&u.repo))
        {
            let old = pending.remove(i);
            // Two batches of commits on the same branch: one entry covering both.
            if let (Some(a), Some(b)) = (&old.commits, &u.commits)
                && a.branch == b.branch
                && !a.base.is_empty()
                && !a.force_pushed
                && !b.force_pushed
            {
                let mut range = b.clone();
                range.base = a.base.clone();
                range.count = a.count.zip(b.count).map(|(x, y)| x + y);
                pending.push(github::commit_update(&u.repo, range, u.body.clone()));
                return;
            }
        }
        pending.push(u);
    }
}

pub fn lock(s: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

pub enum Cmd {
    CheckNow,
    /// Check after a delay unless a check is already due sooner (used after resume).
    CheckIn(Duration),
    Quit,
}

/// Spawns the checker thread. `notify` is called (from the worker thread) after each check.
pub fn spawn(
    shared: Arc<Mutex<Shared>>,
    client: Client,
    notify: impl Fn() + Send + 'static,
) -> Sender<Cmd> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut last_run = SystemTime::now();
        // Give the network a moment after logon before the first check.
        let mut forced = Some(SystemTime::now() + Duration::from_secs(5));
        loop {
            let interval =
                Duration::from_secs(u64::from(lock(&shared).config.interval_minutes.max(5)) * 60);
            let mut due = last_run + interval;
            if let Some(f) = forced {
                due = due.min(f);
            }
            let now = SystemTime::now();
            // Wake at least once a minute: wall-clock based, so sleep/hibernate is handled.
            let wait = due
                .duration_since(now)
                .unwrap_or(Duration::ZERO)
                .min(Duration::from_secs(60));
            let run = match rx.recv_timeout(wait) {
                Ok(Cmd::CheckNow) => true,
                Ok(Cmd::CheckIn(d)) => {
                    let t = now + d;
                    forced = Some(forced.map_or(t, |f| f.min(t)));
                    false
                }
                Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => SystemTime::now() >= due,
            };
            if run {
                forced = run_cycle(&shared, &client);
                last_run = SystemTime::now();
                notify();
            }
        }
    });
    tx
}

/// Checks every repo once. Returns a forced next-check time when rate limited.
fn run_cycle(shared: &Mutex<Shared>, client: &Client) -> Option<SystemTime> {
    // Re-resolved every cycle (outside the lock: may run gh/git), so logging in to gh
    // or saving/removing a token takes effect without a restart.
    let resolved = auth::resolve();
    let (repos, token) = {
        let mut s = lock(shared);
        (s.token, s.token_source) = resolved.map_or((None, None), |(t, src)| (Some(t), Some(src)));
        if let Some(until) = s.rate_limited_until {
            if until > SystemTime::now() {
                return Some(until + Duration::from_secs(5));
            }
            s.rate_limited_until = None;
        }
        s.checking = true;
        (s.config.repos.clone(), s.token.clone())
    };

    let mut results = Vec::with_capacity(repos.len());
    let mut limited = None;
    for r in &repos {
        match client.check(token.as_deref(), r) {
            Err(ApiError::RateLimited(t)) => {
                limited = Some(t);
                break;
            }
            res => results.push((r, res)),
        }
    }

    let mut guard = lock(shared);
    let s = &mut *guard;
    for (spec, res) in results {
        // The user may have edited or removed the repo while we were checking.
        let Some(cur) = s.config.repos.iter_mut().find(|c| {
            c.repo.eq_ignore_ascii_case(&spec.repo)
                && c.mode == spec.mode
                && c.branch == spec.branch
        }) else {
            continue;
        };
        let key = spec.repo.to_ascii_lowercase();
        match res {
            Ok(checked) => {
                cur.state = Some(checked.state);
                s.errors.remove(&key);
                if let Some(u) = checked.update {
                    s.fresh.push(u.clone());
                    s.add_pending(u);
                }
            }
            Err(e) => {
                s.errors.insert(key, e.to_string());
            }
        }
    }
    s.rate_limited_until = limited;
    s.last_check = Some(SystemTime::now());
    s.checking = false;
    s.save();
    limited.map(|t| t + Duration::from_secs(5))
}
