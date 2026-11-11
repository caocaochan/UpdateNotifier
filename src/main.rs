#![windows_subsystem = "windows"]

mod config;
mod dialog;
mod github;
mod icon;
mod theme;
mod ui;
mod win;
mod worker;

use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_SYSTEM_AWARE, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONINFORMATION;

fn main() {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE) };

    let name = win::wide(r"Local\UpdateNotifier.SingleInstance");
    let _mutex = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        win::message_box(null_mut(), "Update Notifier is already running. Look for it in the system tray.", MB_ICONINFORMATION);
        return;
    }

    let (config, warning) = config::load();
    let token = win::token_read().or_else(|| std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.trim().is_empty()));
    let shared = Arc::new(Mutex::new(worker::Shared::new(config, token)));
    ui::run(shared, warning);
}
