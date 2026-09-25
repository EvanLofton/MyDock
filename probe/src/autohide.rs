//! T3：双窗口自动隐藏的帧率/帧耗时实测
//!
//! 目标：量化「移动 Dock 窗口」这件事本身的开销，以及叠加毛玻璃后的额外开销。
//! 这是风险 R5（亚克力 + 高频窗口移动导致掉帧）的直接证据。
//!
//! 两个指标：
//!   1. 未限速：每次 SetWindowPos 的原始耗时（µs），最客观、与环境无关；
//!   2. 限速对齐刷新率：统计「迟到帧」，反映真实观感。

use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::glass;
use crate::win;

unsafe extern "system" fn strip_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn dock_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let _ = ValidateRect(Some(hwnd), None);
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[derive(Debug)]
struct Stats {
    avg: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
}

fn stats(mut v: Vec<f64>) -> Stats {
    if v.is_empty() {
        return Stats {
            avg: 0.0,
            p50: 0.0,
            p95: 0.0,
            p99: 0.0,
            max: 0.0,
        };
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    let pct = |p: f64| v[((n as f64 - 1.0) * p).round() as usize];
    Stats {
        avg: v.iter().sum::<f64>() / n as f64,
        p50: pct(0.50),
        p95: pct(0.95),
        p99: pct(0.99),
        max: v[n - 1],
    }
}

/// 未限速地连续移动窗口，返回每次调用的耗时（µs）
fn measure_unpaced(hwnd: HWND, x: i32, y_top: i32, y_bottom: i32, frames: usize) -> Stats {
    let mut samples = Vec::with_capacity(frames);
    for i in 0..frames {
        let t = (i % 60) as f64 / 60.0;
        // 三角波，模拟显示/隐藏往返
        let k = if t < 0.5 { t * 2.0 } else { 2.0 - t * 2.0 };
        let y = y_top + ((y_bottom - y_top) as f64 * k) as i32;

        let t0 = Instant::now();
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        samples.push(t0.elapsed().as_secs_f64() * 1e6);
        win::pump();
    }
    stats(samples)
}

/// 对齐刷新率移动窗口，返回每帧相对目标时刻的迟到量（µs）
fn measure_paced(hwnd: HWND, x: i32, y_top: i32, y_bottom: i32, frames: usize, hz: i32) -> Stats {
    let interval = Duration::from_secs_f64(1.0 / hz as f64);
    let mut samples = Vec::with_capacity(frames);
    let mut next = Instant::now();
    for i in 0..frames {
        next += interval;
        let t = (i % 60) as f64 / 60.0;
        let k = if t < 0.5 { t * 2.0 } else { 2.0 - t * 2.0 };
        let y = y_top + ((y_bottom - y_top) as f64 * k) as i32;

        // 忙等到目标时刻
        while Instant::now() < next {
            std::hint::spin_loop();
        }
        let target = next;
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        let late = Instant::now().saturating_duration_since(target);
        samples.push(late.as_secs_f64() * 1e6);
        win::pump();
    }
    stats(samples)
}

pub fn run() {
    println!("=== T3 双窗口自动隐藏帧率实测 ===\n");

    let hz = win::refresh_hz();
    println!("显示器刷新率: {} Hz", hz);
    let (sw, sh) = win::screen_size();
    let wa = win::work_area();
    println!(
        "屏幕 {}x{}，工作区 bottom={}（任务栏高度 {}）\n",
        sw,
        sh,
        wa.bottom,
        sh - wa.bottom
    );

    unsafe {
        win::register_class("ProbeStrip", Some(strip_proc), HBRUSH::default());
        win::register_class("ProbeDock3", Some(dock_proc), HBRUSH::default());
    }
    let strip_cls = win::wide("ProbeStrip");
    let dock_cls = win::wide("ProbeDock3");

    // 触发条：贴工作区底部 3px 高，横贯屏幕
    let trigger = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR(strip_cls.as_ptr()),
            windows::core::w!("trigger"),
            WS_POPUP | WS_VISIBLE,
            0,
            wa.bottom - 3,
            sw,
            3,
            None,
            None,
            Some(HINSTANCE(win::hmodule().0)),
            None,
        )
        .expect("创建触发条窗口失败")
    };

    let dock_w = 900;
    let dock_h = 90;
    let dock_x = (sw - dock_w) / 2;
    let shown_y = wa.bottom - dock_h; // 紧贴任务栏上方
    let hidden_y = wa.bottom + 20; // 完全藏到屏幕下方

    let dock = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR(dock_cls.as_ptr()),
            windows::core::w!("dock"),
            WS_POPUP | WS_VISIBLE,
            dock_x,
            shown_y,
            dock_w,
            dock_h,
            None,
            None,
            Some(HINSTANCE(win::hmodule().0)),
            None,
        )
        .expect("创建 Dock 窗口失败")
    };

    win::pump_for(400);

    let frames = 900;
    let mut md = String::from("# T3 双窗口自动隐藏帧率实测\n\n");
    md.push_str(&format!("- 显示器刷新率：**{} Hz**\n", hz));
    md.push_str(&format!("- 屏幕：{}x{}，工作区 bottom={}（任务栏高 {} px）\n", sw, sh, wa.bottom, sh - wa.bottom));
    md.push_str(&format!(
        "- Dock 窗口：{}x{}，移动范围 y = {} → {}\n\n",
        dock_w, dock_h, shown_y, hidden_y
    ));

    md.push_str("## 未限速：单次 SetWindowPos 原始耗时（µs）\n\n");
    md.push_str("| 毛玻璃 | avg | p50 | p95 | p99 | max |\n|---|---|---|---|---|---|\n");

    let phases: [(&str, fn(HWND)); 3] = [
        ("无", |_h| {}),
        ("路线A (SYSTEMBACKDROP_TYPE=Acrylic)", |h| {
            let _ = glass::apply_route_a_acrylic(h);
        }),
        ("路线B (ACCENT_ENABLE_ACRYLICBLURBEHIND)", |h| {
            let _ = glass::apply_route_b(h);
        }),
    ];

    let mut unpaced_rows: Vec<(String, Stats)> = Vec::new();
    let mut paced_rows: Vec<(String, Stats)> = Vec::new();

    for (name, apply) in phases {
        apply(dock);
        win::pump_for(300);

        let u = measure_unpaced(dock, dock_x, shown_y, hidden_y, frames);
        println!(
            "[未限速] {:<42} avg={:>7.1} p95={:>7.1} p99={:>7.1} max={:>8.1} µs",
            name, u.avg, u.p95, u.p99, u.max
        );
        md.push_str(&format!(
            "| {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |\n",
            name, u.avg, u.p50, u.p95, u.p99, u.max
        ));
        unpaced_rows.push((name.to_string(), u));

        let p = measure_paced(dock, dock_x, shown_y, hidden_y, frames, hz);
        let budget = 1e6 / hz as f64;
        let late_pct = if p.p95 > budget { "超预算" } else { "在预算内" };
        println!(
            "[限速 {:>3}Hz] {:<34} avg={:>7.1} p95={:>7.1} p99={:>7.1} max={:>8.1} µs ({})",
            hz, name, p.avg, p.p95, p.p99, p.max, late_pct
        );
        paced_rows.push((name.to_string(), p));
    }

    md.push_str(&format!(
        "\n## 限速对齐 {} Hz：每帧迟到量（µs，帧预算 {:.0} µs）\n\n",
        hz,
        1e6 / hz as f64
    ));
    md.push_str("| 毛玻璃 | avg | p50 | p95 | p99 | max |\n|---|---|---|---|---|---|\n");
    for (n, s) in &paced_rows {
        md.push_str(&format!(
            "| {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |\n",
            n, s.avg, s.p50, s.p95, s.p99, s.max
        ));
    }

    unsafe {
        let _ = DestroyWindow(dock);
        let _ = DestroyWindow(trigger);
    }

    let path = std::path::Path::new("out");
    let _ = std::fs::create_dir_all(path);
    let _ = std::fs::write(path.join("t3-autohide-result.md"), &md);
    println!("\n报告已写入 probe/out/t3-autohide-result.md");
}
