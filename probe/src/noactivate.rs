//! T2：WS_EX_NOACTIVATE 点击不夺焦点 —— 全自动验证
//!
//! 判定：先把焦点给「焦点持有窗口」，再用 SendInput 真实点击 Dock 窗口，
//! 然后看焦点是否仍留在持有窗口。
//! 同时统计 Dock 窗口是否真的收到了点击（区分「没抢焦点」和「点击压根没送达」）。

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::win;

static HANDLE_MOUSEACTIVATE: AtomicU32 = AtomicU32::new(0);
static CLICKS: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn dock_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            CLICKS.fetch_add(1, Ordering::SeqCst);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE if HANDLE_MOUSEACTIVATE.load(Ordering::SeqCst) == 1 => {
            LRESULT(MA_NOACTIVATE as isize)
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

unsafe fn click_at(x: i32, y: i32) {
    let _ = SetCursorPos(x, y);
    std::thread::sleep(Duration::from_millis(80));
    let mk = |flags: MOUSE_EVENT_FLAGS| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [mk(MOUSEEVENTF_LEFTDOWN), mk(MOUSEEVENTF_LEFTUP)];
    SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
}

struct Case {
    name: &'static str,
    noactivate: bool,
    handle_ma: bool,
    /// true = 通过真实点击 holder 来设置焦点（最贴近用户真实操作）
    focus_by_click: bool,
    /// true = 焦点目标换成另一个进程（记事本），排除"同进程可自由激活"的干扰
    notepad: bool,
    expect_steal: bool,
}

pub fn run() {
    println!("=== T2 WS_EX_NOACTIVATE 点击不夺焦点验证 ===\n");

    unsafe {
        win::register_class("ProbeDock", Some(dock_proc), HBRUSH::default());
        win::register_class("ProbeHolder2", Some(holder_proc), HBRUSH::default());
    }
    let dock_cls = win::wide("ProbeDock");
    let holder_cls = win::wide("ProbeHolder2");

    let holder = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(holder_cls.as_ptr()),
            windows::core::w!("FOCUS-HOLDER"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            150,
            150,
            320,
            200,
            None,
            None,
            Some(HINSTANCE(win::hmodule().0)),
            None,
        )
        .expect("创建焦点持有窗口失败")
    };

    let dock_x = 700;
    let dock_y = 500;
    let dock_w = 600;
    let dock_h = 90;

    let cases = [
        Case {
            name: "V1 NOACTIVATE",
            noactivate: true,
            handle_ma: false,
            focus_by_click: false,
            notepad: false,
            expect_steal: false,
        },
        Case {
            name: "V1b NOACTIVATE (click-focus)",
            noactivate: true,
            handle_ma: false,
            focus_by_click: true,
            notepad: false,
            expect_steal: false,
        },
        Case {
            name: "V2 NOACTIVATE+MA_NOACTIVATE",
            noactivate: true,
            handle_ma: true,
            focus_by_click: false,
            notepad: false,
            expect_steal: false,
        },
        Case {
            name: "V2b NOACT+MA (click-focus)",
            noactivate: true,
            handle_ma: true,
            focus_by_click: true,
            notepad: false,
            expect_steal: false,
        },
        Case {
            name: "V4 MA_NOACTIVATE only",
            noactivate: false,
            handle_ma: true,
            focus_by_click: true,
            notepad: false,
            expect_steal: false,
        },
        Case {
            name: "V3 control (no protection)",
            noactivate: false,
            handle_ma: false,
            focus_by_click: true,
            notepad: false,
            expect_steal: true,
        },
        // ---- 跨进程组：焦点目标是另一个进程（记事本），排除"同进程可自由激活"的干扰
        Case {
            name: "V5 NOACTIVATE (other proc)",
            noactivate: true,
            handle_ma: false,
            focus_by_click: false,
            notepad: true,
            expect_steal: false,
        },
        Case {
            name: "V6 NOACT+MA (other proc)",
            noactivate: true,
            handle_ma: true,
            focus_by_click: false,
            notepad: true,
            expect_steal: false,
        },
        Case {
            name: "V7 control (other proc)",
            noactivate: false,
            handle_ma: false,
            focus_by_click: false,
            notepad: true,
            expect_steal: true,
        },
    ];

    let mut md = String::from("# T2 WS_EX_NOACTIVATE 点击不夺焦点验证\n\n");
    md.push_str("| 变体 | 点击前焦点 | 点击后焦点 | 焦点是否被夺 | Dock 是否收到点击 | 判定 |\n");
    md.push_str("|---|---|---|---|---|---|\n");

    let mut orig = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut orig);
    }

    // 起一个 probe 子进程作为「另一个进程」的焦点目标，
    // 用于排除"同进程窗口可自由互相激活"对结论的干扰
    let mut ext_child = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            std::process::Command::new(exe)
                .arg("holdwin")
                .spawn()
                .ok()
        });
    std::thread::sleep(Duration::from_millis(1400));
    win::pump_for(300);
    let ext_win = unsafe {
        FindWindowW(windows::core::w!("ProbeExternalHolder"), None)
    }
    .unwrap_or_default();
    println!("跨进程焦点目标: {}\n", win::describe_window(ext_win));

    let mut all_pass = true;

    for c in &cases {
        HANDLE_MOUSEACTIVATE.store(if c.handle_ma { 1 } else { 0 }, Ordering::SeqCst);
        CLICKS.store(0, Ordering::SeqCst);

        let mut ex = WINDOW_EX_STYLE(0) | WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
        if c.noactivate {
            ex |= WS_EX_NOACTIVATE;
        }

        let dock = unsafe {
            CreateWindowExW(
                ex,
                PCWSTR(dock_cls.as_ptr()),
                windows::core::w!("DOCK"),
                WS_POPUP | WS_VISIBLE,
                dock_x,
                dock_y,
                dock_w,
                dock_h,
                None,
                None,
                Some(HINSTANCE(win::hmodule().0)),
                None,
            )
        };

        let dock = match dock {
            Ok(h) => h,
            Err(e) => {
                println!("{} 创建失败: {e}", c.name);
                continue;
            }
        };

        // 回读实际生效的扩展样式，排除"样式根本没设上"这种可能
        let ex_read = unsafe { GetWindowLongPtrW(dock, GWL_EXSTYLE) } as u32;
        let noact_actual = (ex_read & WS_EX_NOACTIVATE.0) != 0;

        // 解析焦点目标：另一进程的 probe 窗口，或同进程 holder
        let target = if c.notepad {
            unsafe { FindWindowW(windows::core::w!("ProbeExternalHolder"), None) }
                .unwrap_or_default()
        } else {
            holder
        };
        let want = if c.notepad { "EXTERNAL-HOLDER" } else { "FOCUS-HOLDER" };

        if target.0.is_null() {
            println!("{:<30} 未找到目标窗口，跳过", c.name);
            continue;
        }

        // 设置焦点并确认。两条路径：
        //  - focus_by_click: 真实点击 holder（最贴近用户实际操作）
        //  - 否则用 force_foreground（force 里含 AttachThreadInput，绕开前台锁定）
        if c.focus_by_click {
            unsafe {
                click_at(150 + 160, 150 + 100); // holder 中心
            }
        } else {
            win::force_foreground(target);
        }
        win::pump_for(400);
        let before = win::describe_window(unsafe { GetForegroundWindow() });
        let focus_ready = before.contains(want);

        // 真实点击 Dock 中心
        unsafe {
            click_at(dock_x + dock_w / 2, dock_y + dock_h / 2);
        }
        win::pump_for(400);

        let after = win::describe_window(unsafe { GetForegroundWindow() });
        let clicks = CLICKS.load(Ordering::SeqCst);
        let stolen = !after.contains(want);

        let pass = if !focus_ready {
            false
        } else if c.expect_steal {
            stolen // 对照组必须被夺焦点，否则说明测试无效
        } else {
            !stolen && clicks > 0
        };
        if !pass {
            all_pass = false;
        }

        println!(
            "{:<30} exNOACT={:<5} before={:<22} after={:<22} stolen={:<5} clicks={} -> {}",
            c.name,
            noact_actual,
            before,
            after,
            stolen,
            clicks,
            if pass {
                "PASS"
            } else if !focus_ready {
                "INVALID(焦点未就位)"
            } else {
                "FAIL"
            }
        );

        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | **{}** |\n",
            c.name,
            before,
            after,
            stolen,
            clicks,
            if pass { "PASS" } else { "FAIL" }
        ));

        unsafe {
            let _ = DestroyWindow(dock);
        }
        win::pump_for(150);
    }

    unsafe {
        let _ = SetCursorPos(orig.x, orig.y);
        let _ = DestroyWindow(holder);
        let ext = FindWindowW(windows::core::w!("ProbeExternalHolder"), None).unwrap_or_default();
        if !ext.0.is_null() {
            let _ = PostMessageW(Some(ext), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    if let Some(ch) = ext_child.as_mut() {
        let _ = ch.kill();
    }

    md.push_str(&format!(
        "\n**总体结论：{}**\n",
        if all_pass {
            "全部符合预期"
        } else {
            "存在不符合预期的用例，需人工复查"
        }
    ));

    let path = std::path::Path::new("out");
    let _ = std::fs::create_dir_all(path);
    let _ = std::fs::write(path.join("t2-noactivate-result.md"), &md);
    println!("\n报告已写入 probe/out/t2-noactivate-result.md");
}
