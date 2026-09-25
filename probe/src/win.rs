//! 通用 Win32 辅助：DPI、窗口类注册、消息泵、屏幕捕获、像素统计
#![allow(dead_code)]

use std::ffi::c_void;
use std::mem::size_of;
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn init_dpi() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn hmodule() -> HMODULE {
    unsafe { GetModuleHandleW(None).unwrap_or_default() }
}

/// 注册窗口类；返回原子（0 表示失败，若已注册可忽略）
pub fn register_class(name: &str, wndproc: WNDPROC, brush: HBRUSH) -> u16 {
    let name_w = wide(name);
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() };
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: wndproc,
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: HINSTANCE(hmodule().0),
        hIcon: HICON::default(),
        hCursor: cursor,
        hbrBackground: brush,
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(name_w.as_ptr()),
    };
    unsafe { RegisterClassW(&wc) }
}

/// 排空并派发所有待处理消息
pub fn pump() {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 在持续时间内反复泵消息
pub fn pump_for(ms: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        pump();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------- 屏幕捕获

pub struct Shot {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>, // BGRA, top-down
}

impl Shot {
    pub fn luma_at(&self, x: i32, y: i32) -> f64 {
        let i = ((y * self.w + x) * 4) as usize;
        if i + 2 >= self.px.len() {
            return 0.0;
        }
        let b = self.px[i] as f64;
        let g = self.px[i + 1] as f64;
        let r = self.px[i + 2] as f64;
        0.114 * b + 0.587 * g + 0.299 * r
    }

    /// 区域内亮度的均值与标准差
    pub fn stats(&self) -> (f64, f64) {
        let mut sum = 0.0;
        let mut sum2 = 0.0;
        let n = (self.w * self.h) as usize;
        for y in 0..self.h {
            for x in 0..self.w {
                let l = self.luma_at(x, y);
                sum += l;
                sum2 += l * l;
            }
        }
        let nf = n as f64;
        let mean = sum / nf;
        let var = (sum2 / nf) - mean * mean;
        (mean, var.max(0.0).sqrt())
    }
}

/// 抓取屏幕矩形（含 DWM 合成结果）
pub fn capture(x: i32, y: i32, w: i32, h: i32) -> Shot {
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        let _ = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY);

        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h; // 负数 = top-down
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = 0; // BI_RGB

        let mut buf = vec![0u8; (w * h * 4) as usize];
        GetDIBits(
            mem,
            bmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bi,
            DIB_RGB_COLORS,
        );

        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);

        Shot { w, h, px: buf }
    }
}

// ---------------------------------------------------------------- 几何 / 窗口

pub fn work_area() -> RECT {
    let mut rc = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rc as *mut _ as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    rc
}

pub fn window_rect(hwnd: HWND) -> RECT {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rc);
    }
    rc
}

pub fn screen_size() -> (i32, i32) {    unsafe {
        (
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
        )
    }
}

pub fn refresh_hz() -> i32 {
    unsafe {
        let dc = GetDC(None);
        let hz = GetDeviceCaps(Some(dc), VREFRESH);
        ReleaseDC(None, dc);
        hz
    }
}

pub fn foreground_title() -> String {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return "<none>".into();
        }
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

pub fn window_title(hwnd: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// "类名:标题"，用于诊断焦点到底落在哪个窗口
pub fn describe_window(hwnd: HWND) -> String {
    unsafe {
        if hwnd.0.is_null() {
            return "<null>".into();
        }
        let mut cls = [0u16; 128];
        let n = GetClassNameW(hwnd, &mut cls);
        let class = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
        let title = window_title(hwnd);
        if title.is_empty() {
            format!("#{class}")
        } else {
            let t: String = title.chars().take(28).collect();
            format!("#{class}:{t}")
        }
    }
}

/// 强制把焦点交给 target。
/// 关键：Windows 有前台锁定，后台进程直接 SetForegroundWindow 会被静默忽略，
/// 必须先用 AttachThreadInput 附加到当前前台线程。
pub fn force_foreground(target: HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    unsafe {
        let fg = GetForegroundWindow();
        let fg_tid = if fg.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(fg, None)
        };
        let our_tid = GetCurrentThreadId();
        let attached = fg_tid != 0 && fg_tid != our_tid;
        if attached {
            let _ = AttachThreadInput(fg_tid, our_tid, true);
        }
        let _ = ShowWindow(target, SW_SHOW);
        let _ = BringWindowToTop(target);
        let _ = SetForegroundWindow(target);
        if attached {
            let _ = AttachThreadInput(fg_tid, our_tid, false);
        }
    }
}
