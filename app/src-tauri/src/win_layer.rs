//! 窗口层：扩展样式、焦点防护、DPI 工作区定位。
//!
//! 本模块只依赖 Win32，不含任何 Tauri 类型，便于单测与复用。
//! 毛玻璃与窗口材质在 `glass.rs`。

#![allow(dead_code)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::core::BOOL;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Controls::{INITCOMMONCONTROLSEX, InitCommonControlsEx};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    ABE_BOTTOM, ABM_GETSTATE, ABM_GETTASKBARPOS, ABS_AUTOHIDE, APPBARDATA, DefSubclassProc,
    RemoveWindowSubclass, SHAppBarMessage, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const SUBCLASS_ID: usize = 0xD0CC;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn hmodule() -> HMODULE {
    unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default() }
}

/// 是否拦截 WM_MOUSEACTIVATE（自检时用来做 A/B 对照）
pub static BLOCK_MOUSEACTIVATE: AtomicBool = AtomicBool::new(true);
/// Dock 顶层窗口收到的 WM_MOUSEACTIVATE 次数 —— 这才是钩子是否被真正走到
/// 的判据。注意不能用 WM_LBUTTONDOWN 计数：WebView2 子窗口会吃掉鼠标消息，
/// 顶层窗口收不到，用它计数会得出「点击没送达」的错误结论。
pub static MOUSEACTIVATE_COUNT: AtomicU32 = AtomicU32::new(0);
pub static CLICK_COUNT: AtomicU32 = AtomicU32::new(0);

/// 被挡下的「对 Dock 的 Z 序变更」次数。
///
/// ⚠️ 这里**故意不叫"置顶攻击次数"**：Win32 的语义是"插到一个置顶窗口之后 = 自己也置顶"，
/// 而 `HWND_TOPMOST` 这类常量在消息里**已被 win32k 解析成具体句柄**（实测：传 -1 进来的是
/// `0x610464` 这样的真实 HWND），所以从消息里**没法百分百区分**"想把它置顶"和
/// "只想挪到某个正常窗口之后"。实测 tao 自己就会发 `HWND_NOTOPMOST`
/// （`always_on_top(false)` 的实现）。行为上一律挡掉（安全第一：宁可多挡，绝不放过），
/// 但计数时不要把话说成"有人在攻击你"。
pub static ZORDER_BLOCKS: AtomicU32 = AtomicU32::new(0);

/// 被抹掉的「直接给 Dock 加置顶样式」次数（`WM_STYLECHANGING` 那条路）。
/// 这一条**可以**说死：新样式里确实带着 `WS_EX_TOPMOST`。
pub static TOPMOST_STYLE_BLOCKS: AtomicU32 = AtomicU32::new(0);

/// 子类化的 refdata：标记"这就是 Dock 的顶层窗口"。
///
/// 层级防线**只装在顶层窗口上** —— 子窗口（WebView2 宿主）永远不会置顶，
/// 而它们内部的 Z 序由 wry/tao 自己安排，拦了反而可能把 WebView 挡在兄弟窗口后面。
const DOCK_TOP_DATA: usize = 0xD0C7;

// ---------------------------------------------------------------- 消息子类化

/// P0 实测结论：`WS_EX_NOACTIVATE` 只在**跨进程**时挡住点击激活，
/// **同一进程**的窗口之间仍会互相激活。必须额外让 `WM_MOUSEACTIVATE`
/// 返回 `MA_NOACTIVATE` 才能覆盖同进程场景（右键菜单/设置窗打开时点 Dock 本体）。
///
/// tao 既不设 `WS_EX_NOACTIVATE`，也没有暴露该消息的处理入口，
/// 因此这里用 `SetWindowSubclass` 挂上去。
unsafe extern "system" fn dock_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    // `data == DOCK_TOP_DATA` 才是 Dock 的**顶层**窗口（子窗口只用来拦 WM_MOUSEACTIVATE）
    let is_dock_top = data == DOCK_TOP_DATA;
    match msg {
        WM_LBUTTONDOWN => {
            CLICK_COUNT.fetch_add(1, Ordering::SeqCst);
        }
        WM_MOUSEACTIVATE => {
            MOUSEACTIVATE_COUNT.fetch_add(1, Ordering::SeqCst);
            if BLOCK_MOUSEACTIVATE.load(Ordering::Relaxed) {
                return LRESULT(MA_NOACTIVATE as isize);
            }
        }
        // ---- 层级防线①：任何**改 Z 序**的请求都补上 SWP_NOZORDER ----
        //
        // ❗为什么在消息里拦而不是定期查样式位：`WM_WINDOWPOSCHANGING` 是**改动之前**
        // 发给目标窗口的（`SetWindowPos` 同步调进我们的窗口过程，**跨进程也会发过来**），
        // 而且文档明确允许在此时修改 `WINDOWPOS`。补上 `SWP_NOZORDER` 之后，
        // 这次置顶**从来没有发生过** —— 定期轮询最快也要等一个周期，那段时间里
        // Dock 已经真的浮在游戏/全屏窗口上面了（用户就是这么发现的）。
        //
        // 代价：Dock 从此不接受任何 Z 序变更（包括我们自己也不改，见 `reveal::move_to`）。
        // 这正是需求："Dock 只在桌面那一层，别的窗口都在它之上" —— 谁也不许把它抬起来。
        //
        // ⚠️ 记账别把话说死：`HWND_TOPMOST` 在消息里已经被 win32k 解析成**具体句柄**
        // （实测传 -1 进来的是 `0x610464` 这样的 HWND），所以从消息里区分不出
        // "想置顶"还是"只是想挪 Z 序"。这里只统计"挡下了多少次 Z 序变更"，
        // 精确的置顶信号留给下面那条 `WM_STYLECHANGING`（那条能说死）。
        WM_WINDOWPOSCHANGING if is_dock_top => {
            let wpos = lp.0 as *mut WINDOWPOS;
            if !wpos.is_null() && ((*wpos).flags.0 & SWP_NOZORDER.0) == 0 {
                ZORDER_BLOCKS.fetch_add(1, Ordering::Relaxed);
                (*wpos).flags |= SWP_NOZORDER;
            }
        }
        // ---- 层级防线②：绕过 SetWindowPos、直接改扩展样式的野路子 ----
        //
        // `SetWindowLongPtrW(GWL_EXSTYLE, … | WS_EX_TOPMOST)` **不**走 WM_WINDOWPOSCHANGING，
        // 只会来 WM_STYLECHANGING —— 把新样式里那个位抹掉（同样发生在生效之前）。
        // 这条路的判据是确定的：新样式里确实带着 `WS_EX_TOPMOST`。
        WM_STYLECHANGING if is_dock_top => {
            if wp.0 as i32 == GWL_EXSTYLE.0 {
                let ss = lp.0 as *mut STYLESTRUCT;
                if !ss.is_null() && ((*ss).styleNew & WS_EX_TOPMOST.0) != 0 {
                    (*ss).styleNew &= !WS_EX_TOPMOST.0;
                    TOPMOST_STYLE_BLOCKS.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

/// 把 Dock 沉到 Z 序的**最底层**（所有普通窗口之下）。
///
/// ❗**必须在 `install_mouseactivate_hook_tree` 之前调用**：那个钩子会拦掉所有 Z 序变更
/// （包括我们自己这一次），装完之后谁也抬不动它、它自己也不再动。
///
/// 为什么需要这一步：窗口创建时会被插到普通窗口带的**最前面**，于是
/// "启动 Dock 之前就存在、之后一直没被激活过"的窗口会被它盖住底部一条。
/// 需求是"Dock 任何时刻都在最底层、不许有任何上升层级的行为"（用户 2026-09 明确要求），
/// 所以启动时主动沉一次底。
///
/// 注意方向：这不是"置顶的反面"。`HWND_BOTTOM` 把它放到普通窗口带的最下面，
/// 之后新开的窗口照常落在它**上面** —— 那正是需求要的。
pub fn sink_to_bottom(hwnd: HWND) -> bool {
    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
        .is_ok()
    }
}

/// comctl32 未初始化时 SetWindowSubclass 会失败，先初始化再挂。
pub fn init_common_controls() -> bool {
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: windows::Win32::UI::Controls::ICC_STANDARD_CLASSES,
        };
        InitCommonControlsEx(&icc).as_bool()
    }
}

/// 注意：`SetWindowSubclass` **必须由拥有该窗口的线程调用**，
/// 否则会失败（实测在后台线程调用返回 false）。本模块的调用方需保证在主线程执行。
///
/// `is_dock_top`：是不是 Dock 的顶层窗口 —— 只有它为真时才启用层级防线
/// （见 `dock_subclass_proc` 里那两条 `WM_*`）。
pub fn install_mouseactivate_hook(hwnd: HWND, is_dock_top: bool) -> (bool, String) {
    // 先摘再挂，保证**幂等** —— 否则重复调用会在同一个窗口上叠多层子类化，
    // 消息被处理多次（周期性重挂时必须幂等）
    let _ = remove_mouseactivate_hook(hwnd);
    let data = if is_dock_top { DOCK_TOP_DATA } else { 0 };
    let ok = unsafe { SetWindowSubclass(hwnd, Some(dock_subclass_proc), SUBCLASS_ID, data).as_bool() };
    let err = unsafe { GetLastError() };
    (ok, format!("{:?}", err))
}

pub fn remove_mouseactivate_hook(hwnd: HWND) -> bool {
    unsafe { RemoveWindowSubclass(hwnd, Some(dock_subclass_proc), SUBCLASS_ID).as_bool() }
}

static HOOK_OK: AtomicU32 = AtomicU32::new(0);
static HOOK_TOTAL: AtomicU32 = AtomicU32::new(0);
static HOOK_DETAIL: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn record_and_hook(h: HWND, depth: usize) {
    HOOK_TOTAL.fetch_add(1, Ordering::SeqCst);
    let mut cls = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut cls) };
    let class = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
    let (ok, err) = install_mouseactivate_hook(h, depth == 0);
    if ok {
        HOOK_OK.fetch_add(1, Ordering::SeqCst);
    }
    HOOK_DETAIL.lock().unwrap().push(format!(
        "    {}{:<34} 挂载={}  {}",
        "  ".repeat(depth),
        class,
        if ok { "成功" } else { "**失败**" },
        if ok { String::new() } else { err }
    ));
}

unsafe extern "system" fn child_hook_cb(hwnd: HWND, _lp: LPARAM) -> BOOL {
    record_and_hook(hwnd, 1);
    TRUE
}

/// 把 `WM_MOUSEACTIVATE` 口子挂到**整个窗口树**上（自己 + 所有子窗口）。
///
/// 为什么必须挂子窗口：点击落在 WebView2 的宿主子窗口上，
/// Windows 可能把 `WM_MOUSEACTIVATE` 发给子窗口；若子窗口自行处理且不上抛，
/// 只挂在顶层窗口的钩子就永远走不到（这是 P1 报告 §6.1 记录的残留风险）。
///
/// WebView2 会动态创建子窗口，所以需要周期性重挂 —— 因此本函数必须幂等。
///
/// 返回 (挂上的窗口数, 遍历到的窗口总数)
pub fn install_mouseactivate_hook_tree(root: HWND) -> (u32, u32) {
    HOOK_OK.store(0, Ordering::SeqCst);
    HOOK_TOTAL.store(0, Ordering::SeqCst);
    HOOK_DETAIL.lock().unwrap().clear();

    record_and_hook(root, 0);
    unsafe {
        let _ = EnumChildWindows(Some(root), Some(child_hook_cb), LPARAM(0));
    }
    (HOOK_OK.load(Ordering::SeqCst), HOOK_TOTAL.load(Ordering::SeqCst))
}

/// 取上一次遍历的逐窗口明细（用于诊断「哪些窗口挂不上」）
pub fn hook_tree_detail() -> Vec<String> {
    HOOK_DETAIL.lock().unwrap().clone()
}

/// 遍历到的窗口数（含自身），用于让调用方按需重挂
pub fn hook_tree_once(root: HWND) -> (u32, u32) {
    install_mouseactivate_hook_tree(root)
}

pub fn click_count() -> u32 {
    CLICK_COUNT.load(Ordering::SeqCst)
}

pub fn mouseactivate_count() -> u32 {
    MOUSEACTIVATE_COUNT.load(Ordering::SeqCst)
}

pub fn reset_counters() {
    CLICK_COUNT.store(0, Ordering::SeqCst);
    MOUSEACTIVATE_COUNT.store(0, Ordering::SeqCst);
}

/// 点是否落在矩形里（左闭右开，与 Windows 的命中测试习惯一致）。
pub fn point_in(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// 指定屏幕坐标处最上层的**顶层**窗口是谁。
/// 用来证明「点击到底落在哪个窗口上」，不依赖任何消息路由。
pub fn top_level_window_at(x: i32, y: i32) -> HWND {
    unsafe {
        let pt = POINT { x, y };
        let h = WindowFromPoint(pt);
        if h.0.is_null() {
            return HWND::default();
        }
        GetAncestor(h, GA_ROOT)
    }
}

/// 「桌面窗口跑到 Dock 上面了吗」这次扫描用的静态槽（`EnumWindows` 回调不能带闭包）
static REC_DOCK: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static REC_IDX: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static REC_DOCK_IDX: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(-1);
static REC_DESKTOPS: std::sync::Mutex<Vec<isize>> = std::sync::Mutex::new(Vec::new());

/// `EnumWindows` 从最上到最下：**在遇到 Dock 之前**收集所有可见的桌面系窗口。
/// 它们就是"Dock 被壁纸盖住"的元凶（`Progman` / `WorkerW` / `SHELLDLL_DefView`）。
///
/// ⚠️ 只认**可见**的：`WorkerW` 在本机有十几个隐藏的宿主窗口（Wallpaper Engine 之类），
/// 它们不绘制、不会盖住 Dock，动它们纯属惹事。
unsafe extern "system" fn recover_scan_cb(h: HWND, _l: LPARAM) -> BOOL {
    let i = REC_IDX.fetch_add(1, Ordering::SeqCst);
    if h.0 as isize == REC_DOCK.load(Ordering::SeqCst) {
        REC_DOCK_IDX.store(i, Ordering::SeqCst);
        return TRUE;
    }
    if REC_DOCK_IDX.load(Ordering::SeqCst) >= 0 {
        return TRUE; // 已经枚举到 Dock 之后了 —— 那些窗口在它下面，不用管
    }
    if !IsWindowVisible(h).as_bool() {
        return TRUE;
    }
    let mut cls = [0u16; 64];
    let n = GetClassNameW(h, &mut cls);
    let class = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
    if matches!(class.as_str(), "Progman" | "WorkerW" | "SHELLDLL_DefView") {
        REC_DESKTOPS.lock().unwrap().push(h.0 as isize);
    }
    TRUE
}

/// 扫一遍：**当前有多少个可见的桌面系窗口排在 Dock 之前**（= 压在 Dock 上面）。
/// `recover_if_desktop_above` 用它来判定，自检用它来断言。
pub fn visible_desktops_above(dock: HWND) -> Vec<isize> {
    REC_DOCK.store(dock.0 as isize, Ordering::SeqCst);
    REC_IDX.store(0, Ordering::SeqCst);
    REC_DOCK_IDX.store(-1, Ordering::SeqCst);
    REC_DESKTOPS.lock().unwrap().clear();
    unsafe {
        let _ = EnumWindows(Some(recover_scan_cb), LPARAM(0));
    }
    std::mem::take(&mut *REC_DESKTOPS.lock().unwrap())
}

/// 万一**桌面窗口**（`Progman` / `WorkerW` / `SHELLDLL_DefView`）跑到 Dock **上面**了，
/// 把**那些桌面窗口**沉到底 —— 而不是去抬高 Dock。
///
/// # 为什么是这个方向
///
/// "Dock 被壁纸盖住"的成因是桌面窗口的层级变了（典型场景：explorer 重启，shell 重建桌面窗口）。
/// 直觉上的修法是"把 Dock 重新沉一次底"，但那要**改 Dock 自己的 Z 序** ——
/// 而层级钩子会拦掉一切对 Dock 的 Z 序变更（那正是"永不上升"的保证），
/// 于是就得给钩子开一个"允许自己改一次"的旁路，而旁路本身就是外部能钻的洞。
///
/// 换个方向就没有这个问题：**修桌面那一边**。Dock 的 Z 序一次都不动，
/// "不许上升"依旧绝对，也不需要任何旁路。跨进程沉 shell 的桌面窗口是允许的
/// （实测 `SetWindowPos(progman, HWND_BOTTOM)` 返回成功、无拒绝访问，同完整性级别）。
///
/// # 判据为什么不看"Dock 中心点上是谁"
///
/// 那个点常常被**应用窗口**盖着（用户的窗口本来就该盖住 Dock），于是"最上层是桌面"
/// 这种状态根本观察不到 —— 但此时 Dock 已经整条被壁纸盖住了（桌面是全屏窗口）。
/// 所以判据直接比 Z 序：**只要有任何可见的桌面系窗口排在 Dock 之前**，就算出事。
///
/// 返回值：真的沉了至少一个桌面窗口 = `true`。
pub fn recover_if_desktop_above(dock: HWND) -> bool {
    let found = visible_desktops_above(dock);
    let mut sunk = false;
    for addr in found {
        unsafe {
            if SetWindowPos(
                HWND(addr as *mut c_void),
                Some(HWND_BOTTOM),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
            .is_ok()
            {
                sunk = true;
            }
        }
    }
    sunk
}

/// 系统 DPI（日志会话头用；窗口的 DPI 用 `dpi_of`）
pub fn system_dpi() -> u32 {
    unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() }
}

/// 主屏物理像素尺寸（**调用方需保证进程已 DPI 感知**，否则拿到的是虚拟化值 —— 实测踩过）
pub fn screen_size() -> (i32, i32) {
    unsafe {
        (
            windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
            ),
            windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN,
            ),
        )
    }
}

/// 当前进程的工作集（MB）。心跳里记它，用来发现 8 小时里的内存增长。
pub fn memory_mb() -> Option<u64> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    unsafe {
        let mut c = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        let me = windows::Win32::System::Threading::GetCurrentProcess();
        if GetProcessMemoryInfo(me, &mut c, c.cb).is_ok() {
            Some((c.WorkingSetSize / 1024 / 1024) as u64)
        } else {
            None
        }
    }
}

pub fn title_of(hwnd: HWND) -> String {
    unsafe {
        if hwnd.0.is_null() {
            return "<null>".into();
        }
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

// ---------------------------------------------------------------- 扩展样式

/// 应用 Dock 需要的窗口扩展样式。
///
/// - 加 `WS_EX_NOACTIVATE`：跨进程不夺焦点
/// - 加 `WS_EX_TOOLWINDOW`、去 `WS_EX_APPWINDOW`：不出现在任务栏与 Alt+Tab
///   （实测 tao 的 `skip_taskbar` 未生效，这里手动补）
/// - 去掉 `WS_EX_LAYERED`：P0 证明它会把毛玻璃从 76px 削弱到 66px
pub fn apply_dock_ex_style(hwnd: HWND) -> (u32, u32) {
    unsafe {
        let before = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let mut after = before;
        after |= WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0;
        after &= !WS_EX_APPWINDOW.0;
        after &= !WS_EX_LAYERED.0;
        if after != before {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, after as isize);
        }
        let actual = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        (before, actual)
    }
}

pub fn describe_ex_style(ex: u32) -> String {
    let known: [(u32, &str); 9] = [
        (WS_EX_NOACTIVATE.0, "NOACTIVATE"),
        (WS_EX_LAYERED.0, "LAYERED"),
        (WS_EX_NOREDIRECTIONBITMAP.0, "NOREDIRECTIONBITMAP"),
        (WS_EX_TOOLWINDOW.0, "TOOLWINDOW"),
        (WS_EX_TOPMOST.0, "TOPMOST"),
        (WS_EX_APPWINDOW.0, "APPWINDOW"),
        (WS_EX_TRANSPARENT.0, "TRANSPARENT"),
        (WS_EX_ACCEPTFILES.0, "ACCEPTFILES"),
        (WS_EX_WINDOWEDGE.0, "WINDOWEDGE"),
    ];
    let mut v: Vec<&str> = Vec::new();
    for (bit, name) in known {
        if ex & bit != 0 {
            v.push(name);
        }
    }
    if v.is_empty() {
        "<none>".into()
    } else {
        v.join("|")
    }
}

pub fn describe_style(st: u32) -> String {
    let known: [(u32, &str); 6] = [
        (WS_POPUP.0, "POPUP"),
        (WS_CAPTION.0, "CAPTION"),
        (WS_THICKFRAME.0, "THICKFRAME"),
        (WS_SYSMENU.0, "SYSMENU"),
        (WS_MINIMIZEBOX.0, "MINBOX"),
        (WS_VISIBLE.0, "VISIBLE"),
    ];
    let mut v: Vec<&str> = Vec::new();
    for (bit, name) in known {
        if st & bit != 0 {
            v.push(name);
        }
    }
    if v.is_empty() {
        "<none>".into()
    } else {
        v.join("|")
    }
}

// 毛玻璃与窗口材质不在这里 —— 见 `glass.rs`。本模块只管样式、焦点防护、几何与定位。

// ---------------------------------------------------------------- 几何与 DPI

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

pub fn dpi_of(hwnd: HWND) -> u32 {
    unsafe {
        let d = GetDpiForWindow(hwnd);
        if d == 0 { 96 } else { d }
    }
}

pub fn window_rect(hwnd: HWND) -> RECT {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rc);
    }
    rc
}

/// 任务栏信息
#[derive(Debug, Clone, Copy)]
pub struct TaskbarInfo {
    /// 任务栏吸附时占用的高度（物理像素）
    pub height: i32,
    /// 是否**配置为**自动隐藏 —— 注意不是"此刻是否藏着"
    pub auto_hidden: bool,
    /// 吸附在屏幕**底边**（只有这种情况才需要从底边往上扣高度）
    pub at_bottom: bool,
}

/// 量测任务栏。
///
/// 为什么要量而不是查：**任务栏自动隐藏时 `rcWork` 等于整屏**，
/// `SPI_GETWORKAREA` 完全不会告诉我们要给任务栏留空间。
///
/// # 两个都不能省的细节（都踩过）
///
/// 1. **「自动隐藏」必须问配置（`ABM_GETSTATE`），不能看窗口此刻的位置。**
///    自动隐藏的任务栏一触底就弹出来，那一瞬间它的顶边在 1020 —— 看着和常显一样，
///    可 `rcWork` 仍然是整屏。于是"看位置"的写法会得出「任务栏不需要留空间」，
///    Dock 就永久压在任务栏的弹出区上（实测：Dock 底 1080、任务栏弹出来就是 1020-1080，
///    鼠标移到 Dock 上 = 把任务栏叫出来盖住自己）。
/// 2. **高度用吸附矩形（`ABM_GETTASKBARPOS`）**，不用窗口当前矩形：
///    收起时窗口矩形是 `(0,1078) 1920x60` 那种"只露两个像素"的形态 —— 高度照样是 60，
///    但顶边会骗人，而且恢复/收起过程中还会变。
pub fn taskbar_info() -> Option<TaskbarInfo> {
    unsafe {
        let h = FindWindowW(windows::core::w!("Shell_TrayWnd"), None).ok()?;
        if h.0.is_null() {
            return None;
        }

        let state = SHAppBarMessage(ABM_GETSTATE, &mut APPBARDATA::default());
        let auto_hidden = (state as u32) & ABS_AUTOHIDE != 0;

        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        let (height, at_bottom) = if SHAppBarMessage(ABM_GETTASKBARPOS, &mut abd) != 0 {
            (abd.rc.bottom - abd.rc.top, abd.uEdge == ABE_BOTTOM)
        } else {
            // 拿不到吸附矩形就退回窗口矩形（至少高度是对的）
            let r = window_rect(h);
            (r.bottom - r.top, true)
        };
        if height <= 0 {
            return None;
        }
        Some(TaskbarInfo {
            height,
            auto_hidden,
            at_bottom,
        })
    }
}

/// 前端是否已经上报过 Dock 尺寸（`set_dock_size` 置位）。
///
/// 用途：窗口层初始化是**异步**投递到主线程的（见 `main.rs::supervise`）。
/// 它里面那句"先用初值定位"如果在页面已经上报之后才执行，就会把真实尺寸覆盖掉，
/// 而且之后不会再有人上报 —— Dock 永久停在初值宽度（两边一片空白）。
static SIZE_REPORTED: AtomicBool = AtomicBool::new(false);

pub fn mark_size_reported() {
    SIZE_REPORTED.store(true, Ordering::SeqCst);
}

pub fn size_reported() -> bool {
    SIZE_REPORTED.load(Ordering::SeqCst)
}

/// 逻辑像素 → 物理像素（单个长度值）
pub fn logical_px(hwnd: HWND, v: f64) -> i32 {
    (v * (dpi_of(hwnd) as f64 / 96.0)).round() as i32
}

/// 按**物理**宽高算出 Dock 的矩形（已钳制在屏内、已给任务栏留空、已含用户设定的底距）。
///
/// `bottom_offset`：Dock 底边距基准底边**多少物理像素** —— 由用户决定（配置项
/// `bottom_offset`，逻辑像素）。0 = 贴底。
///
/// 基准底边 `bottom` 的取法：
///  - 任务栏常显时，`rcWork` 已经排除了它，直接用 `wa.bottom`；
///  - 任务栏**自动隐藏**时 `rcWork` 等于整屏，但任务栏一触底就会弹出来盖住 Dock，
///    所以要手动扣掉它的高度（Q1：与任务栏并存）。
pub fn dock_rect_for_size(
    hwnd: HWND,
    w: i32,
    h: i32,
    reserve_taskbar: bool,
    bottom_offset: i32,
) -> RECT {
    let _ = hwnd;
    let wa = work_area();

    let mut bottom = wa.bottom;
    if reserve_taskbar {
        if let Some(tb) = taskbar_info() {
            // 自动隐藏的任务栏：rcWork 是整屏（**不管此刻弹没弹出来**），要自己扣。
            // 再加一道保险：只有当工作区真的顶到屏幕底边时才扣 ——
            // 万一某个系统/外壳已经把任务栏排除了，这里就不能扣第二遍。
            let screen_bottom = unsafe { GetSystemMetrics(SM_CYSCREEN) };
            if tb.auto_hidden && tb.at_bottom && wa.bottom >= screen_bottom {
                bottom = (bottom - tb.height).max(wa.top + h);
            }
        }
    }

    let wa_w = wa.right - wa.left;
    let x = wa.left + (wa_w - w) / 2;
    // 往上抬 bottom_offset；`clamp` 的 min 是 wa.top，所以抬太高会被挡在屏内
    let y = bottom - h - bottom_offset.max(0);

    RECT {
        left: x.clamp(wa.left, (wa.right - w).max(wa.left)),
        top: y.clamp(wa.top, bottom - h),
        right: x + w,
        bottom: y + h,
    }
}

/// 逻辑尺寸 → 物理尺寸
pub fn logical_to_physical(hwnd: HWND, w_log: f64, h_log: f64) -> (i32, i32) {
    let scale = dpi_of(hwnd) as f64 / 96.0;
    (
        (w_log * scale).round() as i32,
        (h_log * scale).round() as i32,
    )
}

/// 按逻辑尺寸设置 Dock 的尺寸与位置。
///
/// 圆角不在这里做 —— 亚克力铺满整个窗口矩形，只能靠 `glass::apply_window_material`
/// 里的 `DWMWCP_ROUND` 让 DWM 裁窗户本身（`SetWindowRgn` 对它无效，见 `glass.rs` 的模块说明）。
/// 返回最终生效的物理矩形。
pub fn layout_dock(
    hwnd: HWND,
    w_log: f64,
    h_log: f64,
    reserve_taskbar: bool,
    bottom_offset_logical: u32,
) -> RECT {
    let (w, h) = logical_to_physical(hwnd, w_log, h_log);
    let rect = dock_rect_for_size(
        hwnd,
        w,
        h,
        reserve_taskbar,
        logical_px(hwnd, bottom_offset_logical as f64),
    );
    unsafe {
        // ❗**不设 TOPMOST**：Dock 属于桌面那一层，用户的窗口要在它上面
        // （"Dock 只在桌面上，其他窗口都在它之上"）。`SWP_NOZORDER` 保证
        // 每次布局都不会把窗口又抬到最前面。
        let _ = SetWindowPos(
            hwnd,
            None,
            rect.left,
            rect.top,
            w,
            h,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
    rect
}

// `dock_rect_physical()` 已删除：它是 `dock_rect_for_size()` 的重复实现
// （逻辑尺寸换算 + 同一套 rcWork/任务栏取值），上一次重构后就没人调用了。
// 定点测试那个坑（逻辑坐标交给系统会被 125% 放大到屏外）记在 `logical_to_physical` 上。
