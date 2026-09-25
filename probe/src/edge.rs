//! T1c：用「锐利边缘」直接量出毛玻璃的模糊半径
//!
//! T1 的 8px 棋盘格可能远小于亚克力的模糊半径，导致整片被平均掉而误判为
//! "不透明"。本测试改用**左黑右白的单条锐利竖直边缘**，直接测量
//! 10%→90% 的过渡宽度（像素）：
//!   - 无模糊      → 过渡宽度 ≈ 1-2 px，对比度 ≈ 255
//!   - 真正模糊    → 过渡宽度显著变大，对比度下降
//! 这比方差判定无歧义，且能定量给出模糊强度。

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::glass::{self, apply_route_b};
use crate::win;

static OPAQUE_RED: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn half_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let half = rc.right / 2;
            let _ = PatBlt(hdc, 0, 0, half, rc.bottom, BLACKNESS);
            let _ = PatBlt(hdc, half, 0, rc.right - half, rc.bottom, WHITENESS);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn glass_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            if OPAQUE_RED.load(Ordering::Relaxed) == 1 {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let brush = CreateSolidBrush(COLORREF(0x0000_00FF));
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                FillRect(hdc, &rc, brush);
                let _ = DeleteObject(HGDIOBJ(brush.0));
                let _ = EndPaint(hwnd, &ps);
            } else {
                let _ = ValidateRect(Some(hwnd), None);
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn holder_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 在给定行上测量边缘过渡宽度
/// 返回 (过渡宽度px, 低电平, 高电平)
fn edge_profile(shot: &win::Shot, y: i32) -> (f64, f64, f64) {
    let mut lo = f64::MAX;
    let mut hi = f64::MIN;
    for x in 0..shot.w {
        let l = shot.luma_at(x, y);
        if l < lo {
            lo = l;
        }
        if l > hi {
            hi = l;
        }
    }
    let span = hi - lo;
    if span < 15.0 {
        return (f64::INFINITY, lo, hi); // 边缘完全消失
    }
    let t10 = lo + 0.10 * span;
    let t90 = lo + 0.90 * span;

    // 从黑侧向右找**首次**跨过 t10 与 t90 的位置。
    // 用「首次跨越」而不是「最后一个不超过」：后者在剖面非单调时
    // （例如扫描线穿过窗口内容里的图标）会得出负的过渡宽度。
    let mut x10 = -1i32;
    let mut x90 = -1i32;
    for x in 0..shot.w {
        let l = shot.luma_at(x, y);
        if x10 < 0 && l >= t10 {
            x10 = x;
        }
        if x10 >= 0 && l >= t90 {
            x90 = x;
            break;
        }
    }
    if x10 < 0 || x90 < 0 {
        return (f64::INFINITY, lo, hi);
    }
    (((x90 - x10).max(0)) as f64, lo, hi)
}

fn measure(sx: i32, sy: i32, sw: i32, sh: i32) -> (f64, f64, f64) {
    let shot = win::capture(sx, sy, sw, sh);
    let mut widths = Vec::new();
    let mut los = Vec::new();
    let mut his = Vec::new();
    for k in 0..5 {
        let y = sh / 6 * (k + 1);
        let (w, lo, hi) = edge_profile(&shot, y);
        widths.push(w);
        los.push(lo);
        his.push(hi);
    }
    let finite: Vec<f64> = widths.iter().copied().filter(|w| w.is_finite()).collect();
    let w = if finite.is_empty() {
        f64::INFINITY
    } else {
        finite.iter().sum::<f64>() / finite.len() as f64
    };
    let lo = los.iter().sum::<f64>() / los.len() as f64;
    let hi = his.iter().sum::<f64>() / his.len() as f64;
    (w, lo, hi)
}

/// 对**外部已存在的窗口**（真实 Tauri Dock）做量化模糊量测 —— 完成 R17。
///
/// 做法：背后放一块左黑右白的锐利边缘背景窗（TOPMOST，先创建），
/// 再把目标窗口 `SetWindowPos` 到背景之上（同为 TOPMOST，后置顶者在上），
/// 抓屏量测边缘过渡宽度。并在**同一个窗口**上做开/关亚克力的 A/B，
/// 这样差异只能由亚克力造成，排除了窗口类型等混杂因素。
/// 打印一条横向亮度剖面，用于诊断「为什么量测是平坦的」
fn dump_profile(sx: i32, sy: i32, sw: i32, sh: i32, label: &str) {
    let shot = win::capture(sx, sy, sw, sh);
    let y = sh / 2;
    let mut s = String::new();
    let mut x = 0;
    while x < shot.w {
        s.push_str(&format!("{:.0} ", shot.luma_at(x, y)));
        x += 25;
    }
    println!("    剖面[{label}] (x 每 25px): {s}");
}

pub fn run_external(class: &str, title: &str) {
    println!("=== R17：真实 Tauri 窗口的量化模糊量测 ===\n");

    unsafe {
        win::register_class("EdgeHalfExt", Some(half_proc), HBRUSH::default());
    }
    let half_cls = win::wide("EdgeHalfExt");
    let class_w = win::wide(class);
    let title_w = win::wide(title);

    let target = unsafe { FindWindowW(PCWSTR(class_w.as_ptr()), PCWSTR(title_w.as_ptr())) }
        .unwrap_or_default();
    if target.0.is_null() {
        println!("找不到目标窗口 class='{class}' title='{title}'，请先启动 Dock 应用");
        return;
    }
    println!("目标窗口 = 0x{:X}", target.0 as usize);

    let bg_x = 200;
    let bg_y = 200;
    let bg_w = 1000;
    let bg_h = 500;
    let edge_x = bg_x + bg_w / 2;

    let bg = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(half_cls.as_ptr()),
            windows::core::w!("edge-half-ext"),
            WS_POPUP | WS_VISIBLE,
            bg_x,
            bg_y,
            bg_w,
            bg_h,
            None,
            None,
            Some(HINSTANCE(win::hmodule().0)),
            None,
        )
    };
    let bg = match bg {
        Ok(h) => h,
        Err(e) => {
            println!("创建背景窗口失败: {e}");
            return;
        }
    };

    // 目标窗口放到边缘上方
    let g_x = edge_x - 300;
    let g_y = 320;
    let g_w = 600;
    let g_h = 260;
    // 采样区：横跨边缘，两侧各留 200px。
    // y 取窗口**上部**的一条带 —— 必须避开窗口内容里的图标：
    // 图标是彩色实心块，会主导亮度剖面、破坏单调性，导致量测失效（踩过这个坑）。
    let sx = edge_x - 200;
    let sy = g_y + 16;
    let sw = 400;
    let sh = 64;

    win::pump_for(700);
    let (bw, blo, bhi) = measure(sx, sy, sw, sh);
    println!(
        "基线（目标窗口未遮挡）  : 过渡宽度 {:>6.1} px，对比度 {:>5.0}",
        bw,
        bhi - blo
    );

    // 把目标窗口移到边缘正上方
    unsafe {
        let _ = SetWindowPos(
            target,
            Some(HWND_TOPMOST),
            g_x,
            g_y,
            g_w,
            g_h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
    win::pump_for(800);

    let r = win::window_rect(target);
    println!(
        "目标窗口实际矩形        : ({},{}) {}x{}",
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top
    );

    let (w_ac, lo_ac, hi_ac) = measure(sx, sy, sw, sh);
    println!(
        "[A] 亚克力**开启**（应用所设）: 过渡宽度 {:>6.1} px，对比度 {:>5.0}",
        w_ac,
        hi_ac - lo_ac
    );
    dump_profile(sx, sy, sw, sh, "亚克力开启");

    // 同一窗口上关掉系统背景做对照
    unsafe {
        let none = DWMSBT_NONE;
        let set = DwmSetWindowAttribute(
            target,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &none as *const _ as *const c_void,
            std::mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        );
        if set.is_err() {
            println!("（跨进程设置 DWM 属性失败，对照组不可用）");
        }
    }
    win::pump_for(800);
    let (w_none, lo_none, hi_none) = measure(sx, sy, sw, sh);
    println!(
        "[B] 亚克力**关闭**（同窗口）  : 过渡宽度 {:>6.1} px，对比度 {:>5.0}",
        w_none,
        hi_none - lo_none
    );
    dump_profile(sx, sy, sw, sh, "亚克力关闭");

    // 恢复
    unsafe {
        let ac = DWMSBT_TRANSIENTWINDOW;
        let _ = DwmSetWindowAttribute(
            target,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &ac as *const _ as *const c_void,
            std::mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        );
        let _ = DestroyWindow(bg);
    }

    let mut md = String::from("# R17 真实 Tauri 窗口的量化模糊量测\n\n");
    md.push_str("方法：锐利边缘法（左黑右白），量测 10%→90% 过渡宽度。\n\n");
    md.push_str("| 条件 | 过渡宽度(px) | 对比度 |\n|---|---|---|\n");
    md.push_str(&format!("| 基线（无遮挡） | {:.1} | {:.0} |\n", bw, bhi - blo));
    md.push_str(&format!(
        "| 真实 Tauri 窗口，亚克力开启 | {:.1} | {:.0} |\n",
        w_ac,
        hi_ac - lo_ac
    ));
    md.push_str(&format!(
        "| 同窗口，亚克力关闭 | {:.1} | {:.0} |\n",
        w_none,
        hi_none - lo_none
    ));

    let ok = w_ac > w_none * 3.0 && w_ac.is_finite();
    md.push_str(&format!(
        "\n**结论**：{}\n",
        if ok {
            "真实 Tauri 窗口上毛玻璃生效，且同一窗口关掉后边缘恢复锐利 —— 差异由亚克力造成。"
        } else {
            "未能证明毛玻璃在真实 Tauri 窗口上生效，需复查。"
        }
    ));

    let path = std::path::Path::new("out");
    let _ = std::fs::create_dir_all(path);
    let _ = std::fs::write(path.join("t1d-tauri-edge-result.md"), &md);

    println!(
        "\n判定：亚克力开启 {:.1} px vs 关闭 {:.1} px  =>  {}",
        w_ac,
        w_none,
        if ok { "毛玻璃生效 ✅" } else { "存疑 ⚠️" }
    );
    println!("报告已写入 probe/out/t1d-tauri-edge-result.md");
}

pub fn run() {
    println!("=== T1c 锐利边缘法：直接量测毛玻璃模糊半径 ===\n");

    unsafe {
        win::register_class("EdgeHalf", Some(half_proc), HBRUSH::default());
        win::register_class("EdgeGlass", Some(glass_proc), HBRUSH::default());
        win::register_class("EdgeHolder", Some(holder_proc), HBRUSH::default());
    }
    let half_cls = win::wide("EdgeHalf");
    let glass_cls = win::wide("EdgeGlass");
    let holder_cls = win::wide("EdgeHolder");

    // 背景：左黑右白，边缘在 x = bg_x + bg_w/2 = 200 + 500 = 700
    let bg_x = 200;
    let bg_y = 200;
    let bg_w = 1000;
    let bg_h = 500;
    let edge_x = bg_x + bg_w / 2;

    let bg = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(half_cls.as_ptr()),
            windows::core::w!("half"),
            WS_POPUP | WS_VISIBLE,
            bg_x,
            bg_y,
            bg_w,
            bg_h,
            None,
            None,
            Some(HINSTANCE(win::hmodule().0)),
            None,
        )
        .expect("创建背景窗口失败")
    };

    let holder = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(holder_cls.as_ptr()),
            windows::core::w!("FOCUS-HOLDER"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            1400,
            300,
            360,
            240,
            None,
            None,
            Some(HINSTANCE(win::hmodule().0)),
            None,
        )
        .expect("创建焦点持有窗口失败")
    };

    // 玻璃窗横跨边缘
    let g_x = edge_x - 300;
    let g_y = 320;
    let g_w = 600;
    let g_h = 260;
    // 采样区：横跨边缘的一条带状区域，两边各留 200px
    let sx = edge_x - 200;
    let sy = g_y + 40;
    let sw = 400;
    let sh = g_h - 80;

    win::pump_for(800);

    let (bw, blo, bhi) = measure(sx, sy, sw, sh);
    println!(
        "基线（无玻璃窗，边缘直接可见）: 过渡宽度 = {:.1} px，低/高电平 = {:.0}/{:.0}，对比度 = {:.0}\n",
        bw,
        blo,
        bhi,
        bhi - blo
    );

    let routes = [
        ("ctl-none", 0),
        ("ctl-opaque", 1),
        ("A-acrylic", 2),
        ("A-mica", 3),
        ("B-acrylic", 4),
    ];
    let exstyles = [
        ("plain", WINDOW_EX_STYLE(0)),
        ("layered", WS_EX_LAYERED),
        ("noredir", WS_EX_NOREDIRECTIONBITMAP),
    ];
    let focuses = [
        ("focused", 0u32),
        ("unfocused", 1),
        ("noactivate", 2),
    ];

    let mut md = String::from("# T1c 锐利边缘法：毛玻璃模糊量测\n\n");
    md.push_str(&format!(
        "基线（无玻璃窗）：过渡宽度 **{:.1} px**，对比度 {:.0}\n\n",
        bw,
        bhi - blo
    ));
    md.push_str("> 过渡宽度 ≈ 1-2px 表示边缘没被模糊；宽度明显变大表示真的模糊了。\n\n");
    md.push_str("| 路线 | 样式 | 焦点 | 过渡宽度(px) | 对比度 | 判定 |\n");
    md.push_str("|---|---|---|---|---|---|\n");

    for (rname, rcode) in routes {
        for (ename, ebase) in exstyles {
            for (fname, fcode) in focuses {
                OPAQUE_RED.store(if rcode == 1 { 1 } else { 0 }, Ordering::Relaxed);

                let mut ex = ebase | WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
                if fcode == 2 {
                    ex |= WS_EX_NOACTIVATE;
                }

                let hwnd = unsafe {
                    CreateWindowExW(
                        ex,
                        PCWSTR(glass_cls.as_ptr()),
                        windows::core::w!("glass"),
                        WS_POPUP | WS_VISIBLE,
                        g_x,
                        g_y,
                        g_w,
                        g_h,
                        None,
                        None,
                        Some(HINSTANCE(win::hmodule().0)),
                        None,
                    )
                };
                let hwnd = match hwnd {
                    Ok(h) => h,
                    Err(_) => continue,
                };

                match rcode {
                    2 => {
                        let _ = glass::apply_route_a_acrylic(hwnd);
                    }
                    3 => unsafe {
                        let v = DWMSBT_MAINWINDOW;
                        let _ = DwmSetWindowAttribute(
                            hwnd,
                            DWMWA_SYSTEMBACKDROP_TYPE,
                            &v as *const _ as *const c_void,
                            std::mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
                        );
                    },
                    4 => {
                        let _ = apply_route_b(hwnd);
                    }
                    _ => {}
                }

                if ename == "layered" {
                    unsafe {
                        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 225, LWA_ALPHA);
                    }
                }

                let intended = if fcode == 0 { hwnd } else { holder };
                win::force_foreground(intended);
                win::pump_for(500);

                let (w, lo, hi) = measure(sx, sy, sw, sh);
                let contrast = hi - lo;
                let verdict = if !w.is_finite() {
                    "边缘完全消失(强模糊或纯色)"
                } else if w <= 2.5 && contrast > 200.0 {
                    "无模糊"
                } else if w <= 2.5 {
                    "边缘锐利但对比度低"
                } else if w < 12.0 {
                    "轻微微模糊"
                } else if w < 60.0 {
                    "明显模糊"
                } else {
                    "强模糊"
                };

                let ws = if w.is_finite() {
                    format!("{:.1}", w)
                } else {
                    "inf".into()
                };
                println!(
                    "{:<12} {:<8} {:<11} width={:>6} contrast={:>6.0}  {}",
                    rname, ename, fname, ws, contrast, verdict
                );
                md.push_str(&format!(
                    "| {} | {} | {} | {} | {:.0} | **{}** |\n",
                    rname, ename, fname, ws, contrast, verdict
                ));

                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                win::pump_for(120);
            }
        }
    }

    unsafe {
        let _ = DestroyWindow(holder);
        let _ = DestroyWindow(bg);
    }

    let path = std::path::Path::new("out");
    let _ = std::fs::create_dir_all(path);
    let _ = std::fs::write(path.join("t1c-edge-result.md"), &md);
    println!("\n报告已写入 probe/out/t1c-edge-result.md");
}
