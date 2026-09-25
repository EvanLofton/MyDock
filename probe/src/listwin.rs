//! 列出顶层窗口，用于确认 Tauri 窗口是否真的创建成功、样式如何。
//! 用法: probe listwin [标题或类名子串]

use std::sync::Mutex;

use windows::core::BOOL;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::win;

static FILTER: Mutex<Option<String>> = Mutex::new(None);
static ROWS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn ex_style_names(ex: u32) -> String {
    let known: [(u32, &str); 9] = [
        (WS_EX_NOACTIVATE.0, "NOACTIVATE"),
        (WS_EX_LAYERED.0, "LAYERED"),
        (WS_EX_NOREDIRECTIONBITMAP.0, "NOREDIRECTIONBITMAP"),
        (WS_EX_TOOLWINDOW.0, "TOOLWINDOW"),
        (WS_EX_TOPMOST.0, "TOPMOST"),
        (WS_EX_APPWINDOW.0, "APPWINDOW"),
        (WS_EX_TRANSPARENT.0, "TRANSPARENT"),
        (WS_EX_ACCEPTFILES.0, "ACCEPTFILES"),
        (WS_EX_COMPOSITED.0, "COMPOSITED"),
    ];
    let mut v: Vec<&str> = Vec::new();
    for (bit, name) in known {
        if ex & bit != 0 {
            v.push(name);
        }
    }
    v.join("|")
}

fn style_names(st: u32) -> String {
    let known: [(u32, &str); 7] = [
        (WS_POPUP.0, "POPUP"),
        (WS_CAPTION.0, "CAPTION"),
        (WS_THICKFRAME.0, "THICKFRAME"),
        (WS_SYSMENU.0, "SYSMENU"),
        (WS_MINIMIZEBOX.0, "MINBOX"),
        (WS_MAXIMIZEBOX.0, "MAXBOX"),
        (WS_VISIBLE.0, "VISIBLE"),
    ];
    let mut v: Vec<&str> = Vec::new();
    for (bit, name) in known {
        if st & bit != 0 {
            v.push(name);
        }
    }
    v.join("|")
}

unsafe extern "system" fn enum_cb(hwnd: HWND, _lp: LPARAM) -> BOOL {
    let title = win::window_title(hwnd);
    let mut cls = [0u16; 256];
    let n = GetClassNameW(hwnd, &mut cls);
    let class = String::from_utf16_lossy(&cls[..n.max(0) as usize]);

    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));

    let filter = FILTER.lock().unwrap().clone();
    let matched = match &filter {
        None => true,
        Some(f) => {
            // 纯数字视为 pid
            if let Ok(want) = f.parse::<u32>() {
                pid == want
            } else {
                title.to_lowercase().contains(f) || class.to_lowercase().contains(f)
            }
        }
    };

    if matched {
        let mut rc = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rc);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let st = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let visible = IsWindowVisible(hwnd).as_bool();

        ROWS.lock().unwrap().push(format!(
            "hwnd=0x{:>8X} pid={:<7} vis={:<5} rect=({},{}) {}x{}\n    class='{}'\n    title='{}'\n    style  : {}\n    exstyle: {}\n",
            hwnd.0 as usize,
            pid,
            visible,
            rc.left,
            rc.top,
            rc.right - rc.left,
            rc.bottom - rc.top,
            class,
            title,
            style_names(st),
            ex_style_names(ex),
        ));
    }
    TRUE
}

pub fn run(filter: Option<String>) {
    *FILTER.lock().unwrap() = filter;
    ROWS.lock().unwrap().clear();

    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(0));
    }

    let rows = ROWS.lock().unwrap();
    if rows.is_empty() {
        println!("（没有匹配的顶层窗口）");
    } else {
        println!("匹配到 {} 个顶层窗口：\n", rows.len());
        for r in rows.iter() {
            println!("{r}");
        }
    }
}
