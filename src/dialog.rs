//! Minimal modal Win32 dialogs built from plain windows (no .rc templates).
//! Only one dialog can be open at a time.

use crate::config::Mode;
use crate::win::{self, wide};
use std::cell::{Cell, RefCell};
use std::ptr::{null, null_mut};
use std::rc::Rc;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "UpdateNotifierDialog";
const ID_OK: u16 = IDOK as u16;
const ID_CANCEL: u16 = IDCANCEL as u16;

type Handler = Box<dyn FnMut(HWND, u16, u16) -> bool>;

thread_local! {
    static ACTIVE: Cell<HWND> = const { Cell::new(null_mut()) };
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
    static ICON: Cell<HICON> = const { Cell::new(null_mut()) };
}

/// The open dialog, if any.
pub fn active() -> Option<HWND> {
    let h = ACTIVE.get();
    (!h.is_null()).then_some(h)
}

pub fn set_icon(icon: HICON) {
    ICON.set(icon);
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let (id, code) = ((wp & 0xFFFF) as u16, ((wp >> 16) & 0xFFFF) as u16);
                if id == ID_CANCEL {
                    DestroyWindow(hwnd);
                    return 0;
                }
                // Take the handler out while it runs: it may pump messages (MessageBox).
                if let Some(mut f) = HANDLER.with(|h| h.borrow_mut().take()) {
                    let close = f(hwnd, id, code);
                    HANDLER.with(|h| *h.borrow_mut() = Some(f));
                    if close {
                        DestroyWindow(hwnd);
                    }
                }
                0
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

struct Builder {
    hwnd: HWND,
    font: HFONT,
    scale: f32,
}

impl Builder {
    fn new(title: &str, w: i32, h: i32) -> Self {
        unsafe {
            let hinst = GetModuleHandleW(null());
            let class = wide(CLASS);
            let mut wc: WNDCLASSEXW = std::mem::zeroed();
            wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
            wc.lpfnWndProc = Some(proc);
            wc.hInstance = hinst;
            wc.hIcon = ICON.get();
            wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
            wc.hbrBackground = (COLOR_BTNFACE + 1) as usize as HBRUSH;
            wc.lpszClassName = class.as_ptr();
            RegisterClassExW(&wc); // fails harmlessly if already registered

            let scale = GetDpiForSystem() as f32 / 96.0;
            let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
            let ex = WS_EX_DLGMODALFRAME;
            let mut rc = RECT { left: 0, top: 0, right: (w as f32 * scale) as i32, bottom: (h as f32 * scale) as i32 };
            AdjustWindowRectEx(&mut rc, style, 0, ex);
            let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
            let mut work: RECT = std::mem::zeroed();
            SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut work).cast(), 0);
            let x = work.left + (work.right - work.left - ww) / 2;
            let y = work.top + (work.bottom - work.top - wh) / 2;
            let t = wide(title);
            let hwnd = CreateWindowExW(ex, class.as_ptr(), t.as_ptr(), style, x, y, ww, wh, null_mut(), null_mut(), hinst, null());

            let mut ncm: NONCLIENTMETRICSW = std::mem::zeroed();
            ncm.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
            SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, ncm.cbSize, (&raw mut ncm).cast(), 0);
            let font = CreateFontIndirectW(&ncm.lfMessageFont);
            Self { hwnd, font, scale }
        }
    }

    fn s(&self, v: i32) -> i32 {
        (v as f32 * self.scale).round() as i32
    }

    #[allow(clippy::too_many_arguments)]
    fn ctl(&self, class: &str, text: &str, style: u32, ex: u32, x: i32, y: i32, w: i32, h: i32, id: u16) -> HWND {
        let (c, t) = (wide(class), wide(text));
        unsafe {
            let h = CreateWindowExW(
                ex,
                c.as_ptr(),
                t.as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                self.s(x),
                self.s(y),
                self.s(w),
                self.s(h),
                self.hwnd,
                id as usize as HMENU,
                GetModuleHandleW(null()),
                null(),
            );
            SendMessageW(h, WM_SETFONT, self.font as usize, 1);
            h
        }
    }

    fn label(&self, text: &str, x: i32, y: i32, w: i32, h: i32) -> HWND {
        self.ctl("STATIC", text, 0 /* SS_LEFT */, 0, x, y, w, h, 0xFFFF)
    }

    fn edit(&self, text: &str, extra: u32, x: i32, y: i32, w: i32, id: u16) -> HWND {
        self.ctl("EDIT", text, WS_TABSTOP | ES_AUTOHSCROLL as u32 | extra, WS_EX_CLIENTEDGE, x, y, w, 23, id)
    }

    fn button(&self, text: &str, style: i32, x: i32, y: i32, w: i32, id: u16) -> HWND {
        self.ctl("BUTTON", text, WS_TABSTOP | style as u32, 0, x, y, w, 26, id)
    }

    /// Shows the window and runs a nested message loop until it is destroyed.
    fn run(self, focus: HWND, handler: Handler) {
        unsafe {
            HANDLER.with(|h| *h.borrow_mut() = Some(handler));
            ACTIVE.set(self.hwnd);
            ShowWindow(self.hwnd, SW_SHOW);
            SetForegroundWindow(self.hwnd);
            SetFocus(focus);
            let mut msg: MSG = std::mem::zeroed();
            while IsWindow(self.hwnd) != 0 {
                match GetMessageW(&mut msg, null_mut(), 0, 0) {
                    0 => {
                        PostQuitMessage(msg.wParam as i32); // let the outer loop see WM_QUIT too
                        break;
                    }
                    -1 => break,
                    _ => {
                        if IsDialogMessageW(self.hwnd, &msg) == 0 {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }
            }
            if IsWindow(self.hwnd) != 0 {
                DestroyWindow(self.hwnd);
            }
            HANDLER.with(|h| h.borrow_mut().take());
            ACTIVE.set(null_mut());
            DeleteObject(self.font);
        }
    }
}

fn set_cue(edit: HWND, text: &str) {
    let t = wide(text);
    unsafe { SendMessageW(edit, EM_SETCUEBANNER, 1, t.as_ptr() as isize) };
}

fn checked(h: HWND) -> bool {
    unsafe { SendMessageW(h, BM_GETCHECK, 0, 0) == BST_CHECKED as isize }
}

/// Runs a potentially slow validation with a wait cursor and the window disabled.
fn busy<T>(hwnd: HWND, f: impl FnOnce() -> T) -> T {
    unsafe {
        let focus = GetFocus();
        let old = SetCursor(LoadCursorW(null_mut(), IDC_WAIT));
        EnableWindow(hwnd, 0);
        let r = f();
        EnableWindow(hwnd, 1);
        SetCursor(old);
        SetForegroundWindow(hwnd);
        SetFocus(focus);
        r
    }
}

#[derive(Clone, Debug)]
pub struct RepoForm {
    pub repo: String,
    pub mode: Mode,
    pub branch: Option<String>,
}

/// Add (initial = None) or edit a repository. `validate` may hit the network
/// and returns the canonical form to store, or an error message to show.
pub fn repo_dialog(initial: Option<&RepoForm>, mut validate: impl FnMut(&RepoForm) -> Result<RepoForm, String> + 'static) -> Option<RepoForm> {
    if active().is_some() {
        return None;
    }
    let editing = initial.is_some();
    let b = Builder::new(if editing { "Edit repository" } else { "Add repository" }, 380, 214);

    b.label("Repository (owner/name or GitHub URL):", 14, 14, 352, 18);
    let repo = b.edit(initial.map_or("", |f| f.repo.as_str()), if editing { ES_READONLY as u32 } else { 0 }, 14, 34, 352, 101);
    set_cue(repo, "e.g. neovim/neovim");
    b.label("Notify me about:", 14, 70, 352, 18);
    let r_stable = b.button("Stable releases", BS_AUTORADIOBUTTON | WS_GROUP as i32, 24, 90, 330, 102);
    let r_pre = b.button("All releases, including pre-releases", BS_AUTORADIOBUTTON, 24, 114, 330, 103);
    let r_commits = b.button("New commits on branch:", BS_AUTORADIOBUTTON, 24, 138, 170, 104);
    let branch = b.ctl(
        "EDIT",
        initial.and_then(|f| f.branch.as_deref()).unwrap_or(""),
        WS_TABSTOP | WS_GROUP | ES_AUTOHSCROLL as u32,
        WS_EX_CLIENTEDGE,
        196,
        137,
        170,
        23,
        105,
    );
    set_cue(branch, "default branch");
    let ok = b.button(if editing { "Save" } else { "Add" }, BS_DEFPUSHBUTTON | WS_GROUP as i32, 200, 176, 80, ID_OK);
    b.button("Cancel", BS_PUSHBUTTON, 286, 176, 80, ID_CANCEL);

    let mode = initial.map_or(Mode::Stable, |f| f.mode);
    let radio = match mode {
        Mode::Stable => r_stable,
        Mode::Prerelease => r_pre,
        Mode::Commits => r_commits,
    };
    unsafe {
        SendMessageW(radio, BM_SETCHECK, BST_CHECKED as usize, 0);
        EnableWindow(branch, (mode == Mode::Commits) as i32);
    }
    let _ = ok;

    let result = Rc::new(RefCell::new(None));
    let out = result.clone();
    let focus = if editing { radio } else { repo };
    b.run(
        focus,
        Box::new(move |hwnd, id, code| match id {
            102..=104 if code == BN_CLICKED as u16 => {
                unsafe { EnableWindow(branch, checked(r_commits) as i32) };
                false
            }
            ID_OK => {
                let mode = if checked(r_commits) {
                    Mode::Commits
                } else if checked(r_pre) {
                    Mode::Prerelease
                } else {
                    Mode::Stable
                };
                let br = win::window_text(branch).trim().to_owned();
                let form = RepoForm { repo: win::window_text(repo), mode, branch: (!br.is_empty()).then_some(br) };
                match busy(hwnd, || validate(&form)) {
                    Ok(f) => {
                        *out.borrow_mut() = Some(f);
                        true
                    }
                    Err(e) => {
                        win::message_box(hwnd, &e, MB_ICONWARNING);
                        false
                    }
                }
            }
            _ => false,
        }),
    );
    result.take()
}

pub enum TokenAction {
    Set(String),
    Remove,
}

pub fn token_dialog(has_token: bool, mut validate: impl FnMut(&str) -> Result<(), String> + 'static) -> Option<TokenAction> {
    if active().is_some() {
        return None;
    }
    let b = Builder::new("GitHub token", 400, 190);
    b.label(
        "Without a token GitHub allows 60 requests/hour, and every check costs one request \
         per repository. With one, the limit is 5,000/hour and unchanged repos cost nothing.\n\n\
         A fine-grained token with no permissions is enough for public repos. \
         It is stored in Windows Credential Manager.",
        14,
        12,
        372,
        86,
    );
    let edit = b.edit("", ES_PASSWORD as u32, 14, 104, 372, 101);
    set_cue(edit, if has_token { "(token saved, type to replace)" } else { "github_pat_…" });
    b.button("Create token…", BS_PUSHBUTTON, 14, 148, 100, 110);
    let remove = b.button("Remove", BS_PUSHBUTTON, 120, 148, 80, 111);
    unsafe { EnableWindow(remove, has_token as i32) };
    b.button("Save", BS_DEFPUSHBUTTON, 220, 148, 80, ID_OK);
    b.button("Cancel", BS_PUSHBUTTON, 306, 148, 80, ID_CANCEL);

    let result = Rc::new(RefCell::new(None));
    let out = result.clone();
    b.run(
        edit,
        Box::new(move |hwnd, id, _| match id {
            110 => {
                win::open_url("https://github.com/settings/personal-access-tokens/new");
                false
            }
            111 => {
                *out.borrow_mut() = Some(TokenAction::Remove);
                true
            }
            ID_OK => {
                let t = win::window_text(edit).trim().to_owned();
                if t.is_empty() {
                    return true; // nothing typed: keep whatever is there
                }
                match busy(hwnd, || validate(&t)) {
                    Ok(()) => {
                        *out.borrow_mut() = Some(TokenAction::Set(t));
                        true
                    }
                    Err(e) => {
                        win::message_box(hwnd, &e, MB_ICONWARNING);
                        false
                    }
                }
            }
            _ => false,
        }),
    );
    result.take()
}
