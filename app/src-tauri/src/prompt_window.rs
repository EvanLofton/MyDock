//! 命名输入框窗口（「新建文件夹…」/「重命名…」）。
//!
//! # 为什么是一个**自己的窗口**
//!
//! 浮层窗口（菜单 / 大文件夹 / 悬停标签）都是 `WS_EX_NOACTIVATE` ——
//! 那是"点 Dock 不夺走当前应用焦点"这条核心特性的前提（见 `win_layer.rs`），
//! 代价是**它们打不了字**：没有焦点就没有键盘输入。
//!
//! 第一版用一个手搓的 Win32 对话框顶上了，两个问题：
//! 1. **丑**：系统控件和这个应用（全是玻璃观感的自绘界面）完全不是一套；
//! 2. **内容显示不全**：`CreateWindowExW` 的宽高是**整个窗口**（含标题栏/边框）的尺寸，
//!    而我是按**客户区**排的版 —— 底部按钮被裁掉了。
//!
//! 现在改成和浮层同一条路：一个**页面自己画**的窗口
//! （`prompt.html` + `src/prompt/`），亚克力与 DWM 圆角由 Rust 侧铺（见 `glass.rs`）。
//!
//! # 与浮层窗口的三个关键区别
//!
//! | | 浮层 | 命名输入框 |
//! |---|---|---|
//! | 扩展样式 | `WS_EX_NOACTIVATE`（不夺焦点） | **不设** —— 它要靠焦点才能打字 |
//! | 显示时机 | 页面渲染完回调 `panel_ready` 再显示 | 同样等页面就绪（避免先看到上一次的内容） |
//! | 生命周期 | 启动时建好、常驻隐藏（右键是高频动作） | **用完即毁**（改名是低频动作，常驻白占 ~50MB） |
//!
//! # 等待模型
//!
//! 调用方（工作线程）发一个请求，然后**阻塞等**结果：
//! `present()` 里 `rx.recv()`，页面提交/取消时通过命令把值发回来（`RESULT` 那个 channel）。
//! 窗口被用户叉掉也算取消（`WM_CLOSE` → 页面收不到，所以这里注册 `on_window_event`）。

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use windows::Win32::Foundation::HWND;

use crate::glass;

/// 窗口标签（Rust 侧取窗口、自检都用它）
pub const LABEL: &str = "prompt";

/// 逻辑尺寸：与 `prompt.css` 的排版对得上（上一版就是这里算错才裁掉按钮的）。
///
/// 组成：上下内边距 16+14、标题 19+10、说明 16+6、输入框 30、按钮行 28 ≈ 139，
/// 再加 17 的呼吸位 = 156 —— **宽度是固定值**（标题、说明、输入框都能自适应），
/// 高度按内容算死，所以不会出现滚动条，也不会裁。
///
/// ❗这两个数改了必须回头看 `prompt.css`：按钮行是 `margin-top: auto` 顶到底边的，
/// 窗口给多了就是输入框和按钮之间空一大块（第一版 178 就空得像个没画完的框），
/// 给少了直接把按钮裁掉。CSS 里的内边距只影响**内部**排版，Rust 这边不参与计算
/// （宽度写死 360，够放下任何长度的说明文字）。
const W: f64 = 360.0;
const H: f64 = 156.0;
/// 与 Dock 顶边的间距
const GAP_ABOVE_DOCK: f64 = 10.0;
/// 页面渲染完成回调的兜底超时
const READY_TIMEOUT: Duration = Duration::from_millis(400);

/// 本次要问什么
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptPayload {
    pub seq: u64,
    pub title: String,
    pub label: String,
    pub value: String,
    pub max_length: usize,
}

struct Open {
    payload: PromptPayload,
    /// 提交/取消时把结果送回去；`None` = 取消
    tx: Sender<Option<String>>,
}

static CUR: Mutex<Option<Open>> = Mutex::new(None);
static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 页面来读内容（`commands::prompt_state`）
pub fn payload() -> Option<PromptPayload> {
    CUR.lock().unwrap().as_ref().map(|o| o.payload.clone())
}

/// 页面渲染完成，可以显示窗口了（`commands::prompt_ready`）
pub fn ready(app: &AppHandle, seq: u64) {
    if CUR.lock().unwrap().as_ref().map(|o| o.payload.seq) != Some(seq) {
        return; // 过期回调
    }
    show_window(app);
}

/// 页面提交了内容（`commands::prompt_submit`）
pub fn submit(text: String) {
    finish(Some(text));
}

/// 页面点了取消 / 按了 Esc（`commands::prompt_cancel`）
pub fn cancel() {
    finish(None);
}

fn finish(value: Option<String>) {
    let open = CUR.lock().unwrap().take();
    if let Some(o) = open {
        let _ = o.tx.send(value);
    }
}

/// 关掉窗口（提交/取消后调；用户叉掉窗口时也走这里）
fn close_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.close();
    }
}

/// 显示窗口（在 owner 上方居中）
fn show_window(app: &AppHandle) {
    let Some(w) = app.get_webview_window(LABEL) else {
        return;
    };
    let Ok(raw) = w.hwnd() else { return };
    let hwnd = HWND(raw.0 as *mut std::ffi::c_void);
    let scale = crate::win_layer::dpi_of(hwnd) as f64 / 96.0;
    let (pw, ph) = (
        (W * scale).round() as i32,
        (H * scale).round() as i32,
    );
    let wa = crate::win_layer::work_area();
    let dock = crate::commands::dock_hwnd(app.clone())
        .map(|h| crate::win_layer::window_rect(HWND(h as *mut std::ffi::c_void)))
        .unwrap_or(wa);
    let gap = (GAP_ABOVE_DOCK * scale).round() as i32;
    let x = (dock.left + (dock.right - dock.left - pw) / 2)
        .clamp(wa.left + 8, (wa.right - pw - 8).max(wa.left + 8));
    let mut y = dock.top - ph - gap;
    if y < wa.top + 8 {
        y = (dock.bottom + gap).min((wa.bottom - ph - 8).max(wa.top + 8));
    }

    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
            hwnd,
            None,
            x,
            y,
            pw,
            ph,
            windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE
                | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER
                | windows::Win32::UI::WindowsAndMessaging::SWP_SHOWWINDOW,
        );
        // 这是**要打字**的窗口：必须抢到前台 + 把焦点给它
        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(hwnd));
    }
}

/// 弹出输入框并**阻塞等结果**（调用方必须是工作线程，不能是主线程）。
///
/// 返回 `Some(文本)` = 用户点了确定；`None` = 取消 / 关窗 / 建不出窗口。
pub fn ask(
    app: &AppHandle,
    title: &str,
    label: &str,
    default_value: &str,
    max_length: usize,
) -> Option<String> {
    // 同一时刻只允许一个
    if CUR.lock().unwrap().is_some() {
        return None;
    }
    let (tx, rx): (Sender<Option<String>>, Receiver<Option<String>>) = channel();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let payload = PromptPayload {
        seq,
        title: title.to_string(),
        label: label.to_string(),
        value: default_value.to_string(),
        max_length,
    };
    *CUR.lock().unwrap() = Some(Open {
        payload: payload.clone(),
        tx,
    });

    // 建窗口必须在主线程（Tauri 的窗口 API 都在主线程），调用方是工作线程 → 投递过去等就绪
    let (ok_tx, ok_rx) = channel::<bool>();
    let a = app.clone();
    let p = payload.clone();
    if app
        .run_on_main_thread(move || {
            let r = create_and_notify(&a, &p);
            let _ = ok_tx.send(r.is_ok());
            if let Err(e) = r {
                log_error!("[命名] 创建窗口失败: {e}");
            }
        })
        .is_err()
    {
        finish(None);
        close_window(app);
        return None;
    }
    if ok_rx.recv_timeout(Duration::from_secs(10)) != Ok(true) {
        finish(None);
        close_window(app);
        return None;
    }

    // 等用户回答。给一个很宽松的上限（用户可能去接杯水），但不无限等 ——
    // 无限等的线程会一直挂着，万一窗口被别的东西弄没了就没人叫醒它。
    let out = rx.recv_timeout(Duration::from_secs(600)).ok().flatten();
    close_window(app);
    out
}

/// 建窗口（若已存在则复用）→ 让页面读内容 → 等它报就绪。
fn create_and_notify(app: &AppHandle, payload: &PromptPayload) -> Result<(), String> {
    if app.get_webview_window(LABEL).is_none() {
        let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("prompt.html".into()))
            .title("Dock 命名")
            // 尺寸只是初值：显示时按内容重算（见 `show_window`）
            .inner_size(W, H)
            .transparent(true)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .shadow(false)
            // 这个窗口**要**能拿焦点（要打字）—— 所以 `focused(true)`，
            // 且**不**套 `apply_dock_ex_style`（那会加 WS_EX_NOACTIVATE）。
            .focused(true)
            .visible(false)
            .build()
            .map_err(|e| format!("创建命名窗口失败: {e}"))?;

        let raw = win.hwnd().map_err(|e| e.to_string())?;
        let hwnd = HWND(raw.0 as *mut std::ffi::c_void);
        log_info!("[命名] 窗口已创建 0x{:X}", raw.0 as usize);

        // 不进 Alt+Tab（工具窗口），但**保留可激活**
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{
                GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_TOOLWINDOW,
            };
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_TOOLWINDOW.0 as isize);
        }
        // 与浮层同材质：DWM 圆角 + 去边框 + 亚克力（浓度取浮层同一套下限）
        glass::apply_window_material(hwnd);
        let prefs = app
            .state::<crate::store::PrefsState>()
            .0
            .lock()
            .unwrap()
            .clone();
        let alpha = prefs.glass_alpha.clamp(150, 200);
        let diag = glass::apply_acrylic(
            hwnd,
            (prefs.glass_rgb[0], prefs.glass_rgb[1], prefs.glass_rgb[2], alpha),
        );
        log_info!("[命名] 亚克力 {diag}");

        // 用户直接叉掉窗口 = 取消
        let a = app.clone();
        win.on_window_event(move |e| {
            if let tauri::WindowEvent::CloseRequested { .. } = e {
                finish(None);
                let _ = &a;
            }
        });
    }

    let win = app
        .get_webview_window(LABEL)
        .ok_or_else(|| "命名窗口不存在".to_string())?;
    win.eval(&format!("window.__dockPrompt && window.__dockPrompt({});", payload.seq))
        .map_err(|e| format!("通知命名页面失败: {e}"))?;

    // 兜底：页面没回调也要显示（否则用户点了「新建文件夹…」什么都不发生）
    let a = app.clone();
    let seq = payload.seq;
    std::thread::spawn(move || {
        std::thread::sleep(READY_TIMEOUT);
        let still = CUR.lock().unwrap().as_ref().map(|o| o.payload.seq) == Some(seq);
        if still {
            show_window(&a);
        }
    });
    Ok(())
}
