//! 自动隐藏 / 贴边唤出。
//!
//! 设计取舍（与文档 §3.2 的双窗口方案不同）：
//! 文档原本提出「触发条窗口 + 主窗口」两个窗口，理由是隐藏时若主窗口仍有
//! 透明区域会挡住桌面点击。这里改为**把主窗口整个移到屏幕外**——
//! 移出屏幕的窗口不可能挡住任何东西，于是**不需要第二个窗口**，
//! 少一个窗口就少一套生命周期与命中测试的麻烦。
//!
//! P0 的 T3 已实测：单次 `SetWindowPos` 约 300µs，最坏占 60Hz 帧预算 12.4%，
//! 逐帧移动窗口是安全的。

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::win_layer;

/// 暂停**计数**：右键菜单打开、文件对话框打开时各自 `pause()` 一次。
///
/// ❗为什么是计数而不是布尔：这两件事**可能同时发生**（右键菜单开着的时候从托盘
/// 选「添加应用…」）。用布尔时后关闭的那一方会把标志清成 `false`，
/// 于是菜单还开着、Dock 却开始自动隐藏了 —— 而按 `AtomicBool` 写的话这个 bug
/// 只会在很窄的时序里出现，极难复现。计数让「谁申请的谁释放」互相独立。
static PAUSE_DEPTH: AtomicUsize = AtomicUsize::new(0);

/// 是否启用自动隐藏。托盘上的「自动隐藏」开关直接改它，**立即生效**
/// （线程每帧都会读，不需要重启）。
pub static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn pause() {
    PAUSE_DEPTH.fetch_add(1, Ordering::SeqCst);
}

pub fn unpause() {
    // saturating：多余的 unpause 不能被忽略成"计数回绕到极大值"（那就永远暂停了）
    let _ = PAUSE_DEPTH.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
        Some(v.saturating_sub(1))
    });
}

pub fn is_paused() -> bool {
    PAUSE_DEPTH.load(Ordering::SeqCst) > 0
}

pub fn set_enabled(v: bool) {
    ENABLED.store(v, Ordering::SeqCst);
}

/// Dock 当前的位置与尺寸（物理像素）。前端上报尺寸后由 `win_layer::layout_dock` 更新。
/// 隐藏线程**每帧读它** —— 所以改尺寸不需要重启线程。
static TARGET: std::sync::Mutex<(i32, i32, i32, i32)> = std::sync::Mutex::new((0, 0, 0, 0));

pub fn set_target(r: RECT) {
    *TARGET.lock().unwrap() = (r.left, r.top, r.right - r.left, r.bottom - r.top);
}

fn get_target() -> (i32, i32, i32, i32) {
    *TARGET.lock().unwrap()
}

/// 隐藏位置：整块移到屏幕外
fn hidden_top() -> i32 {
    win_layer::work_area().bottom + 2
}

#[derive(Debug, Clone, Copy)]
pub struct RevealParams {
    /// 距屏幕底边多少像素内算「触底唤出」
    pub trigger_px: i32,
    /// 鼠标离开后多久隐藏
    pub hide_delay: Duration,
    /// 动画时长
    pub duration: Duration,
}

impl Default for RevealParams {
    fn default() -> Self {
        Self {
            trigger_px: 3,
            hide_delay: Duration::from_millis(420),
            duration: Duration::from_millis(180),
        }
    }
}

fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

fn ease_in_quad(t: f64) -> f64 {
    t * t
}

fn cursor() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

fn point_in(r: &RECT, x: i32, y: i32, slack: i32) -> bool {
    x >= r.left - slack && x <= r.right + slack && y >= r.top - slack && y <= r.bottom + slack
}

/// 把窗口移动/缩放到指定矩形（**不改 Z 序**）。
///
/// ❗为什么必须是 `SWP_NOZORDER` 而不是原来的 `HWND_TOPMOST`：
/// Dock 属于**桌面那一层** —— 用户的窗口应该在它上面（"Dock 只在桌面上，
/// 其他窗口都在它之上"）。每帧重新 `HWND_TOPMOST` 会把窗口又拽回最顶层，
/// 于是刚被窗口盖住的 Dock 会在下一帧自己冒出来。
fn move_to(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None, // SWP_NOZORDER：Z 序保持不动
            x,
            y,
            w,
            h,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
}

/// 兜底体检：Dock 窗口**绝不能带 `WS_EX_TOPMOST`**，带上就清掉。
///
/// ❗这只是**兜底**，主防线在 `win_layer::dock_subclass_proc` 的两条消息钩子里：
/// 那里在改动**生效之前**就把它拦掉了（`WM_WINDOWPOSCHANGING` 补 `SWP_NOZORDER`、
/// `WM_STYLECHANGING` 抹掉 `WS_EX_TOPMOST`），所以正常情况下这个函数永远不该触发。
///
/// 留着它的理由：万一还有既不发这两条消息、又能把窗口设成置顶的野路子
/// （直接操作窗口带/内核对象之类），5 秒内也会被纠正，并且日志会留下证据 ——
/// 真触发了就说明主防线有洞，值得去查。
///
/// 开销：一次 `GetWindowLongPtrW`。**实测 8.5 ns/次**（同进程 / 跨进程一样，
/// user32 在客户端就把 GWL_EXSTYLE 答了），每 5 秒一次 = 0.00000017% 单核。
fn clear_topmost_if_set(hwnd: HWND) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOPMOST.0 == 0 {
            return;
        }
        log_info!("[层级] ⚠️ 兜底体检发现 Dock 带着置顶位（说明消息钩子没拦住）—— 已清除");
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_NOTOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// 上一次**真正应用过**的窗口矩形。
#[derive(PartialEq, Clone, Copy, Debug)]
struct Applied {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// 只在矩形真的变化时才 `SetWindowPos`。
///
/// ❗**为什么必须有这个判断**（这是本项目最大的一处纯浪费，2026-09 实测定位）：
///
/// 主循环是 **62.5 Hz**（`sleep(16ms)`），而一次带亚克力的 `SetWindowPos` 实测约
/// **313 µs**。原来两条分支都无条件调用 `move_to`，于是
/// `62.5 × 313µs ≈ 1.96% 单核` 被永久浪费在「把窗口挪到它已经在的地方」。
///
/// 实测证据：把前端轮询从 1.5 秒改成 60 秒（几乎不轮询），空闲 CPU 只从 2.34%
/// 降到 2.188% —— **2.19% 里轮询只占 0.15%**，剩下的就是这个循环。
///
/// 窗口没动就不需要再 `SetWindowPos`：topmost 一旦设上就保持，不靠每帧重申。
fn move_if_changed(hwnd: HWND, last: &mut Option<Applied>, x: i32, y: i32, w: i32, h: i32) {
    let r = Applied { x, y, w, h };
    if *last == Some(r) {
        return;
    }
    move_to(hwnd, x, y, w, h);
    *last = Some(r);
}

/// 带缓动地把窗口从 from_y 移到 to_y
fn animate(hwnd: HWND, x: i32, w: i32, h: i32, from_y: i32, to_y: i32, duration: Duration, reveal: bool) {
    if from_y == to_y {
        move_to(hwnd, x, to_y, w, h);
        return;
    }
    let start = Instant::now();
    let total = duration.as_secs_f64().max(0.001);
    loop {
        let t = (start.elapsed().as_secs_f64() / total).min(1.0);
        let e = if reveal { ease_out_cubic(t) } else { ease_in_quad(t) };
        let y = from_y + ((to_y - from_y) as f64 * e).round() as i32;
        move_to(hwnd, x, y, w, h);
        if t >= 1.0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    move_to(hwnd, x, to_y, w, h);
}

/// 启动自动隐藏线程。调用方需保证窗口当前处于 `shown` 位置。
///
/// 注意：`HWND` 含裸指针、不是 `Send`，跨线程只传地址。
pub fn spawn(hwnd_addr: usize, params: RevealParams) {
    std::thread::spawn(move || {
        let hwnd = HWND(hwnd_addr as *mut core::ffi::c_void);
        run(hwnd, params);
    });
}

fn run(hwnd: HWND, p: RevealParams) {
    // 初始状态：贴到屏幕外（隐藏）
    let mut visible = false;
    let mut y = hidden_top();
    // 上一次真正应用过的矩形（见 move_if_changed）
    let mut applied: Option<Applied> = None;

    let mut left_at = Instant::now();
    let mut cooldown_until = Instant::now();
    // 兜底体检的帧计数（62.5Hz → 312 帧 ≈ 5 秒，见 clear_topmost_if_set）
    let mut tick: u32 = 0;
    // 上次报告过的拦截次数（见下：钩子不做 I/O，日志由这里代打）
    let mut reported_z: u32 = 0;
    let mut reported_s: u32 = 0;

    log_info!(
        "[自动隐藏] 线程已启动：触发区 {}px 延迟 {}ms（当前{}）",
        p.trigger_px,
        p.hide_delay.as_millis(),
        if ENABLED.load(Ordering::SeqCst) { "启用" } else { "关闭" }
    );

    loop {
        std::thread::sleep(Duration::from_millis(16));

        // 报告被消息钩子挡下来的东西（钩子自己不做 I/O —— 那是别的进程
        // 同步调进来的调用栈，见 `win_layer::ZORDER_BLOCKS` / `TOPMOST_STYLE_BLOCKS`）。
        let z = win_layer::ZORDER_BLOCKS.load(Ordering::Relaxed);
        let s = win_layer::TOPMOST_STYLE_BLOCKS.load(Ordering::Relaxed);
        if z != reported_z || s != reported_s {
            log_info!(
                "[层级] 已挡下对 Dock 的 Z 序变更 {z} 次、直接的置顶样式 {s} 次 —— \
                 它固定在桌面那一层（顺带：tao 自己也会发 NOTOPMOST，那种也算在 Z 序里）"
            );
            reported_z = z;
            reported_s = s;
        }

        // 兜底体检（主防线是上面那两条消息钩子，这里只管野路子）。
        // 放在 `continue` 之前 —— 自动隐藏关闭 / 暂停时它也要在。
        tick = tick.wrapping_add(1);
        if tick % 312 == 0 {
            clear_topmost_if_set(hwnd);
        }

        // 桌面窗口跑到 Dock **上面** = Dock 被壁纸盖住（典型成因：explorer 重启后
        // shell 重建桌面窗口）。修法是**把那些桌面窗口沉下去**，Dock 自己的 Z 序一次都不动 ——
        // 所以"永不上升"依旧是绝对不变式，钩子也不需要任何旁路。
        //
        // 每 2 秒扫一次 Z 序（约 150 个顶层窗口 + 类名判断，微秒级）：
        // 出事后 Dock 最多被盖住两秒就自己回来。
        if tick % 124 == 0 && win_layer::recover_if_desktop_above(hwnd) {
            log_warn!("[层级] 桌面窗口跑到了 Dock 上面（多半是 explorer 重启）—— 已把桌面沉回 Dock 下方");
        }

        // 心跳：每 5 分钟一行（62.5Hz → 18750 帧）。
        // 有了它，日志里能直接看出两件事：**它还活着**、以及**死在什么时刻**
        // （闪退时最后一条心跳的时间 + 崩溃报告里的时间对得上）。
        // 顺带记工作集 —— 8 小时验收要看内存有没有一路涨。
        if tick % 18750 == 0 {
            crate::logging::heartbeat();
        }

        // 尺寸由前端上报，没上报之前什么都不做
        let (tx, ty, tw, th) = get_target();
        if tw == 0 || th == 0 {
            continue;
        }
        let hidden = hidden_top();
        // 让窗口回到当前应有的位置（尺寸变化时也能立刻跟上）
        let shown_rect = RECT {
            left: tx,
            top: ty,
            right: tx + tw,
            bottom: ty + th,
        };

        // 菜单打开 / 拖拽中 / 用户关掉了自动隐藏 —— 保持显示，不隐藏
        if is_paused() || !ENABLED.load(Ordering::SeqCst) {
            if !visible {
                animate(hwnd, tx, tw, th, y, ty, p.duration, true);
                y = ty;
                visible = true;
                applied = None; // animate 自己动过窗口，缓存的矩形作废
            } else {
                move_if_changed(hwnd, &mut applied, tx, ty, tw, th); // 跟随尺寸变化
            }
            left_at = Instant::now();
            continue;
        }

        let c = cursor();

        // 动画期间不接受新决策，避免抖动
        if Instant::now() < cooldown_until {
            continue;
        }

        let at_bottom = c.y >= shown_rect.bottom - p.trigger_px;
        let inside = point_in(&shown_rect, c.x, c.y, 8);

        if !visible {
            if at_bottom {
                animate(hwnd, tx, tw, th, y, ty, p.duration, true);
                y = ty;
                visible = true;
                left_at = Instant::now();
                cooldown_until = Instant::now() + p.duration;
                applied = None; // animate 自己动过窗口，缓存的矩形作废
            }
        } else {
            // 尺寸/位置变化时跟着走，避免残留旧位置
            move_if_changed(hwnd, &mut applied, tx, y, tw, th);
            if inside {
                left_at = Instant::now();
            } else if left_at.elapsed() >= p.hide_delay && !at_bottom {
                animate(hwnd, tx, tw, th, y, hidden, p.duration, false);
                y = hidden;
                visible = false;
                cooldown_until = Instant::now() + p.duration;
                applied = None; // animate 自己动过窗口，缓存的矩形作废
            }
        }
    }
}
