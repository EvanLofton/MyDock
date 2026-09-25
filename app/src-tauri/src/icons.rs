//! 图标提取：从 exe 取高分辨率图标，转成 BGRA 供前端用 canvas 转 PNG。
//!
//! 为什么不用 `ExtractIconEx` / `SHGetFileInfo`：它们只能给 16/32px，
//! 在 125% DPI 下必然糊。必须走 `IShellItemImageFactory::GetImage` 并请求 256px。

#![allow(dead_code)]

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};

pub struct IconData {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// 从 exe 路径提取图标。`size` 为请求的边长（物理像素）。
///
/// `.url`（Internet 快捷方式）走一条**特殊前置**：Shell 对它一律返回那个通用的
/// 「Internet 快捷方式」图标（蓝地球 + 箭头，实测），而用户期待的是**它指向的那个东西**的图标。
/// 所以先按快捷方式自己写的 `IconFile=` 取（Steam 的桌面快捷方式指着游戏目录里的
/// `launcher.exe`），取不到再退回 Shell 图标。
///
/// ⚠️ 这里刻意**不做链式递归**：`IconFile` 指向的东西一律用 `extract_from_path`
/// 直接取，所以 `IconFile` 万一又是个 `.url` 也不会转圈。
pub fn extract_icon(path: &str, size: u32) -> Option<IconData> {
    if let Some(src) = crate::apps::url_icon_source(path) {
        if let Some(d) = extract_from_path(&src, size) {
            return Some(d);
        }
    }
    extract_from_path(path, size)
}

/// 直接问 Shell 要这个路径的图标（不做任何 `.url` 特殊处理）。
fn extract_from_path(path: &str, size: u32) -> Option<IconData> {    let size = size.clamp(16, 256);
    let w = crate::apps::wide(path);
    unsafe {
        let item: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok()?;
        let hbmp = item
            .GetImage(
                SIZE {
                    cx: size as i32,
                    cy: size as i32,
                },
                SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
            )
            .ok()?;
        let data = hbitmap_to_bgra(hbmp);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        data
    }
}

/// 图标里**真正有内容**的那块矩形（alpha > 阈值的包围盒）。
///
/// 为什么需要它：图标的画布是固定方的，但**图形本身在画布里留多少白**各不相同 ——
/// 「此电脑」的显示器画得偏小，「微信」几乎是满格。于是"看起来的间距" =
/// 布局间距 + 左边图标的右留白 + 右边图标的左留白。
/// 用户说"某几个图标之间显得空"，量这个才知道是布局的问题还是图形留白的问题。
///
/// 返回 `(left, top, right, bottom)`，都是**含**边界的像素下标。
pub fn alpha_bbox(d: &IconData, threshold: u8) -> Option<(u32, u32, u32, u32)> {
    if d.width == 0 || d.height == 0 {
        return None;
    }
    let (mut x0, mut y0) = (u32::MAX, u32::MAX);
    let (mut x1, mut y1) = (0u32, 0u32);
    let mut any = false;
    for y in 0..d.height {
        for x in 0..d.width {
            let a = d.bgra[((y * d.width + x) * 4 + 3) as usize];
            if a > threshold {
                any = true;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    any.then_some((x0, y0, x1, y1))
}

fn hbitmap_to_bgra(hbmp: HBITMAP) -> Option<IconData> {
    unsafe {
        let mut bm = BITMAP::default();
        let n = GetObjectW(
            HGDIOBJ(hbmp.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut c_void),
        );
        if n == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight);

        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let old = SelectObject(mem, HGDIOBJ(hbmp.0));

        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h; // top-down
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = 0; // BI_RGB

        let mut buf = vec![0u8; (w * h * 4) as usize];
        GetDIBits(
            mem,
            hbmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bi,
            DIB_RGB_COLORS,
        );

        SelectObject(mem, old);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);

        normalize_alpha(&mut buf);

        Some(IconData {
            width: w as u32,
            height: h as u32,
            bgra: buf,
        })
    }
}

/// alpha 统计：用来判断提取出来的图标到底有没有透明区域。
///
/// 为什么需要：如果 `IShellItemImageFactory` 返回的位图**整体不透明**，
/// 图标在 Dock 上就会显示成一块方形底板（看起来像"图标有背景图"）。
/// 有了这个统计就能区分「图标本身就没有透明区」和「我们的转换把 alpha 丢了」。
pub struct AlphaStats {
    pub min: u8,
    pub max: u8,
    /// 完全透明（alpha == 0）的像素占比，0.0 - 1.0
    pub transparent_ratio: f64,
    /// 完全不透明（alpha == 255）的像素占比
    pub opaque_ratio: f64,
}

pub fn alpha_stats(d: &IconData) -> AlphaStats {
    let total = d.bgra.len() / 4;
    if total == 0 {
        return AlphaStats {
            min: 0,
            max: 0,
            transparent_ratio: 0.0,
            opaque_ratio: 0.0,
        };
    }
    let mut min = 255u8;
    let mut max = 0u8;
    let mut zero = 0usize;
    let mut full = 0usize;
    for px in d.bgra.chunks_exact(4) {
        let a = px[3];
        if a < min {
            min = a;
        }
        if a > max {
            max = a;
        }
        if a == 0 {
            zero += 1;
        }
        if a == 255 {
            full += 1;
        }
    }
    AlphaStats {
        min,
        max,
        transparent_ratio: zero as f64 / total as f64,
        opaque_ratio: full as f64 / total as f64,
    }
}
///
/// 判定方式很可靠：预乘 alpha 下任何通道都不可能大于 alpha 通道。
/// 只要发现一个像素某通道 > alpha，就说明它已经是直通 alpha，原样返回。
fn normalize_alpha(buf: &mut [u8]) {
    let mut looks_premultiplied = true;
    let mut has_translucent = false;
    for px in buf.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 || a == 255 {
            continue;
        }
        has_translucent = true;
        if px[0] > a || px[1] > a || px[2] > a {
            looks_premultiplied = false;
            break;
        }
    }
    if !looks_premultiplied || !has_translucent {
        return;
    }
    for px in buf.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a == 0 || a == 255 {
            continue;
        }
        for c in 0..3 {
            let v = (px[c] as u32 * 255 + a / 2) / a;
            px[c] = v.min(255) as u8;
        }
    }
}
