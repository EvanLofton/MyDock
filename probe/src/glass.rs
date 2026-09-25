//! T1：毛玻璃路线矩阵实测（v2：修正聚焦控制与背景遮挡）
//!
//! 问题：Windows 在窗口失去焦点时会关闭 acrylic 模糊，而我们的 Dock 是
//! WS_EX_NOACTIVATE、将永远处于失焦状态。因此必须测「失焦」列。
//!
//! v2 修正：
//!   1. 聚焦用 AttachThreadInput 强制，并**校验焦点是否真的转移**，未转移标记 INVALID；
//!   2. 棋盘背景窗改为 TOPMOST（先创建），保证玻璃窗一定叠在棋盘上方；
//!   3. 新增 A+extend 路线（DwmExtendFrameIntoClientArea），以公平检验路线 A。
//!
//! 判定方式：抓屏后用像素统计区分
//!   - 图案依旧清晰      → NO_EFFECT（完全没有玻璃）
//!   - 图案被抹平、亮度不变 → BLUR（真的是模糊，正是我们要的）
//!   - 图案被抹平、亮度大变 → TINT_ONLY（只是一层不透明/纯色罩）

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::win;

// ------------------------------------------------ 未公开 API：SetWindowCompositionAttribute

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    gradient_color: u32, // 0xAABBGGRR
    animation_id: u32,
}

#[repr(C)]
struct WindowCompositionAttributeData {
    attrib: u32,
    pv_data: *mut c_void,
    cb_data: usize,
}

const WCA_ACCENT_POLICY: u32 = 0x13;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

type SwcaFn = unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> BOOL;

/// 关键：SetWindowCompositionAttribute 虽是 user32.dll 的导出（序号 2391），
/// 但**不在 Windows SDK 的 user32.lib 导入库里**，静态链接会报 LNK2019。
/// 必须 LoadLibrary + GetProcAddress 动态取地址（window-vibrancy 也如此）。
fn swca() -> Option<SwcaFn> {
    use windows::core::s;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
    unsafe {
        let module = LoadLibraryA(s!("user32.dll")).ok()?;
        let addr = GetProcAddress(module, s!("SetWindowCompositionAttribute"))?;
        Some(std::mem::transmute::<_, SwcaFn>(addr))
    }
}

/// 路线 B：传统亚克力（ACCENT_ENABLE_ACRYLICBLURBEHIND）
pub fn apply_route_b(hwnd: HWND) -> String {
    let Some(f) = swca() else {
        return "swca:NOT_FOUND".to_string();
    };
    // 深色半透明：R=0x09 G=0x09 B=0x0B A=0xB4(180)，打包为 0xAABBGGRR
    let mut a: u32 = 0xB4;
    if a == 0 {
        a = 1; // acrylic 不接受 alpha = 0
    }
    let mut policy = AccentPolicy {
        accent_state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
        accent_flags: 0, // acrylic 用 0；ACCENT_ENABLE_BLURBEHIND 才用 2
        gradient_color: 0x09 | (0x09 << 8) | (0x0B << 16) | (a << 24),
        animation_id: 0,
    };
    let mut data = WindowCompositionAttributeData {
        attrib: WCA_ACCENT_POLICY,
        pv_data: &mut policy as *mut _ as *mut c_void,
        cb_data: std::mem::size_of::<AccentPolicy>(),
    };
    let r = unsafe { f(hwnd, &mut data) };
    format!("swca:{}", r.0)
}

/// 路线 A：官方 DWM 系统背景（Acrylic）
pub fn apply_route_a_acrylic(hwnd: HWND) -> windows::core::Result<()> {
    let v = DWMSBT_TRANSIENTWINDOW;
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &v as *const _ as *const c_void,
            std::mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        )
    }
}

fn set_backdrop(hwnd: HWND, v: DWM_SYSTEMBACKDROP_TYPE) -> String {
    unsafe {
        match DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &v as *const _ as *const c_void,
            std::mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        ) {
            Ok(()) => "dwm:ok".to_string(),
            Err(e) => format!("dwm:ERR{:x}", e.code().0),
        }
    }
}

// ------------------------------------------------ 窗口过程

/// 玻璃窗口绘制模式：0 = 完全透明（不绘制），1 = 不透明红（捕获自检用）
static GLASS_MODE: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn glass_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1), // 不擦除背景，保留下层
        WM_PAINT => {
            if GLASS_MODE.load(Ordering::Relaxed) == 1 {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let brush = CreateSolidBrush(COLORREF(0x0000_00FF)); // 纯红，亮度 76.2
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

/// 背景棋盘格窗口：高频黑白格（8px），便于用方差判断是否被模糊
unsafe extern "system" fn checker_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let _ = PatBlt(hdc, 0, 0, rc.right, rc.bottom, WHITENESS);
            let cell = 8;
            let mut y = 0;
            while y < rc.bottom {
                let mut x = ((y / cell) % 2) * cell;
                while x < rc.right {
                    let _ = PatBlt(hdc, x, y, cell, cell, BLACKNESS);
                    x += cell * 2;
                }
                y += cell;
            }
            let _ = EndPaint(hwnd, &ps);
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

// ------------------------------------------------ 测试矩阵

#[derive(Clone, Copy, PartialEq)]
enum Route {
    CtlOpaque,
    CtlNone,
    AAcrylic,
    AMica,
    AAcrylicExtend,
    BAcrylic,
}

#[derive(Clone, Copy, PartialEq)]
enum ExStyleKind {
    Plain,
    Layered,
    NoRedir,
}

#[derive(Clone, Copy, PartialEq)]
enum FocusKind {
    Focused,
    Unfocused,
    NoActivate,
}

impl Route {
    fn name(&self) -> &'static str {
        match self {
            Route::CtlOpaque => "ctl-opaque",
            Route::CtlNone => "ctl-none",
            Route::AAcrylic => "A-acrylic",
            Route::AMica => "A-mica",
            Route::AAcrylicExtend => "A-acrylic+extend",
            Route::BAcrylic => "B-acrylic",
        }
    }
}

impl ExStyleKind {
    fn name(&self) -> &'static str {
        match self {
            ExStyleKind::Plain => "plain",
            ExStyleKind::Layered => "layered",
            ExStyleKind::NoRedir => "noredir",
        }
    }
}

impl FocusKind {
    fn name(&self) -> &'static str {
        match self {
            FocusKind::Focused => "focused",
            FocusKind::Unfocused => "unfocused",
            FocusKind::NoActivate => "noactivate",
        }
    }
}

struct Verdict {
    label: String,
    mean: f64,
    std: f64,
    ratio: f64,
    class: String,
    api: String,
    focus_ok: bool,
    focus_actual: String,
}

fn classify(mean: f64, std: f64, base_mean: f64, base_std: f64) -> (&'static str, f64) {
    let ratio = if base_std > 0.5 { std / base_std } else { 1.0 };
    let dmean = (mean - base_mean).abs();

    if base_std < 0.5 {
        return ("CAPTURE_FAIL", ratio);
    }
    if std < 0.5 {
        if dmean < 22.0 {
            return ("BLUR_PERFECT", ratio);
        }
        return ("TINT_OPAQUE", ratio);
    }
    if ratio < 0.45 && dmean < 30.0 {
        ("BLUR", ratio)
    } else if ratio < 0.45 {
        ("TINT_MIXED", ratio)
    } else if ratio > 0.80 {
        ("NO_EFFECT", ratio)
    } else {
        ("PARTIAL", ratio)
    }
}

pub fn run() {
    println!("=== T1 毛玻璃路线矩阵实测（v2）===\n");

    unsafe {
        win::register_class("ProbeChecker", Some(checker_proc), HBRUSH::default());
        win::register_class("ProbeGlass", Some(glass_proc), HBRUSH::default());
        win::register_class("ProbeHolder", Some(holder_proc), HBRUSH::default());
    }

    let checker_cls = win::wide("ProbeChecker");
    let glass_cls = win::wide("ProbeGlass");
    let holder_cls = win::wide("ProbeHolder");

    let bg_x = 200;
    let bg_y = 200;
    let bg_w = 1000;
    let bg_h = 500;

    // 背景棋盘窗：TOPMOST + 最先创建 → 后续的玻璃窗（同为 TOPMOST，后创建）必然在其上方
    let checker = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(checker_cls.as_ptr()),
            windows::core::w!("checker"),
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
        .expect("创建背景棋盘窗口失败")
    };

    // 焦点持有窗：放在棋盘之外的空闲区域，避免被遮挡
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

    let g_x = 400;
    let g_y = 320;
    let g_w = 600;
    let g_h = 260;
    let inset = 30;
    let sx = g_x + inset;
    let sy = g_y + inset;
    let sw = g_w - inset * 2;
    let sh = g_h - inset * 2;

    win::pump_for(800);

    let base = win::capture(sx, sy, sw, sh);
    let (base_mean, base_std) = base.stats();
    println!("基线（棋盘格可见）: mean={:.1} std={:.1}\n", base_mean, base_std);

    let routes = [
        Route::CtlOpaque,
        Route::CtlNone,
        Route::AAcrylic,
        Route::AMica,
        Route::AAcrylicExtend,
        Route::BAcrylic,
    ];
    let exstyles = [ExStyleKind::Plain, ExStyleKind::Layered, ExStyleKind::NoRedir];
    let focuses = [FocusKind::Focused, FocusKind::Unfocused, FocusKind::NoActivate];

    let mut results: Vec<Verdict> = Vec::new();

    for &route in &routes {
        for &ex in &exstyles {
            for &focus in &focuses {
                let mut ex_style = WINDOW_EX_STYLE(0);
                match ex {
                    ExStyleKind::Plain => {}
                    ExStyleKind::Layered => ex_style |= WS_EX_LAYERED,
                    ExStyleKind::NoRedir => ex_style |= WS_EX_NOREDIRECTIONBITMAP,
                }
                if focus == FocusKind::NoActivate {
                    ex_style |= WS_EX_NOACTIVATE;
                }
                ex_style |= WS_EX_TOPMOST | WS_EX_TOOLWINDOW;

                GLASS_MODE.store(
                    if route == Route::CtlOpaque { 1 } else { 0 },
                    Ordering::Relaxed,
                );

                let hwnd = unsafe {
                    CreateWindowExW(
                        ex_style,
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
                    Err(e) => {
                        results.push(Verdict {
                            label: format!("{}/{}/{}", route.name(), ex.name(), focus.name()),
                            mean: 0.0,
                            std: 0.0,
                            ratio: 0.0,
                            class: "CREATE_FAIL".into(),
                            api: format!("{e}"),
                            focus_ok: false,
                            focus_actual: String::new(),
                        });
                        continue;
                    }
                };

                // ---- 应用路线
                let api = match route {
                    Route::CtlOpaque | Route::CtlNone => "n/a".to_string(),
                    Route::AAcrylic => set_backdrop(hwnd, DWMSBT_TRANSIENTWINDOW),
                    Route::AMica => set_backdrop(hwnd, DWMSBT_MAINWINDOW),
                    Route::AAcrylicExtend => unsafe {
                        // -1 边距 = 把整个客户区交给 DWM 作为玻璃合成
                        let m = MARGINS {
                            cxLeftWidth: -1,
                            cxRightWidth: -1,
                            cyTopHeight: -1,
                            cyBottomHeight: -1,
                        };
                        let ext = match DwmExtendFrameIntoClientArea(hwnd, &m) {
                            Ok(()) => "extend:ok",
                            Err(_) => "extend:ERR",
                        };
                        format!("{}+{}", ext, set_backdrop(hwnd, DWMSBT_TRANSIENTWINDOW))
                    },
                    Route::BAcrylic => apply_route_b(hwnd),
                };

                if ex == ExStyleKind::Layered {
                    unsafe {
                        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 225, LWA_ALPHA);
                    }
                }

                // ---- 焦点状态（强制 + 校验）
                let intended = match focus {
                    FocusKind::Focused => hwnd,
                    FocusKind::Unfocused | FocusKind::NoActivate => holder,
                };
                win::force_foreground(intended);
                win::pump_for(550);

                let actual = unsafe { GetForegroundWindow() };
                let focus_ok = actual == intended;
                let focus_actual = win::describe_window(actual);

                let shot = win::capture(sx, sy, sw, sh);
                let (mean, std) = shot.stats();
                let (mut class, ratio) = classify(mean, std, base_mean, base_std);
                if !focus_ok {
                    class = "INVALID_FOCUS";
                }

                let label = format!("{}/{}/{}", route.name(), ex.name(), focus.name());
                println!(
                    "{:<30} mean={:>6.1} std={:>6.1} r={:>5.2} {:<14} {:<22} fg={} {}",
                    label,
                    mean,
                    std,
                    ratio,
                    class,
                    api,
                    focus_actual,
                    if focus_ok { "" } else { "<<FOCUS FAIL" }
                );

                results.push(Verdict {
                    label,
                    mean,
                    std,
                    ratio,
                    class: class.to_string(),
                    api,
                    focus_ok,
                    focus_actual,
                });

                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                win::pump_for(150);
            }
        }
    }

    unsafe {
        let _ = DestroyWindow(holder);
        let _ = DestroyWindow(checker);
    }

    write_report(&results, base_mean, base_std);
}

fn write_report(results: &[Verdict], base_mean: f64, base_std: f64) {
    let mut md = String::new();
    md.push_str("# T1 毛玻璃路线矩阵实测结果（v2）\n\n");
    md.push_str(&format!(
        "基线（棋盘格直接可见）：mean = {:.1}，std = {:.1}\n\n",
        base_mean, base_std
    ));
    md.push_str("## 判定口径\n\n");
    md.push_str("| 分类 | 含义 |\n|---|---|\n");
    md.push_str("| `BLUR_PERFECT` | 图案被完全抹平且亮度不变 —— 真实模糊 |\n");
    md.push_str("| `BLUR` | 图案被显著抹平且亮度基本不变 —— 真实模糊 |\n");
    md.push_str("| `TINT_OPAQUE` / `TINT_MIXED` | 图案被抹平但亮度大变 —— 只是罩了一层色，不是模糊 |\n");
    md.push_str("| `NO_EFFECT` | 图案依旧清晰 —— 完全没有玻璃 |\n");
    md.push_str("| `PARTIAL` | 部分弱化 |\n");
    md.push_str("| `INVALID_FOCUS` | 焦点未能按预期转移，该行结论不可用 |\n\n");

    md.push_str("## 全部结果\n\n");
    md.push_str("| 路线 | 窗口样式 | 焦点 | mean | std | std/基线 | 判定 | API | 实际前台窗口 |\n");
    md.push_str("|---|---|---|---|---|---|---|---|---|\n");
    for r in results {
        let p: Vec<&str> = r.label.split('/').collect();
        md.push_str(&format!(
            "| {} | {} | {} | {:.1} | {:.1} | {:.2} | **{}** | {} | `{}` |\n",
            p[0], p[1], p[2], r.mean, r.std, r.ratio, r.class, r.api, r.focus_actual
        ));
    }

    md.push_str("\n## 关键子集：失焦 / 不可激活（Dock 的真实工况）\n\n");
    md.push_str("| 路线 | 样式 | 焦点 | 判定 |\n|---|---|---|---|\n");
    for r in results {
        let p: Vec<&str> = r.label.split('/').collect();
        if p[2] != "focused" {
            md.push_str(&format!(
                "| {} | {} | {} | **{}** |\n",
                p[0], p[1], p[2], r.class
            ));
        }
    }

    let ok = results.iter().filter(|r| r.focus_ok).count();
    md.push_str(&format!(
        "\n> 焦点控制成功 {}/{} 行。\n",
        ok,
        results.len()
    ));

    let path = std::path::Path::new("out");
    let _ = std::fs::create_dir_all(path);
    let _ = std::fs::write(path.join("t1-glass-result.md"), &md);
    println!("\n报告已写入 probe/out/t1-glass-result.md");
}
