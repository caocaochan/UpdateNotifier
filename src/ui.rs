//! Tray icon, context menu, notifications. Everything here runs on the UI thread.

use crate::config::{self, Mode, Repo};
use crate::dialog::{self, RepoForm, TokenAction};
use crate::github::{self, ApiError, Client};
use crate::icon;
use crate::win::{self, APP_NAME, wide};
use crate::worker::{self, Cmd, Shared, lock};
use std::cell::{Cell, RefCell};
use std::ptr::{null, null_mut};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 1;
const WM_CHECKED: u32 = WM_APP + 2;
const TRAY_UID: u32 = 1;
const NIN_KEYSELECT: u32 = NIN_SELECT + 1; // not exported by windows-sys (shellapi.h: NINF_KEY | NIN_SELECT)

const ID_CHECK: u32 = 1;
const ID_ADD: u32 = 2;
const ID_TOKEN: u32 = 3;
const ID_STARTUP: u32 = 4;
const ID_EXIT: u32 = 5;
const ID_OPEN_ALL: u32 = 6;
const ID_CLEAR: u32 = 7;
const ID_CONFIG: u32 = 8;
const ID_INTERVAL: u32 = 0x100;
const ID_PENDING: u32 = 0x200;
const ID_REPO: u32 = 0x1000; // + index * 16 + action

const A_STABLE: u32 = 0;
const A_PRE: u32 = 1;
const A_COMMITS: u32 = 2;
const A_EDIT: u32 = 3;
const A_OPEN: u32 = 4;
const A_REMOVE: u32 = 5;
const A_LATEST: u32 = 6;

const INTERVALS: [(u32, &str); 6] =
    [(15, "15 minutes"), (30, "30 minutes"), (60, "1 hour"), (180, "3 hours"), (360, "6 hours"), (720, "12 hours")];

enum Balloon {
    None,
    Open { repo: String, url: String },
    Menu,
}

struct App {
    hwnd: HWND,
    shared: Arc<Mutex<Shared>>,
    client: Client,
    tx: Sender<Cmd>,
    icon: HICON,
    icon_badge: HICON,
    icon_large: HICON,
    taskbar_created: u32,
    balloon: RefCell<Balloon>,
    in_menu: Cell<bool>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

fn app() -> Option<Rc<App>> {
    APP.with(|a| a.borrow().clone())
}

pub fn run(shared: Arc<Mutex<Shared>>, warning: Option<String>) {
    unsafe {
        let hinst = GetModuleHandleW(null());
        let class = wide("UpdateNotifierTray");
        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.lpszClassName = class.as_ptr();
        RegisterClassExW(&wc);

        crate::theme::apply();
        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        // A hidden top-level window (not message-only: those miss the TaskbarCreated broadcast).
        let title = wide(APP_NAME);
        let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), WS_OVERLAPPED, 0, 0, 0, 0, null_mut(), null_mut(), hinst, null());

        let client = Client::new();
        let hwnd_val = hwnd as isize;
        let tx = worker::spawn(shared.clone(), client.clone(), move || {
            PostMessageW(hwnd_val as HWND, WM_CHECKED, 0, 0);
        });

        let app = Rc::new(App {
            hwnd,
            shared,
            client,
            tx,
            icon: icon::make(GetSystemMetrics(SM_CXSMICON), false),
            icon_badge: icon::make(GetSystemMetrics(SM_CXSMICON), true),
            icon_large: icon::make(GetSystemMetrics(SM_CXICON), false),
            taskbar_created,
            balloon: RefCell::new(Balloon::None),
            in_menu: Cell::new(false),
        });
        dialog::set_icon(app.icon_large);
        APP.with(|a| *a.borrow_mut() = Some(app.clone()));
        app.add_tray();

        if let Some(w) = warning {
            win::message_box(null_mut(), &w, MB_ICONWARNING);
        }
        if lock(&app.shared).config.repos.is_empty() {
            app.balloon("Update Notifier is running", "Click the tray icon to add a repository.", Balloon::Menu);
        }
        drop(app);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let Some(app) = app() else {
            return DefWindowProcW(hwnd, msg, wp, lp);
        };
        match msg {
            WM_TRAY => {
                // NOTIFYICON_VERSION_4: event in LOWORD(lParam), anchor point in wParam.
                let x = (wp & 0xFFFF) as i16 as i32;
                let y = ((wp >> 16) & 0xFFFF) as i16 as i32;
                match (lp & 0xFFFF) as u32 {
                    WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT => app.show_menu(x, y),
                    NIN_BALLOONUSERCLICK => app.balloon_clicked(),
                    _ => {}
                }
                0
            }
            WM_CHECKED => {
                app.on_checked();
                0
            }
            WM_POWERBROADCAST => {
                if wp as u32 == PBT_APMRESUMEAUTOMATIC {
                    let _ = app.tx.send(Cmd::CheckIn(Duration::from_secs(30)));
                }
                1
            }
            WM_SETTINGCHANGE => {
                // Light/dark switch in Settings broadcasts lParam = "ImmersiveColorSet".
                if lp != 0 {
                    let p = lp as *const u16;
                    let len = (0..64).take_while(|&i| *p.add(i) != 0).count();
                    if String::from_utf16_lossy(std::slice::from_raw_parts(p, len)) == "ImmersiveColorSet" {
                        crate::theme::apply();
                    }
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            m if m == app.taskbar_created && m != 0 => {
                app.add_tray(); // Explorer restarted
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe fn add(menu: HMENU, flags: u32, id: u32, text: &str) {
    let t = wide(text);
    unsafe { AppendMenuW(menu, flags, id as usize, t.as_ptr()) };
}

unsafe fn sep(menu: HMENU) {
    unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, null()) };
}

/// Escape '&' so it is not taken as a mnemonic marker.
fn esc(s: &str) -> String {
    s.replace('&', "&&")
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl App {
    fn nid(&self) -> NOTIFYICONDATAW {
        let mut n: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        n.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        n.hWnd = self.hwnd;
        n.uID = TRAY_UID;
        n
    }

    fn fill_icon_tip(&self, n: &mut NOTIFYICONDATAW) {
        let s = lock(&self.shared);
        let pending = s.config.pending.len();
        n.hIcon = if pending > 0 { self.icon_badge } else { self.icon };
        let tip = if pending > 0 {
            format!("{APP_NAME}\n{}", plural(pending, "new update", "new updates"))
        } else {
            format!("{APP_NAME}\nWatching {}", plural(s.config.repos.len(), "repository", "repositories"))
        };
        win::copy_to(&mut n.szTip, &tip);
    }

    fn add_tray(&self) {
        let mut n = self.nid();
        n.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        n.uCallbackMessage = WM_TRAY;
        self.fill_icon_tip(&mut n);
        unsafe {
            Shell_NotifyIconW(NIM_ADD, &n);
            n.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            Shell_NotifyIconW(NIM_SETVERSION, &n);
        }
    }

    fn refresh(&self) {
        let mut n = self.nid();
        n.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        self.fill_icon_tip(&mut n);
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &n) };
    }

    /// Shows a notification (Windows turns tray balloons into toasts).
    fn balloon(&self, title: &str, body: &str, action: Balloon) {
        let mut n = self.nid();
        n.uFlags = NIF_INFO;
        win::copy_to(&mut n.szInfoTitle, title);
        win::copy_to(&mut n.szInfo, if body.is_empty() { " " } else { body }); // empty text = no balloon
        n.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON | NIIF_RESPECT_QUIET_TIME;
        n.hBalloonIcon = self.icon_large;
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &n) };
        *self.balloon.borrow_mut() = action;
    }

    fn on_checked(&self) {
        let fresh = std::mem::take(&mut lock(&self.shared).fresh);
        match fresh.as_slice() {
            [] => {}
            [u] => self.balloon(&u.title, &u.body, Balloon::Open { repo: u.repo.clone(), url: u.url.clone() }),
            many => {
                let body = many.iter().map(|u| u.title.as_str()).collect::<Vec<_>>().join("\n");
                self.balloon(&format!("{} new updates", many.len()), &body, Balloon::Menu);
            }
        }
        self.refresh();
    }

    fn balloon_clicked(&self) {
        match self.balloon.replace(Balloon::None) {
            Balloon::Open { repo, url } => {
                win::open_url(&url);
                self.dismiss(|p| p.repo == repo && p.url == url);
            }
            Balloon::Menu => {
                let mut pt = POINT { x: 0, y: 0 };
                unsafe { GetCursorPos(&mut pt) };
                self.show_menu(pt.x, pt.y);
            }
            Balloon::None => {}
        }
    }

    fn dismiss(&self, pred: impl Fn(&config::Update) -> bool) {
        {
            let mut s = lock(&self.shared);
            s.config.pending.retain(|p| !pred(p));
            s.save();
        }
        self.refresh();
    }

    fn show_menu(&self, x: i32, y: i32) {
        if let Some(d) = dialog::active() {
            unsafe { SetForegroundWindow(d) };
            return;
        }
        if self.in_menu.replace(true) {
            return;
        }
        let (menu, names, pending) = self.build_menu();
        let cmd = unsafe {
            SetForegroundWindow(self.hwnd);
            let align = if GetSystemMetrics(SM_MENUDROPALIGNMENT) != 0 { TPM_RIGHTALIGN } else { TPM_LEFTALIGN };
            let cmd = TrackPopupMenuEx(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | align, x, y, self.hwnd, null());
            PostMessageW(self.hwnd, WM_NULL, 0, 0); // KB135788: menu closes properly
            DestroyMenu(menu); // also destroys submenus
            cmd as u32
        };
        self.in_menu.set(false);
        if cmd != 0 {
            self.handle(cmd, &names, &pending);
        }
    }

    /// Returns the menu plus the repo names and pending (repo, url) pairs that command ids index into.
    fn build_menu(&self) -> (HMENU, Vec<String>, Vec<(String, String)>) {
        let s = lock(&self.shared);
        let now = SystemTime::now();
        unsafe {
            let m = CreatePopupMenu();

            let status = if s.checking {
                "checking…".to_owned()
            } else if let Some(u) = s.rate_limited_until.filter(|u| *u > now) {
                format!("rate limited until {}", win::local_hhmm(u))
            } else if let Some(t) = s.last_check {
                format!("checked {}", win::local_hhmm(t))
            } else {
                "not checked yet".to_owned()
            };
            add(m, MF_STRING | MF_GRAYED, 0, &format!("{APP_NAME} — {status}"));
            let per_hour = s.config.repos.len() as u32 * 60 / s.config.interval_minutes.max(1);
            if s.token.is_none() && per_hour > 45 {
                add(m, MF_STRING | MF_GRAYED, 0, &format!("⚠ ~{per_hour} requests/h, limit without a token is 60"));
            }
            if let Some(e) = &s.save_error {
                add(m, MF_STRING | MF_GRAYED, 0, &format!("⚠ {e}"));
            }

            // Unseen updates.
            let pending: Vec<(String, String)> = s.config.pending.iter().map(|p| (p.repo.clone(), p.url.clone())).collect();
            if !pending.is_empty() {
                sep(m);
                for (i, u) in s.config.pending.iter().enumerate().take(0x100) {
                    add(m, MF_STRING, ID_PENDING + i as u32, &format!("● {}", esc(&u.title)));
                }
                if pending.len() > 1 {
                    add(m, MF_STRING, ID_OPEN_ALL, "Open all");
                }
                add(m, MF_STRING, ID_CLEAR, if pending.len() > 1 { "Dismiss all" } else { "Dismiss" });
            }

            // Repositories.
            sep(m);
            let names: Vec<String> = s.config.repos.iter().map(|r| r.repo.clone()).collect();
            for (i, r) in s.config.repos.iter().enumerate().take(0xFF) {
                let id = |a: u32| ID_REPO + i as u32 * 16 + a;
                let sub = CreatePopupMenu();
                let err = s.errors.get(&r.repo.to_ascii_lowercase());
                if let Some(e) = err {
                    add(sub, MF_STRING | MF_GRAYED, 0, &format!("⚠ {}", esc(e)));
                    sep(sub);
                }
                let seen = r.state.as_ref().and_then(|st| st.seen.as_ref());
                if let Some(seen) = seen {
                    let what = if r.mode == Mode::Commits { "latest commit" } else { seen.label.as_str() };
                    add(sub, MF_STRING, id(A_LATEST), &format!("Open {}", esc(what)));
                    sep(sub);
                }
                add(sub, MF_STRING, id(A_STABLE), "Stable releases");
                add(sub, MF_STRING, id(A_PRE), "Releases incl. pre-releases");
                add(sub, MF_STRING, id(A_COMMITS), &format!("Commits on {}", esc(r.branch_label())));
                let checked = match r.mode {
                    Mode::Stable => A_STABLE,
                    Mode::Prerelease => A_PRE,
                    Mode::Commits => A_COMMITS,
                };
                CheckMenuRadioItem(sub, id(A_STABLE), id(A_COMMITS), id(checked), MF_BYCOMMAND);
                add(sub, MF_STRING, id(A_EDIT), "Edit / change branch…");
                sep(sub);
                add(sub, MF_STRING, id(A_OPEN), "Open on GitHub");
                add(sub, MF_STRING, id(A_REMOVE), "Remove");

                let right = match (err, &r.state, seen) {
                    (Some(_), _, _) => "⚠ error".to_owned(),
                    (_, None, _) => "…".to_owned(),
                    (_, Some(_), Some(seen)) => seen.label.clone(),
                    (_, Some(_), None) if r.mode == Mode::Commits => "no commits".to_owned(),
                    (_, Some(_), None) => "no releases".to_owned(),
                };
                let w = wide(&format!("{}\t{}", esc(&r.repo), esc(&right)));
                AppendMenuW(m, MF_POPUP, sub as usize, w.as_ptr());
            }
            if names.is_empty() {
                add(m, MF_STRING | MF_GRAYED, 0, "No repositories yet");
            }
            add(m, MF_STRING, ID_ADD, "Add repository…");

            // Settings.
            sep(m);
            add(m, MF_STRING | if s.checking { MF_GRAYED } else { 0 }, ID_CHECK, "Check now");
            let iv = CreatePopupMenu();
            for (i, (mins, label)) in INTERVALS.iter().enumerate() {
                add(iv, MF_STRING, ID_INTERVAL + i as u32, label);
                if *mins == s.config.interval_minutes {
                    CheckMenuRadioItem(iv, ID_INTERVAL, ID_INTERVAL + INTERVALS.len() as u32 - 1, ID_INTERVAL + i as u32, MF_BYCOMMAND);
                }
            }
            let w = wide("Check every");
            AppendMenuW(m, MF_POPUP, iv as usize, w.as_ptr());
            add(m, MF_STRING, ID_TOKEN, &format!("GitHub token…\t{}", if s.token.is_some() { "set" } else { "not set" }));
            add(m, MF_STRING | if win::startup_enabled() { MF_CHECKED } else { 0 }, ID_STARTUP, "Start with Windows");
            add(m, MF_STRING, ID_CONFIG, "Open config folder");
            sep(m);
            add(m, MF_STRING, ID_EXIT, "Exit");
            (m, names, pending)
        }
    }

    fn handle(&self, cmd: u32, names: &[String], pending: &[(String, String)]) {
        match cmd {
            ID_EXIT => self.exit(),
            ID_CHECK => {
                let _ = self.tx.send(Cmd::CheckNow);
            }
            ID_ADD => self.add_repo(),
            ID_TOKEN => self.edit_token(),
            ID_STARTUP => {
                if !win::set_startup(!win::startup_enabled()) {
                    win::message_box(null_mut(), "Could not change the startup setting.", MB_ICONWARNING);
                }
            }
            ID_CONFIG => {
                let _ = std::fs::create_dir_all(config::dir());
                win::open_url(&config::dir().to_string_lossy());
            }
            ID_OPEN_ALL => {
                for (_, url) in pending {
                    win::open_url(url);
                }
                self.dismiss(|p| pending.iter().any(|(r, u)| *r == p.repo && *u == p.url));
            }
            ID_CLEAR => self.dismiss(|p| pending.iter().any(|(r, u)| *r == p.repo && *u == p.url)),
            c if (ID_INTERVAL..ID_INTERVAL + INTERVALS.len() as u32).contains(&c) => {
                let mut s = lock(&self.shared);
                s.config.interval_minutes = INTERVALS[(c - ID_INTERVAL) as usize].0;
                s.save();
            }
            c if (ID_PENDING..ID_PENDING + 0x100).contains(&c) => {
                if let Some((repo, url)) = pending.get((c - ID_PENDING) as usize) {
                    win::open_url(url);
                    self.dismiss(|p| p.repo == *repo && p.url == *url);
                }
            }
            c if c >= ID_REPO => {
                let (i, a) = ((c - ID_REPO) / 16, (c - ID_REPO) % 16);
                if let Some(name) = names.get(i as usize) {
                    self.repo_action(name, a);
                }
            }
            _ => {}
        }
    }

    fn repo_action(&self, name: &str, action: u32) {
        let mut s = lock(&self.shared);
        let Some(idx) = s.config.find(name) else { return };
        match action {
            A_STABLE | A_PRE | A_COMMITS => {
                let mode = [Mode::Stable, Mode::Prerelease, Mode::Commits][action as usize];
                let r = &mut s.config.repos[idx];
                if r.mode != mode {
                    r.mode = mode;
                    r.state = None; // new baseline: switching modes never notifies by itself
                    s.errors.remove(&name.to_ascii_lowercase());
                    s.save();
                    drop(s);
                    let _ = self.tx.send(Cmd::CheckNow);
                }
            }
            A_LATEST => {
                if let Some(seen) = s.config.repos[idx].state.as_ref().and_then(|st| st.seen.as_ref()) {
                    win::open_url(&seen.url);
                }
            }
            A_OPEN => {
                let r = &s.config.repos[idx];
                let url = match (r.mode, &r.branch) {
                    (Mode::Commits, Some(b)) => format!("https://github.com/{}/commits/{b}", r.repo),
                    (Mode::Commits, None) => format!("https://github.com/{}/commits", r.repo),
                    _ => format!("https://github.com/{}/releases", r.repo),
                };
                win::open_url(&url);
            }
            A_EDIT => {
                drop(s);
                self.edit_repo(name);
            }
            A_REMOVE => {
                drop(s);
                let ok = win::message_box(null_mut(), &format!("Stop watching {name}?"), MB_YESNO | MB_ICONQUESTION) == IDYES;
                if ok {
                    let mut s = lock(&self.shared);
                    if let Some(i) = s.config.find(name) {
                        s.config.repos.remove(i);
                    }
                    s.config.pending.retain(|p| !p.repo.eq_ignore_ascii_case(name));
                    s.errors.remove(&name.to_ascii_lowercase());
                    s.save();
                    drop(s);
                    self.refresh();
                }
            }
            _ => {}
        }
    }

    fn validator(&self, adding: bool) -> impl FnMut(&RepoForm) -> Result<RepoForm, String> + 'static {
        let (shared, client) = (self.shared.clone(), self.client.clone());
        move |f| validate_repo(&shared, &client, f, adding)
    }

    fn branch_fetcher(&self) -> dialog::BranchFetcher {
        let (shared, client) = (self.shared.clone(), self.client.clone());
        Arc::new(move |name: &str| {
            let token = lock(&shared).token.clone();
            let token = token.as_deref();
            let info = client.repo_info(token, name).map_err(|e| match e {
                ApiError::NotFound => "repository not found".to_owned(),
                e => e.to_string(),
            })?;
            let list = client.branches(token, &info.full_name, &info.default_branch).map_err(|e| e.to_string())?;
            Ok((info.default_branch, list))
        })
    }

    fn add_repo(&self) {
        let Some(f) = dialog::repo_dialog(None, self.branch_fetcher(), self.validator(true)) else { return };
        {
            let mut s = lock(&self.shared);
            if s.config.find(&f.repo).is_some() {
                return;
            }
            s.config.repos.push(Repo { repo: f.repo, mode: f.mode, branch: f.branch, state: None });
            s.save();
        }
        self.refresh();
        let _ = self.tx.send(Cmd::CheckNow);
    }

    fn edit_repo(&self, name: &str) {
        let initial = {
            let s = lock(&self.shared);
            let Some(i) = s.config.find(name) else { return };
            let r = &s.config.repos[i];
            RepoForm { repo: r.repo.clone(), mode: r.mode, branch: r.branch.clone() }
        };
        let Some(f) = dialog::repo_dialog(Some(&initial), self.branch_fetcher(), self.validator(false)) else { return };
        let mut s = lock(&self.shared);
        let Some(i) = s.config.find(name) else { return };
        let r = &mut s.config.repos[i];
        let branch_matters = f.mode == Mode::Commits || r.mode == Mode::Commits;
        if r.mode != f.mode || (branch_matters && r.branch != f.branch) {
            r.state = None;
            s.errors.remove(&name.to_ascii_lowercase());
        }
        let r = &mut s.config.repos[i];
        r.mode = f.mode;
        r.branch = f.branch;
        let recheck = r.state.is_none();
        s.save();
        drop(s);
        if recheck {
            let _ = self.tx.send(Cmd::CheckNow);
        }
    }

    fn edit_token(&self) {
        let has = lock(&self.shared).token.is_some();
        let client = self.client.clone();
        let res = dialog::token_dialog(has, move |t| {
            client.verify_token(t).map_err(|e| match e {
                ApiError::Unauthorized => "GitHub rejected this token.".to_owned(),
                e => format!("Could not verify the token: {e}"),
            })
        });
        match res {
            Some(TokenAction::Set(t)) => {
                if !win::token_write(&t) {
                    win::message_box(null_mut(), "Could not save the token to Credential Manager; it will be used until exit.", MB_ICONWARNING);
                }
                lock(&self.shared).token = Some(t);
                let _ = self.tx.send(Cmd::CheckNow);
            }
            Some(TokenAction::Remove) => {
                win::token_delete();
                lock(&self.shared).token = None;
            }
            None => {}
        }
    }

    fn exit(&self) {
        let _ = self.tx.send(Cmd::Quit);
        let n = self.nid();
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &n);
            DestroyWindow(self.hwnd);
        }
    }
}

/// Resolves user input to a canonical repo/branch, hitting the API. Runs on the UI thread
/// inside the dialog (with a wait cursor); requests time out after 20 s.
fn validate_repo(shared: &Mutex<Shared>, client: &Client, form: &RepoForm, adding: bool) -> Result<RepoForm, String> {
    let name = github::parse_repo(&form.repo).ok_or("Enter a repository as owner/name, or paste its GitHub URL.")?;
    let token = lock(shared).token.clone();
    let token = token.as_deref();
    let api_err = |e: ApiError| match e {
        ApiError::RateLimited(t) => format!("GitHub rate limit reached. Try again after {}, or set a token.", win::local_hhmm(t)),
        e => format!("Could not reach GitHub: {e}"),
    };
    let info = client.repo_info(token, &name).map_err(|e| match e {
        ApiError::NotFound => format!("{name} was not found. Private repositories need a token with access to them."),
        e => api_err(e),
    })?;
    if adding && lock(shared).config.find(&info.full_name).is_some() {
        return Err(format!("{} is already in the list.", info.full_name));
    }
    let branch = match &form.branch {
        Some(b) if form.mode == Mode::Commits => match client.branch_exists(token, &info.full_name, b) {
            Ok(true) => b.clone(),
            Ok(false) => return Err(format!("{} has no branch named \"{b}\".", info.full_name)),
            Err(e) => return Err(api_err(e)),
        },
        Some(b) => b.clone(),
        None => info.default_branch,
    };
    Ok(RepoForm { repo: info.full_name, mode: form.mode, branch: Some(branch) })
}
