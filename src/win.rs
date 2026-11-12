//! Small Win32 helpers: strings, shell, registry, credential manager, time.

use std::ptr::{null, null_mut};
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Credentials::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Time::*;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub const APP_NAME: &str = "Update Notifier";

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies `s` into a fixed-size, NUL-terminated buffer, truncating with "…".
pub fn copy_to(dst: &mut [u16], s: &str) {
    let max = dst.len() - 1;
    let mut units: Vec<u16> = s.encode_utf16().collect();
    if units.len() > max {
        units.truncate(max - 1);
        if units.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
            units.pop(); // don't leave half a surrogate pair
        }
        units.push('…' as u16);
    }
    dst[..units.len()].copy_from_slice(&units);
    dst[units.len()] = 0;
}

pub fn open_url(url: &str) {
    let (op, url) = (wide("open"), wide(url));
    unsafe {
        ShellExecuteW(
            null_mut(),
            op.as_ptr(),
            url.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
}

pub fn message_box(owner: HWND, text: &str, flags: u32) -> i32 {
    let (t, c) = (wide(text), wide(APP_NAME));
    unsafe { MessageBoxW(owner, t.as_ptr(), c.as_ptr(), flags | MB_SETFOREGROUND) }
}

pub fn window_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

/// Local wall-clock "HH:MM" for a SystemTime.
pub fn local_hhmm(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let ticks = (d.as_secs() + 11_644_473_600) * 10_000_000 + u64::from(d.subsec_nanos() / 100);
    let ft = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    unsafe {
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        let mut local: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&ft, &mut utc) == 0
            || SystemTimeToTzSpecificLocalTime(null(), &utc, &mut local) == 0
        {
            return "?".into();
        }
        format!("{:02}:{:02}", local.wHour, local.wMinute)
    }
}

// ---- Start with Windows (HKCU Run key) ----

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "UpdateNotifier";

fn exe_command() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    format!("\"{}\"", exe.display())
}

pub fn startup_enabled() -> bool {
    let (key, val) = (wide(RUN_KEY), wide(RUN_VALUE));
    let mut buf = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if rc != ERROR_SUCCESS {
        return false;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    // Only count it as enabled if it points at this exe (it may have been moved).
    String::from_utf16_lossy(&buf[..len]).eq_ignore_ascii_case(&exe_command())
}

pub fn set_startup(on: bool) -> bool {
    let (key, val) = (wide(RUN_KEY), wide(RUN_VALUE));
    unsafe {
        if on {
            let data = wide(&exe_command());
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                val.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            ) == ERROR_SUCCESS
        } else {
            let rc = RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr());
            rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND
        }
    }
}

// ---- GitHub token in Windows Credential Manager ----

const CRED_TARGET: &str = "UpdateNotifier:github-token";

pub fn token_read() -> Option<String> {
    let target = wide(CRED_TARGET);
    unsafe {
        let mut p: *mut CREDENTIALW = null_mut();
        if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) == 0 {
            return None;
        }
        let c = &*p;
        let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
        let s = String::from_utf8(blob.to_vec())
            .ok()
            .filter(|s| !s.is_empty());
        CredFree(p.cast());
        s
    }
}

pub fn token_write(token: &str) -> bool {
    let mut target = wide(CRED_TARGET);
    let mut user = wide("github");
    let mut blob = token.as_bytes().to_vec();
    unsafe {
        let mut c: CREDENTIALW = std::mem::zeroed();
        c.Type = CRED_TYPE_GENERIC;
        c.TargetName = target.as_mut_ptr();
        c.UserName = user.as_mut_ptr();
        c.CredentialBlobSize = blob.len() as u32;
        c.CredentialBlob = blob.as_mut_ptr();
        c.Persist = CRED_PERSIST_LOCAL_MACHINE;
        CredWriteW(&c, 0) != 0
    }
}

pub fn token_delete() {
    let target = wide(CRED_TARGET);
    unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
}
