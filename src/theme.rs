//! Dark popup menus. Win32 menus have no documented dark mode; Explorer and
//! most native apps use two undocumented uxtheme exports (Windows 10 1903+):
//!   #135 SetPreferredAppMode(mode)  0 Default, 1 AllowDark, 2 ForceDark, 3 ForceLight
//!   #136 FlushMenuThemes()
//! We follow the *Windows* mode (taskbar/tray), not the app mode, since this is a tray menu.

use crate::win::wide;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};

const FORCE_DARK: i32 = 2;
const FORCE_LIGHT: i32 = 3;

pub fn system_is_dark() -> bool {
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let val = wide("SystemUsesLightTheme");
    let mut data: u32 = 1;
    let mut size = size_of::<u32>() as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&raw mut data).cast(),
            &mut size,
        )
    };
    rc == ERROR_SUCCESS && data == 0
}

/// Applies the current Windows light/dark mode to this process's menus.
/// Call at startup and on WM_SETTINGCHANGE("ImmersiveColorSet").
pub fn apply() {
    let mode = if system_is_dark() {
        FORCE_DARK
    } else {
        FORCE_LIGHT
    };
    unsafe {
        let lib = LoadLibraryExW(
            wide("uxtheme.dll").as_ptr(),
            null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        if lib.is_null() {
            return;
        }
        // Ordinals are passed as the low word of the name pointer (MAKEINTRESOURCEA).
        if let Some(f) = GetProcAddress(lib, 135usize as *const u8) {
            let set_preferred_app_mode: unsafe extern "system" fn(i32) -> i32 =
                std::mem::transmute(f);
            set_preferred_app_mode(mode);
        }
        if let Some(f) = GetProcAddress(lib, 136usize as *const u8) {
            let flush_menu_themes: unsafe extern "system" fn() = std::mem::transmute(f);
            flush_menu_themes();
        }
        // uxtheme stays loaded (comctl32 v6 already depends on it), so no FreeLibrary.
    }
}
