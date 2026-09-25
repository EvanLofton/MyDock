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

/// 「把桌面沉回 Dock 下方」成功了多少次。
///
/// 这是桌面层守卫**真的动过手**的证据 —— 日志那句 `[层级] 桌面层被抬到了…` 会被限流，
/// 自检也没法读日志，所以留一个计数器（自检 ⑦ 断言它涨了，才说明"守卫发现了并处理了"，
/// 而不是"这次抬起根本没发生"）。
pub static DESKTOP_SINKS: AtomicUsize = AtomicUsize::new(0);

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

/// 「桌面层被抬起」事件触发后**连扫**的帧数（62.5Hz → 62 帧 ≈ 1 秒）。
///
/// ❗事件和"桌面真的压到 Dock 上面"不是同一时刻：shell 先最小化窗口 / 先切前台，
/// 再改 Z 序，中间隔着几十到几百毫秒（实测"最小化事件"比"桌面被抬起来"早约 90ms）。
/// 第一版这个数是 20 帧（320ms），实测**不够**：A/B 对照里"有事件钩子"反而比
/// "只靠前台轮询"更差（最坏 260ms vs 0ms）—— 事件太早到，把连扫窗口在桌面抬起之前
/// 就用完了。所以这里必须覆盖整个 shell 动画期，而不是"够长就行"。
const WATCH_TICKS: u32 = 62;

/// 把桌面沉下去之后**再连扫**的帧数（≈384ms）：实测 shell 会在动画期间**反复**抬它
/// （我们沉一次、它抬一次），只跟一帧是不够的。
const FOLLOWUP_TICKS: u32 = 24;

/// 没有事件时的兜底扫描间隔（62.5Hz → 31 帧 ≈ 500ms）。
///
/// 成本实测：一次 `EnumWindows` + 类名判断（约 150 个顶层窗口）≈ **25µs**，
/// 500ms 一次 = **0.005% 单核**。原来这个数是 2 秒 —— 那是用户看到"Win+D 之后
/// Dock 消失 1.7 秒"的直接原因，现在事件负责毫秒级、兜底只负责漏网的。
const FALLBACK_TICKS: u32 = 31;

/// 「桌面被抬起来了」这条日志的限流间隔 —— 万一 shell 进入"抬—沉"循环，
/// 62.5Hz 的日志会瞬间把 8MB 的日志文件刷满。
const LOG_THROTTLE: Duration = Duration::from_secs(5);

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
    // 桌面层守卫：事件触发后的连扫剩余帧数（见 WATCH_TICKS），以及日志限流时刻
    let mut watch_ticks: u32 = 0;
    let mut last_desktop_log: Option<Instant> = None;
    // 上次报告过的拦截次数（见下：钩子不做 I/O，日志由这里代打）
    let mut reported_z: u32 = 0;
    let mut reported_s: u32 = 0;
    let mut reported_ev: u32 = 0;
    let mut reported_sinks: u32 = 0;
    // 上一次看到的**前台窗口**（桌面层守卫用；见下面的"前台轮询"）
    let mut last_foreground: isize = 0;
    // `DOCK_DEBUG=1` 时把守卫的开扫/扫描结果逐帧打出来（排查用，平时不输出）
    let debug_desktop = std::env::var("DOCK_DEBUG").as_deref() == Ok("1");
    // 本次连扫是**哪条触发源**开的（写进日志：四条路各自都能单独失灵，说清楚才好排查）
    let mut trigger = String::new();

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
        // 事件计数也一起报：没有它就分不清"钩子没工作"和"钩子工作了但没抓到"
        let (ev_count, ev_last, _ev_time) = win_layer::desktop_watch_stats();
        let sinks = DESKTOP_SINKS.load(Ordering::Relaxed) as u32;
        if z != reported_z || s != reported_s || ev_count != reported_ev || sinks != reported_sinks {
            log_info!(
                "[层级] 已挡下对 Dock 的 Z 序变更 {z} 次、直接的置顶样式 {s} 次、最小化 {m} 次、\
                 桌面沉回 {sinks} 次 —— 它固定在桌面那一层\
                 （顺带：tao 自己也会发 NOTOPMOST，那种也算在 Z 序里）",
                m = win_layer::MINIMIZE_BLOCKS.load(Ordering::Relaxed)
            );
            if ev_count != reported_ev {
                log_info!(
                    "[层级] 桌面守卫事件：累计 {ev_count} 次，最后一次 event=0x{ev_last:04X}\
                     （0x0003=前台变成桌面，0x0016/0x0017=窗口开始/结束最小化）"
                );
                reported_ev = ev_count;
            }
            reported_z = z;
            reported_s = s;
            reported_sinks = sinks;
        }

        // 兜底体检（主防线是上面那两条消息钩子，这里只管野路子）。
        // 放在 `continue` 之前 —— 自动隐藏关闭 / 暂停时它也要在。
        tick = tick.wrapping_add(1);
        if tick % 312 == 0 {
            clear_topmost_if_set(hwnd);
        }

        // 最小化自愈：`SC_MINIMIZE` 已在窗口过程里被吃掉，这里管绕开 `WM_SYSCOMMAND`
        // 的路子（`ShowWindow(SW_MINIMIZE)`）。一次 `IsIconic` 而已，所以每帧查。
        if win_layer::ensure_not_minimized(hwnd) {
            log_warn!("[层级] Dock 被系统最小化了（Win+M / 最小化所有窗口）—— 已还原到桌面层");
            // 位置/显示状态都由本循环管，还原之后让它重新决策
            visible = false;
            y = hidden_top();
            applied = None;
        }

        // ---- 桌面层守卫（"Win+D 之后 Dock 不见了"就死在这里）----
        //
        // 触发源有两类：
        //   ① **事件**（`win_layer::install_desktop_watch`）：前台变成桌面、或有窗口开始/结束
        //      最小化 —— Win+D 就是这个动作。回调只置一个原子标记，消费在这里。
        //   ② **兜底**：每 ~500ms 无条件扫一遍，覆盖钩子抓不到的路子（explorer 重启等）。
        //
        // 修法永远是**把桌面沉下去**，Dock 自己的 Z 序一次都不动（见
        // `win_layer::recover_if_desktop_above` 里的取舍说明）。
        //
        // ⚠️ 事件到了、桌面**还没**抬起来是正常时序（shell 先切前台/最小化窗口，再改 Z 序），
        // 所以事件触发后要**连扫若干帧**，而不是扫一次就完事 —— 扫一次会正好错过。
        //
        // 触发源有四条，前三条各自都可能失灵（实测都失灵过），所以都要：
        //   ① **点探测**（每帧问"Dock 正中心现在被谁占着"，是桌面系就开扫）——
        //      唯一"当帧必定成立"的一条：桌面是全屏窗口，压上来必然接管那个点。
        //      2026-09 实测：`Win+D` 时 WinEvent 没送到、前台也没变成桌面系，
        //      整条防线只剩 500ms 兜底（用户看到桌面盖住 Dock 100~240ms）。这条补的就是它；
        //   ② **前台轮询**（`GetForegroundWindow` + 只在句柄变化时比类名）：
        //      点桌面、`Win+D` 的另一半会让前台变成桌面系；
        //   ③ **WinEvent 钩子**：`Win+D` 实测会发 `EVENT_SYSTEM_MINIMIZESTART`
        //      （比"桌面被抬起来"更早），送到时就是当帧；
        //   ④ 500ms 兜底轮询（explorer 重启等既无前台变化、点也没被接管的极端情况）。
        if win_layer::desktop_covers_dock_center(hwnd) {
            watch_ticks = WATCH_TICKS;
            trigger = "点探测：Dock 中心点被桌面系窗口接管".to_string();
            if debug_desktop {
                log_debug!("[守卫调试] 开扫：Dock 中心点被桌面系窗口接管（{WATCH_TICKS} 帧）");
            }
        }
        let fg = unsafe { GetForegroundWindow().0 as isize };
        if fg != last_foreground {
            let first = last_foreground == 0;
            last_foreground = fg;
            // 首次只记账、不触发（启动那一刻前台是什么都不该开扫）
            if !first && win_layer::foreground_is_desktop() {
                watch_ticks = WATCH_TICKS;
                trigger = "前台窗口变成了桌面".to_string();
                if debug_desktop {
                    log_debug!("[守卫调试] 开扫：前台变成桌面（{WATCH_TICKS} 帧）");
                }
            }
        }
        if win_layer::DESKTOP_ABOVE_SUSPECTED.swap(false, Ordering::SeqCst) {
            watch_ticks = WATCH_TICKS;
            let (c, e, _) = win_layer::desktop_watch_stats();
            trigger = format!("WinEvent 0x{e:04X}（累计 {c} 次）");
            if debug_desktop {
                log_debug!("[守卫调试] 开扫：{trigger}（{WATCH_TICKS} 帧）");
            }
        }
        if watch_ticks > 0 {
            watch_ticks -= 1;
        }
        if watch_ticks > 0 || tick % FALLBACK_TICKS == 0 {
            let sunk = win_layer::recover_if_desktop_above(hwnd);
            if debug_desktop {
                log_debug!(
                    "[守卫调试] 扫描 tick={tick} 连扫剩余={watch_ticks} 兜底={} → {}",
                    tick % FALLBACK_TICKS == 0,
                    if sunk { "发现桌面在上，已沉回" } else { "干净" }
                );
            }
            if sunk {
                DESKTOP_SINKS.fetch_add(1, Ordering::SeqCst);
                // 沉完再连扫几帧：shell 有时会在几十毫秒内**再抬一次**（实测 4 秒内连抬两次）
                watch_ticks = watch_ticks.max(FOLLOWUP_TICKS);
                // 限流：万一 shell 进入"抬—沉"循环，别把日志刷爆
                if debug_desktop
                    || last_desktop_log.map_or(true, |t: Instant| t.elapsed() >= LOG_THROTTLE)
                {
                    // ❗触发源要说实话：第一版按"有没有 WinEvent"二分，于是点探测/前台
                    // 轮询抓到的那次被写成"由 500ms 兜底轮询发现"（实测日志里看到才发现）。
                    let how = if trigger.is_empty() {
                        "500ms 兜底轮询发现".to_string()
                    } else if trigger.starts_with("WinEvent") {
                        let (_, _, ev_time) = win_layer::desktop_watch_stats();
                        let latency =
                            unsafe { windows::Win32::System::SystemInformation::GetTickCount() }
                                .wrapping_sub(ev_time);
                        format!("{trigger}，事件→沉回 {latency}ms")
                    } else {
                        trigger.clone()
                    };
                    log_warn!(
                        "[层级] 桌面层被抬到了 Dock 上方（Win+D/显示桌面、点桌面、explorer 重启都会这样）\
                         —— 已把桌面沉回 Dock 下方（{how}）"
                    );
                    last_desktop_log = Some(Instant::now());
                }
            }
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
