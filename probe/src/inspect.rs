//! 抓取某个窗口并输出粗略颜色网格，用于**无需肉眼**地确认界面是否真的渲染出来了。
//! 用法: probe inspect <窗口类名> <标题> [列数] [行数]

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::win;

pub fn run(class: &str, title: &str, cols: i32, rows: i32) {
    let class_w = win::wide(class);
    let title_w = win::wide(title);
    let hwnd = unsafe { FindWindowW(PCWSTR(class_w.as_ptr()), PCWSTR(title_w.as_ptr())) }
        .unwrap_or_default();
    if hwnd.0.is_null() {
        println!("找不到窗口 class='{class}' title='{title}'");
        return;
    }

    let rc = win::window_rect(hwnd);
    let (x, y) = (rc.left, rc.top);
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    println!("窗口 0x{:X}  位置 ({x},{y})  尺寸 {w}x{h}", hwnd.0 as usize);

    // 内缩一圈，避开边框与阴影
    let inset = 6;
    let sx = x + inset;
    let sy = y + inset;
    let sw = w - inset * 2;
    let sh = h - inset * 2;

    let shot = win::capture(sx, sy, sw, sh);
    let (mean, std) = shot.stats();
    println!("整体：亮度均值 {mean:.1}，标准差 {std:.1}");

    if std < 1.0 {
        println!("=> 画面**完全均匀**，界面很可能没有渲染出来（或窗口是空的）");
    }

    let cw = (shot.w as f64 / cols as f64).max(1.0);
    let ch = (shot.h as f64 / rows as f64).max(1.0);

    println!("\n颜色网格（每格为平均色）：");
    for r in 0..rows {
        let mut line = String::new();
        for c in 0..cols {
            let x0 = (c as f64 * cw) as i32;
            let y0 = (r as f64 * ch) as i32;
            let x1 = (((c + 1) as f64 * cw) as i32).min(shot.w);
            let y1 = (((r + 1) as f64 * ch) as i32).min(shot.h);
            let (mut sr, mut sg, mut sb, mut n) = (0f64, 0f64, 0f64, 0f64);
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let i = ((yy * shot.w + xx) * 4) as usize;
                    if i + 2 < shot.px.len() {
                        sb += shot.px[i] as f64;
                        sg += shot.px[i + 1] as f64;
                        sr += shot.px[i + 2] as f64;
                        n += 1.0;
                    }
                }
            }
            if n == 0.0 {
                n = 1.0;
            }
            let (rr, gg, bb) = ((sr / n) as u8, (sg / n) as u8, (sb / n) as u8);
            // 用色块字符表示亮度，便于肉眼看形状
            let luma = 0.299 * rr as f64 + 0.587 * gg as f64 + 0.114 * bb as f64;
            let ch_sym = match luma as i32 / 26 {
                0..=1 => '.',
                2..=3 => ':',
                4..=5 => 'o',
                6..=7 => 'O',
                8..=9 => '#',
                _ => '@',
            };
            line.push_str(&format!("{rr:02X}{gg:02X}{bb:02X}{ch_sym} "));
        }
        println!("  {line}");
    }

    // 统计「有多少个明显有色的格子」——占位图标是彩色的，能据此判断是否渲染
    let mut colorful = 0;
    let mut total = 0;
    for r in 0..rows {
        for c in 0..cols {
            let x0 = (c as f64 * cw) as i32;
            let y0 = (r as f64 * ch) as i32;
            let (mut sr, mut sg, mut sb, mut n) = (0f64, 0f64, 0f64, 0f64);
            for yy in y0..(((r + 1) as f64 * ch) as i32).min(shot.h) {
                for xx in x0..(((c + 1) as f64 * cw) as i32).min(shot.w) {
                    let i = ((yy * shot.w + xx) * 4) as usize;
                    if i + 2 < shot.px.len() {
                        sb += shot.px[i] as f64;
                        sg += shot.px[i + 1] as f64;
                        sr += shot.px[i + 2] as f64;
                        n += 1.0;
                    }
                }
            }
            if n == 0.0 {
                continue;
            }
            total += 1;
            let (rr, gg, bb) = (sr / n, sg / n, sb / n);
            let mx = rr.max(gg).max(bb);
            let mn = rr.min(gg).min(bb);
            if mx > 40.0 && (mx - mn) > 30.0 {
                colorful += 1;
            }
        }
    }
    println!("\n有色格子 {colorful}/{total}  ({:.0}%)", 100.0 * colorful as f64 / total.max(1) as f64);

    // 内容包围盒：以「主色」为背景基准，凡是明显偏离主色的像素都算内容。
    // 不能用「彩色度」判定 —— 灰色/白色图标（如设置、记事本）没有色相，
    // 会被漏掉从而把包围盒算歪（踩过这个坑，导致误判成「偏左 43px」）。
    let mut hist: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for yy in 0..shot.h {
        for xx in 0..shot.w {
            let i = ((yy * shot.w + xx) * 4) as usize;
            if i + 2 >= shot.px.len() {
                continue;
            }
            let b = shot.px[i] as u32;
            let g = shot.px[i + 1] as u32;
            let r = shot.px[i + 2] as u32;
            // 量化到 16 级，避免抗锯齿噪声把直方图打散
            let key = ((r >> 4) << 8) | ((g >> 4) << 4) | (b >> 4);
            *hist.entry(key).or_insert(0) += 1;
        }
    }
    let dom = match hist.iter().max_by_key(|(_, v)| **v) {
        Some((k, _)) => *k,
        None => {
            println!("画面为空");
            return;
        }
    };
    let dr = ((dom >> 8) & 0xF) as i32 * 16 + 8;
    let dg = ((dom >> 4) & 0xF) as i32 * 16 + 8;
    let db = (dom & 0xF) as i32 * 16 + 8;
    println!("\n背景主色 ≈ #{dr:02X}{dg:02X}{db:02X}（以此判定内容边界）");

    let (mut minx, mut maxx) = (i32::MAX, -1i32);
    let (mut miny, mut maxy) = (i32::MAX, -1i32);
    for yy in 0..shot.h {
        for xx in 0..shot.w {
            let i = ((yy * shot.w + xx) * 4) as usize;
            if i + 2 >= shot.px.len() {
                continue;
            }
            let b = shot.px[i] as i32;
            let g = shot.px[i + 1] as i32;
            let r = shot.px[i + 2] as i32;
            let d = (r - dr).abs().max((g - dg).abs()).max((b - db).abs());
            if d > 28 {
                minx = minx.min(xx);
                maxx = maxx.max(xx);
                miny = miny.min(yy);
                maxy = maxy.max(yy);
            }
        }
    }
    if maxx < 0 {
        println!("未找到偏离背景的内容");
        return;
    }
    println!(
        "\n内容包围盒: x {minx}..{maxx}（宽 {}）  y {miny}..{maxy}（高 {}）",
        maxx - minx,
        maxy - miny
    );
    println!("窗口内容宽 {}，中心 x = {}", shot.w, shot.w / 2);
    println!("内容中心 x = {}", (minx + maxx) / 2);
    let dx = (minx + maxx) / 2 - shot.w / 2;
    println!(
        "水平偏移 = {dx} px  =>  {}",
        if dx.abs() <= 10 {
            "水平居中 ✅"
        } else if dx < 0 {
            "**偏左** ⚠️"
        } else {
            "**偏右** ⚠️"
        }
    );

    // 角点 vs 中心：判断「窗口圆角」是否真的把 OS 材质裁掉了。
    // 网格分辨率不够（一格几十像素），必须直接读单像素。
    let full = win::capture(x, y, w, h);
    let pt = |px: i32, py: i32| -> String {
        if px < 0 || py < 0 || px >= full.w || py >= full.h {
            return "-".into();
        }
        let i = ((py * full.w + px) * 4) as usize;
        if i + 2 < full.px.len() {
            format!(
                "#{:02X}{:02X}{:02X}",
                full.px[i + 2], full.px[i + 1], full.px[i]
            )
        } else {
            "-".into()
        }
    };
    println!("\n角点 vs 中心（判断圆角是否真的裁掉了 OS 材质）：");
    println!("  左上 {}   右上 {}", pt(1, 1), pt(full.w - 2, 1));
    println!("  左下 {}   右下 {}", pt(1, full.h - 2), pt(full.w - 2, full.h - 2));
    println!("  中心 {}", pt(full.w / 2, full.h / 2));
    println!("  => 四角若与中心不同，说明圆角生效；完全相同说明材质没被裁");
}
