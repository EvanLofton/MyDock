//! Dock 项目 P0 可行性验证 probe
//!
//! 子命令：
//!   env         环境与系统能力报告
//!   glass       T1 毛玻璃路线矩阵实测（自动化像素判定）
//!   noactivate  T2 WS_EX_NOACTIVATE 点击不夺焦点验证
//!   autohide    T3 双窗口自动隐藏帧率实测
//!   all         依次运行 T1/T2/T3

#![allow(unsafe_op_in_unsafe_fn)] // probe 为一次性验证工具，放宽 edition 2024 的 unsafe 块检查

mod autohide;
mod edge;
mod glass;
mod inspect;
mod listwin;
mod noactivate;
mod win;

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CMONITORS};

fn reg_str(subkey: &str, value: &str) -> Option<String> {
    let sk = win::wide(subkey);
    let v = win::wide(value);
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(sk.as_ptr()),
            PCWSTR(v.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    };
    if rc.0 != 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

fn env_report() {
    println!("=== 环境与系统能力报告 ===\n");

    let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    println!("ProductName     : {:?}", reg_str(key, "ProductName"));
    println!("DisplayVersion  : {:?}", reg_str(key, "DisplayVersion"));
    println!("CurrentBuild    : {:?}", reg_str(key, "CurrentBuildNumber"));
    println!("UBR             : {:?}", reg_str(key, "UBR"));

    let (w, h) = win::screen_size();
    println!("主屏分辨率       : {} x {}", w, h);
    println!("刷新率           : {} Hz", win::refresh_hz());
    println!(
        "显示器数量       : {}",
        unsafe { GetSystemMetrics(SM_CMONITORS) }
    );

    let wa = win::work_area();
    println!(
        "工作区           : ({},{})-({},{})  → 任务栏高度 {} px",
        wa.left,
        wa.top,
        wa.right,
        wa.bottom,
        h - wa.bottom
    );

    unsafe {
        match DwmIsCompositionEnabled() {
            Ok(b) => println!("DWM 合成         : {}", b.as_bool()),
            Err(e) => println!("DWM 合成         : 查询失败 {e}"),
        }
    }

    let hz = win::refresh_hz();
    println!(
        "\n帧预算           : {:.0} µs（{} Hz）",
        1e6 / hz.max(1) as f64,
        hz
    );
    println!("\n提示：T1 会创建多个窗口并抓屏，请勿移动鼠标或切换窗口。");
}

unsafe extern "system" fn external_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wp: windows::Win32::Foundation::WPARAM,
    lp: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::*;
    match msg {
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 子进程模式：只创建一个具名窗口并跑消息循环，
/// 供 T2 作为「另一个进程」的焦点目标使用。
fn hold_window() {
    use windows::Win32::Foundation::{HINSTANCE, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;

    unsafe {
        win::register_class("ProbeExternalHolder", Some(external_proc), HBRUSH::default());
    }
    let cls = win::wide("ProbeExternalHolder");
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(cls.as_ptr()),
            windows::core::w!("EXTERNAL-HOLDER"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            150,
            150,
            420,
            280,
            None,
            None,
            Some(HINSTANCE(win::hmodule().0)),
            None,
        )
    };
    if hwnd.is_err() {
        eprintln!("holdwin: 创建窗口失败");
        std::process::exit(1);
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn main() {    win::init_dpi();

    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("env");

    match cmd {
        "env" => env_report(),
        "glass" => glass::run(),
        "edge" => edge::run(),
        "edgeext" => edge::run_external(
            args.get(2).map(|s| s.as_str()).unwrap_or("Tauri Window"),
            args.get(3).map(|s| s.as_str()).unwrap_or("Dock"),
        ),
        "holdwin" => hold_window(),
        "cursor" => {
            let x: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            let y: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            unsafe {
                match windows::Win32::UI::WindowsAndMessaging::SetCursorPos(x, y) {
                    Ok(()) => println!("光标已移动到 ({x},{y})"),
                    Err(e) => println!("移动光标失败: {e}"),
                }
            }
        }
        "listwin" => listwin::run(args.get(2).map(|s| s.to_lowercase())),
        "inspect" => inspect::run(
            args.get(2).map(|s| s.as_str()).unwrap_or("Tauri Window"),
            args.get(3).map(|s| s.as_str()).unwrap_or("Dock"),
            args.get(4).and_then(|s| s.parse().ok()).unwrap_or(18),
            args.get(5).and_then(|s| s.parse().ok()).unwrap_or(5),
        ),
        "noactivate" => noactivate::run(),
        "autohide" => autohide::run(),
        "all" => {
            env_report();
            println!("\n{}\n", "=".repeat(70));
            glass::run();
            println!("\n{}\n", "=".repeat(70));
            noactivate::run();
            println!("\n{}\n", "=".repeat(70));
            autohide::run();
        }
        other => {
            eprintln!("未知子命令: {other}");
            eprintln!("用法: probe [env|glass|noactivate|autohide|all]");
            std::process::exit(2);
        }
    }
}
