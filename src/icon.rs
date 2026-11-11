//! Draws the tray icon at runtime (no .ico/.rc needed): a blue disc with a
//! white up-arrow, plus a red badge when there are unseen updates.

use std::ptr::null_mut;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

type Rgb = (f32, f32, f32);
const BLUE: Rgb = (37.0, 99.0, 235.0);
const WHITE: Rgb = (255.0, 255.0, 255.0);
const RED: Rgb = (220.0, 38.0, 38.0);

/// Sample the design (16x16 unit space) at one point.
fn sample(x: f32, y: f32, badge: bool) -> Option<Rgb> {
    let dist = |cx: f32, cy: f32| ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
    if badge {
        let d = dist(12.6, 3.4);
        if d < 3.1 {
            return Some(RED);
        }
        if d < 4.3 {
            return None; // transparent ring separating badge from disc
        }
    }
    if dist(8.0, 8.0) > 7.6 {
        return None;
    }
    let shaft = (x - 8.0).abs() <= 1.15 && (6.5..=12.6).contains(&y);
    let head = (3.0..=8.2).contains(&y) && (x - 8.0).abs() <= (y - 3.0) / 5.2 * 4.2;
    Some(if shaft || head { WHITE } else { BLUE })
}

pub fn make(size: i32, badge: bool) -> HICON {
    const SS: i32 = 4; // 4x4 supersampling for anti-aliasing
    let unit = 16.0 / size as f32;
    let mut px = vec![0u32; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let (mut r, mut g, mut b, mut hits) = (0.0, 0.0, 0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let ux = (x as f32 + (sx as f32 + 0.5) / SS as f32) * unit;
                    let uy = (y as f32 + (sy as f32 + 0.5) / SS as f32) * unit;
                    if let Some(c) = sample(ux, uy, badge) {
                        r += c.0;
                        g += c.1;
                        b += c.2;
                        hits += 1.0;
                    }
                }
            }
            if hits > 0.0 {
                let a = hits / (SS * SS) as f32;
                // Icons use straight (non-premultiplied) alpha.
                let (r, g, b) = (r / hits, g / hits, b / hits);
                px[(y * size + x) as usize] =
                    ((a * 255.0).round() as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32;
            }
        }
    }

    unsafe {
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = size;
        bmi.bmiHeader.biHeight = -size; // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits = null_mut();
        let hdc = GetDC(null_mut());
        let color = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        ReleaseDC(null_mut(), hdc);
        if color.is_null() {
            return null_mut();
        }
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits.cast::<u32>(), px.len());
        // Monochrome mask rows are WORD-aligned; all-zero = use the alpha channel.
        let mask_bits = vec![0u8; (((size + 15) / 16) * 2 * size) as usize];
        let mask = CreateBitmap(size, size, 1, 1, mask_bits.as_ptr().cast());
        let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}
