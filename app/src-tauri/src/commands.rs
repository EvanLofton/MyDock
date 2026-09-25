//! Tauri 命令：前端唯一能触达平台层的入口。
//!
//! 约定：错误一律返回**具体且可操作**的信息（`NeedsAdmin` / `ForegroundDenied` 等语义），
//! 前端据此给出明确提示，绝不静默失败。

use tauri::ipc::Response;
use tauri::Manager;
use windows::Win32::Foundation::HWND;

use crate::model::AppEntry;

/// Dock 自身的窗口句柄（右键菜单需要它作为 owner）
#[tauri::command]
pub fn dock_hwnd(app: tauri::AppHandle) -> Option<u64> {
    app.get_webview_window("dock")
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as u64)
}

/// 自检回执：前端把 IPC 测试结果写回 Rust，供自检读取。
/// 这样就能验证「JS → invoke → Rust 命令 → Win32」整条链路，
/// 而不用依赖 window title 之类的间接通道。
///
/// 生产运行时前端**不会**调它（只有 `--features selftest` 的自动化会注入调用），
/// 留着是因为它必须出现在 `invoke_handler` 注册表里才能被前端触达。
static TEST_REPORT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

#[tauri::command]
pub fn selftest_report(msg: String) {
    *TEST_REPORT.lock().unwrap() = msg;
}

#[cfg(feature = "selftest")]
pub fn take_test_report() -> String {
    std::mem::take(&mut *TEST_REPORT.lock().unwrap())
}

/// 前端上报 Dock 的**逻辑**尺寸；Rust 据此调整窗口大小与位置，并把四角裁圆。
///
/// 为什么必须由前端上报：Dock 面板宽度 = 图标行宽 + 内边距，随应用数量变化；
/// 而亚克力是**按整个窗口矩形**铺的 —— 窗口比面板大，就会在面板周围露出一块灰底
/// （这正是用户反馈的「灰色背景上才是 Dock」）。所以窗口尺寸必须跟着面板走。
#[tauri::command]
pub fn set_dock_size(
    app: tauri::AppHandle,
    width: f64,
    height: f64,
    radius: f64,
) -> Result<(), String> {
    // radius 目前用不到（圆角交给 DWM），保留参数以便前端与 CSS 保持同一份配置
    let _ = radius;
    let win = app
        .get_webview_window("dock")
        .ok_or("找不到 dock 窗口")?;
    let raw = win.hwnd().map_err(|e| e.to_string())?;
    let hwnd = HWND(raw.0 as *mut core::ffi::c_void);

    // ❗记下「前端已经报过尺寸了」：`supervise` 里的窗口层初始化是**异步**投递到主线程的，
    // 如果它排在这一步之后（启动时创建多个 WebView 会占住主线程几秒），
    // 那句"先用初值 520 定位"就会把这里刚上报的真实宽度**覆盖掉**，
    // 而且之后不会再有人上报（宽度没变）—— Dock 就永久停在初值宽度
    // （症状：面板比内容宽一大截，两边全是空的）。本轮真踩了。
    crate::win_layer::mark_size_reported();

    let (reserve, bottom_offset) = {
        // `app.state()` 返回临时值，必须先绑定（E0716）
        let state = app.state::<crate::store::PrefsState>();
        let p = state.0.lock().unwrap();
        (p.reserve_taskbar, p.bottom_offset)
    };

    let rect = crate::win_layer::layout_dock(hwnd, width, height, reserve, bottom_offset);
    crate::reveal::set_target(rect);
    Ok(())
}

#[tauri::command]
pub fn get_preferences(app: tauri::AppHandle) -> crate::store::Preferences {
    app.state::<crate::store::PrefsState>().0.lock().unwrap().clone()
}

/// 前端（WebView 里的 React）把错误/警告转进 Dock 的日志文件。
///
/// 为什么需要这条通道：WebView 是**独立进程** —— 页面里的未捕获异常、
/// `console.error`、React 渲染报错都只活在那边，Dock 的日志本来一个字都看不到。
/// 而"点了没反应 / 图标全没了 / 页面一片空白"恰恰全在页面里。
/// 前端在 `window.onerror` / `unhandledrejection` / `console.error` 里调它
/// （见 `src/lib/log.ts`）。
#[tauri::command]
pub fn js_log(level: String, page: String, msg: String) {
    crate::logging::from_frontend(&level, &page, &msg);
}

/// 保存配置，并把「立即生效」的项同步给运行时（不重启就生效）。
#[tauri::command]
pub fn set_preferences(
    app: tauri::AppHandle,
    mut prefs: crate::store::Preferences,
) -> Result<(), String> {
    // ⚠️ **Dock 上的应用列表一律以内存里的为准，不接受来自前端的覆盖。**
    //
    // `pinned`（= Dock 上的图标列表）是 Dock 自己维护的（用户拖放/添加/移除的）。
    // 前端只是「顺带」拿到它，一旦某次 UI 更新漏传这个字段，就会把用户添加的
    // 应用整个清空 —— 这种误伤不值得为了少写一个命令去冒。
    // 要改这个列表请走托盘的「添加应用…」、从桌面拖放、或右键「从 Dock 移除」。
    {
        let cur = app.state::<crate::store::PrefsState>().0.lock().unwrap().clone();
        prefs.pinned = cur.pinned;
    }

    crate::store::save(&app, &prefs)?;
    *app.state::<crate::store::PrefsState>().0.lock().unwrap() = prefs.clone();

    // 开机自启：设置页和托盘菜单都能改这项，**都走 store::set_launch_at_login**
    // （写注册表 + 落配置 + 同步托盘勾选）。这里只在"想要的与注册表里的实际状态不一致"
    // 时才动手 —— 免得每次改别的设置都去写一遍注册表。
    if prefs.launch_at_login != crate::autostart::is_enabled() {
        if let Err(e) = crate::store::set_launch_at_login(&app, prefs.launch_at_login) {
            log_error!("[配置] 应用开机自启失败: {e}");
        }
    }

    apply_live(&app, &prefs);
    Ok(())
}

/// 把「改了必须立刻看到」的配置项应用到运行中的 Dock。
///
/// 为什么需要它：Dock 的背景是**两层**叠出来的 ——
///  1. OS 层的亚克力着色（`SetWindowCompositionAttribute`，由 Rust 调）
///  2. 前端 `.dock` 的一层半透明遮罩（由 CSS 决定，前端自己会重渲染）
///
/// 第 2 层改完前端立刻会变；**第 1 层必须重新调一次 API 才生效**，
/// 所以配置一变就得在这里重新应用一遍，否则用户会看到「只有一半变了」。
fn apply_live(app: &tauri::AppHandle, prefs: &crate::store::Preferences) {
    // 自动隐藏线程每帧都读这个标志，改完立刻生效
    crate::reveal::set_enabled(prefs.auto_hide);

    let Some(win) = app.get_webview_window("dock") else {
        return;
    };
    let Ok(raw) = win.hwnd() else {
        return;
    };
    let hwnd = HWND(raw.0 as *mut core::ffi::c_void);
    let r = crate::glass::apply_acrylic(
        hwnd,
        (
            prefs.glass_rgb[0],
            prefs.glass_rgb[1],
            prefs.glass_rgb[2],
            prefs.glass_alpha,
        ),
    );
    log_info!("[配置] 毛玻璃已重新应用: {r}");

    // 第 3 件：位置类参数（距底部距离）改了要让窗口**立刻挪过去**。
    //
    // 不需要记住逻辑尺寸 —— 当前窗口的宽高就是对的，只要按新的底距重算 y 再
    // 交给 reveal 线程即可（它每帧比对目标矩形，变了就 SetWindowPos）。
    let cur = crate::win_layer::window_rect(hwnd);
    let rect = crate::win_layer::dock_rect_for_size(
        hwnd,
        cur.right - cur.left,
        cur.bottom - cur.top,
        prefs.reserve_taskbar,
        crate::win_layer::logical_px(hwnd, prefs.bottom_offset as f64),
    );
    crate::reveal::set_target(rect);

    // 第 2 层（前端 CSS 遮罩）的颜色也来自同一份配置，得让 Dock 页面重读一次。
    // 这里用 `eval` 是 **Rust → 页面** 的单向调用，不经过前端 invoke，
    // 所以**不需要**任何 capability（与「前端 listen 事件」那条路不同，见 backlog BL-1）。
    // 没有这个的话，Dock 要等下一次轮询（最多 1.5 秒）才跟上，拖色板时会明显滞后。
    let _ = win.eval("window.__dockReloadPrefs && window.__dockReloadPrefs();");
}

/// Dock 的基础信息，供设置页展示。
///
/// 全是**只读**的运行时事实 —— 设置页只显示，不从这里改任何东西。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockInfo {
    pub version: String,
    pub config_path: String,
    pub hwnd: String,
    /// Dock 上有多少个图标（= 用户列表里的应用数，**不含分割线**）
    pub pinned_count: usize,
    /// 其中有多少条分割线（纯视觉分组，见 `store::add_separator`）
    pub separator_count: usize,
    /// 当前正在运行的应用数（实时枚举；**不影响 Dock 上显示什么**，只用来画白点）
    pub running_count: usize,
    /// 窗口 DPI 与缩放比（本机 125% → dpi 120 / scale 1.25）
    pub dpi: u32,
    pub scale: f64,
    /// Dock 窗口物理矩形 `[left, top, right, bottom]`
    pub dock_rect: [i32; 4],
    /// Dock 面板逻辑尺寸 `[宽, 高]`
    pub dock_logical: [f64; 2],
    /// 屏幕工作区（`rcWork`）物理矩形
    pub work_area: [i32; 4],
    pub auto_hide: bool,
    pub hide_delay_ms: u64,
    pub icon_size: u32,
    /// 配置里的面板高度（逻辑像素）。**实际生效值可能更大** —— 前端会按图标尺寸兜底钳制
    pub panel_height: u32,
    /// 图标之间的间距（逻辑像素）
    pub icon_gap: u32,
    pub magnification: f64,
    pub show_running_indicators: bool,
    pub reserve_taskbar: bool,
    /// Dock 底边距基准底边的逻辑像素数（用户可调）
    pub bottom_offset: u32,
    pub launch_at_login: bool,
    pub glass_rgb: [u8; 3],
    pub glass_alpha: u8,
}

#[tauri::command]
pub fn get_dock_info(app: tauri::AppHandle) -> DockInfo {
    let prefs = app
        .state::<crate::store::PrefsState>()
        .0
        .lock()
        .unwrap()
        .clone();

    let (hwnd, dpi, scale, rect) = match app.get_webview_window("dock").and_then(|w| w.hwnd().ok())
    {
        Some(raw) => {
            let h = HWND(raw.0 as *mut core::ffi::c_void);
            let dpi = crate::win_layer::dpi_of(h);
            let r = crate::win_layer::window_rect(h);
            (format!("0x{:X}", raw.0 as usize), dpi, dpi as f64 / 96.0, r)
        }
        None => (String::from("(无)"), 96, 1.0, Default::default()),
    };
    let wa = crate::win_layer::work_area();

    let w = (rect.right - rect.left) as f64;
    let h = (rect.bottom - rect.top) as f64;

    DockInfo {
        version: app.package_info().version.to_string(),
        config_path: crate::store::config_path(&app).display().to_string(),
        hwnd,
        pinned_count: prefs.pinned.iter().filter(|p| !p.separator).count(),
        separator_count: prefs.pinned.iter().filter(|p| p.separator).count(),
        running_count: crate::apps::enumerate_apps().len(),
        dpi,
        scale,
        dock_rect: [rect.left, rect.top, rect.right, rect.bottom],
        dock_logical: [
            (w / scale).round(),
            (h / scale).round(),
        ],
        work_area: [wa.left, wa.top, wa.right, wa.bottom],
        auto_hide: prefs.auto_hide,
        hide_delay_ms: prefs.hide_delay_ms,
        icon_size: prefs.icon_size,
        panel_height: prefs.panel_height,
        icon_gap: prefs.icon_gap,
        magnification: prefs.magnification,
        show_running_indicators: prefs.show_running_indicators,
        reserve_taskbar: prefs.reserve_taskbar,
        bottom_offset: prefs.bottom_offset,
        launch_at_login: prefs.launch_at_login,
        glass_rgb: prefs.glass_rgb,
        glass_alpha: prefs.glass_alpha,
    }
}

/// 打开设置窗口（托盘菜单与 Dock 右键都会用到）。
#[tauri::command]
pub fn open_settings(app: tauri::AppHandle) -> Result<(), String> {
    crate::settings_window::open(&app)
}

#[tauri::command]
pub fn list_apps(app: tauri::AppHandle) -> Vec<AppEntry> {
    let dock_apps = app
        .state::<crate::store::PrefsState>()
        .0
        .lock()
        .unwrap()
        .pinned
        .clone();
    let out = crate::apps::enumerate_dock_apps(&dock_apps);

    // DOCK_DEBUG=1：打印 Dock 最终要显示的列表。
    // 排查「两个图标」「该有运行点却没有」时，这是唯一能看清 Dock 到底收到了什么的地方。
    if std::env::var("DOCK_DEBUG").as_deref() == Ok("1") {
        log_debug!("---- list_apps: {} 条 ----", out.len());
        for a in &out {
            log_info!(
                "  {:<18} running={:<5} 前台={:<5} 窗口={}  id={}",
                a.display_name, a.running, a.has_foreground, a.windows.len(), a.id
            );
            for w in &a.windows {
                let t: String = w.title.chars().take(40).collect();
                log_debug!("      hwnd=0x{:X} 最小化={:<5} '{}'", w.hwnd, w.minimized, t);
            }
        }
    }
    out
}

/// 保存 Dock 上图标的新顺序（Dock 内拖拽排序结束时调用）。
///
/// 校验在 `store::reorder` 里：顺序必须与现有列表完全一致，否则整条拒绝、不写盘。
#[tauri::command]
pub fn reorder_dock_apps(app: tauri::AppHandle, ids: Vec<String>) -> Result<(), String> {
    crate::store::reorder(&app, &ids)
}

/// 从 Dock 上移除一个图标（把图标拖出 Dock 松手时调用；
/// 右键菜单里的「移出 Dock」走的是 `store::remove` 的同一条路）。
#[tauri::command]
pub fn remove_dock_app(app: tauri::AppHandle, id: String) -> Result<(), String> {
    crate::store::remove(&app, &id)
}

/// 把一个系统位置（此电脑 / 回收站…）加到 Dock 最左侧。
///
/// `target` 用 Shell 命名空间路径（`shell:MyComputerFolder` / `shell:RecycleBinFolder`），
/// 白名单在 `apps::SYSTEM_LOCATIONS`；别的值会被拒绝（不认识的 `shell:` 路径
/// 打开行为不可预期，不如不做）。
#[tauri::command]
pub fn add_system_location(app: tauri::AppHandle, target: String) -> Result<String, String> {
    let id = crate::store::add_system_location(&app, &target)?;
    crate::drop::refresh_apps(&app);
    Ok(id)
}

/// 命名输入框页面读内容（`prompt_window::present` 用 `eval` 通知它来读）。
#[tauri::command]
pub fn prompt_state() -> Option<crate::prompt_window::PromptPayload> {
    crate::prompt_window::payload()
}

/// 命名输入框页面渲染完成 → 可以显示窗口了。
#[tauri::command]
pub fn prompt_ready(app: tauri::AppHandle, seq: u64) {
    crate::prompt_window::ready(&app, seq);
}

/// 用户点了「确定」。
#[tauri::command]
pub fn prompt_submit(text: String) {
    crate::prompt_window::submit(text);
}

/// 用户点了「取消」/ 按了 Esc。
#[tauri::command]
pub fn prompt_cancel() {
    crate::prompt_window::cancel();
}

/// 回收站里现在有多少项（BL-8）。
///
/// 前端拿它当回收站图标的**版本号**：图标在页面里按 target 缓存，
/// 计数变了才能让缓存作废、重取那张「空 / 满」不同的图标。
/// 只有 Dock 上真的有回收站时前端才会调它（一次 shell 查询，很便宜）。
#[tauri::command]
pub fn get_recycle_bin_items() -> u32 {
    crate::apps::recycle_bin_items()
}

/// 在 `after_id` 后面加一条分割线（右键菜单里的「添加分割线」）。
///
/// 返回新条目的 id。界面刷新不靠返回值 —— 这里直接让 Dock 重画一次
/// （`refresh_apps` 正是拖放添加应用之后用的那条路），不必等下一次轮询。
#[tauri::command]
pub fn add_separator(app: tauri::AppHandle, after_id: Option<String>) -> Result<String, String> {
    let id = crate::store::add_separator(&app, after_id.as_deref())?;
    crate::drop::refresh_apps(&app);
    Ok(id)
}

/// 弹出文件选择器，把选中的程序加到 Dock 上。
/// 用 `spawn_blocking`：文件对话框是模态的，会阻塞调用线程。
#[tauri::command]
pub async fn pick_and_pin_app(app: tauri::AppHandle) -> Result<(), String> {
    crate::reveal::pause(); // 选择期间不要自动隐藏
    let a = app.clone();
    let r = tauri::async_runtime::spawn_blocking(move || crate::picker::pick_and_pin(&a))
        .await
        .map_err(|e| format!("选择器线程失败: {e}"))?;
    crate::reveal::unpause();
    r
}

/// 返回 `[width:u32 LE][height:u32 LE][BGRA...]` 的原始字节。
/// 用 `Response` 走二进制通道，避免几十 KB 的图标被序列化成 JSON 数字数组。
/// 空返回表示取图标失败。
#[tauri::command]
pub fn get_icon_bgra(path: String, size: u32) -> Response {
    match crate::icons::extract_icon(&path, size) {
        Some(d) => {
            // `DOCK_DEBUG=1` 时打一行：排查"某个图标为什么没跟着状态变"（如 BL-8 的回收站）
            // 时，这一行是**唯一**能看出"前端到底有没有重新来取"的地方 ——
            // 页面里的图标是缓存的，从外面看不见它有没有重取。
            if std::env::var("DOCK_DEBUG").as_deref() == Ok("1") {
                log_debug!("[图标] 取 {path} @{size} → {}x{}", d.width, d.height);
            }
            let mut v = Vec::with_capacity(8 + d.bgra.len());
            v.extend_from_slice(&d.width.to_le_bytes());
            v.extend_from_slice(&d.height.to_le_bytes());
            v.extend_from_slice(&d.bgra);
            Response::new(v)
        }
        None => {
            if std::env::var("DOCK_DEBUG").as_deref() == Ok("1") {
                log_debug!("[图标] 取 {path} @{size} → 失败（前端会退成首字母占位）");
            }
            Response::new(Vec::<u8>::new())
        }
    }
}

#[tauri::command]
pub fn activate_app(hwnd: u64) -> Result<(), String> {
    crate::apps::activate(HWND(hwnd as *mut core::ffi::c_void))
}

#[tauri::command]
pub fn minimize_app(hwnd: u64) -> Result<(), String> {
    crate::apps::minimize(HWND(hwnd as *mut core::ffi::c_void))
}

#[tauri::command]
pub fn close_app(hwnd: u64) -> Result<(), String> {
    crate::apps::close_window(HWND(hwnd as *mut core::ffi::c_void))
}

#[tauri::command]
pub fn launch_app(target: String, run_as_admin: bool) -> Result<(), String> {
    crate::apps::launch(&target, run_as_admin)
}

/// 弹出右键菜单（浮层窗口，见 `panel_window.rs`）。
///
/// `anchor_x` 是**光标在 Dock 客户端里的逻辑横坐标** —— 浮层以它为中心。
/// 用光标而不是图标中心：鱼眼放大时图标会平移，光标才是用户眼里「我点的地方」。
///
/// `anchor_x = None` 表示**由 Rust 现取光标位置**：从文件夹里右键（点里面的图标）
/// 时用得上 —— 那时光标在浮层窗口上，页面拿不到"Dock 客户端坐标"这个量。
/// 同时 `None` 也代表"这次右键来自文件夹内部"，所以**文件夹层要留着**
/// （用户明确要求：在文件夹里右键，文件夹不该关）。
///
/// 返回值 = 调用之后菜单是不是打开的。同一个图标再右键一次会关掉它
/// （`false`），前端不需要自己记「菜单开着没有」—— 那会有两个真相来源。
#[tauri::command]
pub fn show_panel_menu(
    app: tauri::AppHandle,
    owner_hwnd: u64,
    app_id: String,
    anchor_x: Option<f64>,
) -> Result<bool, String> {
    let target = menu_target(&app, &app_id)?;
    let (anchor, keep_folder) = match anchor_x {
        Some(x) => (x, false),
        None => (cursor_x_in_dock(owner_hwnd).unwrap_or(0.0), true),
    };
    crate::panel_window::show_menu(&app, owner_hwnd, target, anchor, keep_folder)
}

/// 光标当前落在 Dock 客户端的哪个逻辑横坐标上。
///
/// 用途：从浮层里右键某个图标时，把菜单摆到同一位置（不然菜单会跳到 Dock 左边）。
/// 取不到就返回 `None`，调用方退化成 0（至少还能弹出来）。
fn cursor_x_in_dock(dock_hwnd: u64) -> Option<f64> {
    use windows::Win32::Graphics::Gdi::ScreenToClient;

    let dock = HWND(dock_hwnd as *mut core::ffi::c_void);
    if dock.0.is_null() {
        return None;
    }
    let mut pt = windows::Win32::Foundation::POINT::default();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
        let _ = ScreenToClient(dock, &mut pt);
    }
    let scale = crate::win_layer::dpi_of(dock) as f64 / 96.0;
    Some(pt.x as f64 / scale)
}

/// 展开一个**文件夹**（Dock 大文件夹点开后那个图标网格）。
///
/// 和右键菜单共用同一个浮层窗口 —— 同一时刻只可能有一个浮层（见 `panel_window.rs`）。
#[tauri::command]
pub fn show_panel_folder(
    app: tauri::AppHandle,
    owner_hwnd: u64,
    folder_id: String,
    anchor_x: f64,
) -> Result<bool, String> {
    let folder = folder_entry(&app, &folder_id)?;
    if !folder.is_folder {
        return Err("它不是文件夹".into());
    }
    crate::panel_window::show_folder(
        &app,
        owner_hwnd,
        &folder.id,
        &folder.display_name,
        &folder.children,
        anchor_x,
    )
}

/// 鼠标停在 Dock 图标上 → 在**图标上方**显示名字（macOS 那种小气泡）。
///
/// 为什么不用 `title`：那是 WebView2/系统原生 tooltip —— 方角、系统配色、
/// 出现在光标右下角，和 Dock 的玻璃观感完全不是一套。这里复用浮层窗口自己画。
///
/// `target_id` 是那个图标的 id（"同一个图标别重复弹"靠它判断）。
///
/// `in_folder` = 这个图标在**大文件夹里面**：标签要贴在文件夹面板上方（锚点是面板客户端坐标），
/// 不然一个屏幕底部的气泡会跑去指文件夹里的小格子。
#[tauri::command]
pub fn show_icon_label(
    app: tauri::AppHandle,
    owner_hwnd: u64,
    target_id: String,
    text: String,
    anchor_x: f64,
    in_folder: bool,
) -> Result<bool, String> {
    crate::panel_window::show_label(&app, owner_hwnd, &target_id, &text, anchor_x, in_folder)
}

/// 鼠标移开 → 关掉悬停标签（**不会**误关右键菜单：真的菜单留着）。
#[tauri::command]
pub fn hide_icon_label() {
    crate::panel_window::hide_label();
}

/// 关掉**所有**浮层（幂等）。Dock 页面上任何一次按下都会调它 ——
/// 那一下是给 Dock 的，所以菜单和文件夹一起收掉。
#[tauri::command]
pub fn hide_panel() {
    crate::panel_window::hide_all();
}

/// 浮层页面读取要渲染的内容。`null` = 已经关掉了。
///
/// 两个浮层窗口（文件夹 / 菜单）**共用同一个页面**，所以要用窗口标签区分
/// "是谁在问" —— 否则两边会拿到同一份内容。
#[tauri::command]
pub fn panel_state(window: tauri::WebviewWindow) -> Option<crate::panel_window::PanelPayload> {
    crate::panel_window::payload_for_label(window.label())
}

/// 浮层页面渲染完成，可以显示了（见 `panel_window.rs` 的显示时序说明）。
#[tauri::command]
pub fn panel_ready(window: tauri::WebviewWindow, seq: u64) {
    crate::panel_window::ready_for_label(window.label(), window.app_handle(), seq);
}

/// 执行一个菜单项（**菜单层**里的动作）。
///
/// 先取目标、再关**菜单层**、最后执行动作：浮层在动作之后才消失会让人以为
/// 「点了没反应」（尤其是启动程序这种要等一会儿的动作）。
///
/// ⚠️ 只关菜单层 —— 文件夹层要留着（用户明确要求）。
///
/// 用 `async` + `spawn_blocking`：`ShellExecuteW` 在冷启动一个程序时会阻塞，
/// 而「以管理员身份运行」还会等 UAC 对话框 —— 这些都不能占住 Tauri 的线程。
#[tauri::command]
pub async fn panel_invoke(app: tauri::AppHandle, item_id: String) -> Result<(), String> {
    // ⚠️ 顺序不能反：`hide()` 会把「当前浮层的目标」一起清掉，先取再关。
    // （第一版写反了 —— 浮层正常关闭，动作却静默不执行，看起来像"点了没反应"。）
    let Some(target) = crate::panel_window::current_target_id().and_then(|id| menu_target(&app, &id).ok())
    else {
        // 浮层已经关了（比如看门线程先一步关掉）—— 这不是错误，忽略即可
        return Ok(());
    };
    crate::panel_window::hide_menu();
    let r = run_panel_action(&app, move |a| crate::menu::run(a, &target, &item_id)).await;
    // 动作可能改了文件夹的内容（移出文件夹 / 移出 Dock），
    // 而文件夹还开着 —— 原地刷新一次，否则网格里还留着已经不在的那一项。
    refresh_open_folder(&app);
    r
}

/// 点了文件夹里的一个图标：**先关掉所有浮层再启动/激活**。
///
/// 和菜单不同，这一下是"我选好了" —— 文件夹该跟着关掉（iOS 上也是这样）。
///
/// ⚠️ 这里传给动作层的是**固定的菜单项 id `activate`**，不是 `item_id`
/// —— `item_id` 是"哪个条目"（应用 id），而 `menu::run` 的第三个参数是
/// "点了哪一项"。第一版把两者弄混了：`run` 收到一个应用 id 当作菜单项，
/// 落进 `_ => Err("未知的菜单项")`，于是**点文件夹里的图标完全没反应**
/// （错误还被 toast 掉，看起来就像"点了不开"）。
#[tauri::command]
pub async fn panel_launch(app: tauri::AppHandle, item_id: String) -> Result<(), String> {
    let Some(target) = menu_target(&app, &item_id).ok() else {
        return Ok(());
    };
    crate::panel_window::hide_all();
    run_panel_action(&app, move |a| {
        crate::menu::run(a, &target, crate::menu::id::ACTIVATE)
    })
    .await
}

/// 浮层动作的公共收尾：在阻塞线程里执行 → 让 Dock 重画 → 失败就弹 toast。
///
/// 抽出来是因为「动作会改变 Dock 上的东西」和「浮层已经关了、错误只能弹到 Dock 上」
/// 这两条对菜单项和文件夹图标都成立 —— 抄两遍必然会漏掉一处。
async fn run_panel_action<F>(app: &tauri::AppHandle, f: F) -> Result<(), String>
where
    F: FnOnce(&tauri::AppHandle) -> Result<(), String> + Send + 'static,
{
    let a = app.clone();
    let r = tauri::async_runtime::spawn_blocking(move || f(&a))
        .await
        .map_err(|e| format!("浮层动作线程失败: {e}"))?;

    // 启动 → 白点；移除/加分割线/进文件夹 → 图标变了，都要立刻重画
    crate::drop::refresh_apps(app);

    // 浮层已经关了，没有地方显示错误 —— 一律弹到 Dock 上（绝不静默失败）
    if let Err(e) = &r {
        log_error!("[浮层] 动作失败: {e}");
        crate::drop::toast(app, e);
    }
    r
}

/// 把一个条目变成文件夹（右键菜单里的「新建文件夹…」）。
///
/// `name` 是用户在弹出的输入框里填的名字（`None` = 用默认的「新建文件夹」）。
/// 界面上这个命令现在由**菜单项**触发（会先弹输入框，见 `store::spawn_create_folder`），
/// 这里留着是为了自检/脚本能直接建文件夹，不必去驱动那个模态框。
#[tauri::command]
pub fn create_folder(
    app: tauri::AppHandle,
    app_id: String,
    name: Option<String>,
) -> Result<String, String> {
    let id = crate::store::create_folder(&app, &app_id, name.as_deref())?;
    crate::drop::refresh_apps(&app);
    Ok(id)
}

/// 把一个顶层条目放进文件夹（把图标拖到文件夹上）。
#[tauri::command]
pub fn add_app_to_folder(
    app: tauri::AppHandle,
    folder_id: String,
    app_id: String,
) -> Result<(), String> {
    crate::store::add_to_folder(&app, &folder_id, &app_id)?;
    crate::drop::refresh_apps(&app);
    Ok(())
}

/// 解散文件夹：里面的条目回到顶层（右键菜单里的「解散文件夹」）。
#[tauri::command]
pub fn dissolve_folder(app: tauri::AppHandle, folder_id: String) -> Result<usize, String> {
    let n = crate::store::dissolve_folder(&app, &folder_id)?;
    crate::drop::refresh_apps(&app);
    Ok(n)
}

/// 按 id 在**顶层和文件夹里**找条目 —— 用户眼里的"这个图标"不分层级。
///
/// 只信 `app_id`，其余全从**当前真实的枚举结果**里取 —— 前端传来的运行状态、
/// 窗口句柄都可能已经过期（右键那一刻和点菜单那一刻之间程序可能已经退出了）。
pub fn menu_target(app: &tauri::AppHandle, app_id: &str) -> Result<crate::menu::Target, String> {
    use tauri::Manager;
    let dock_apps = app
        .state::<crate::store::PrefsState>()
        .0
        .lock()
        .unwrap()
        .pinned
        .clone();
    let all = crate::apps::enumerate_dock_apps(&dock_apps);
    let top = all.iter().find(|e| e.id == app_id);
    let (entry, in_folder) = match top {
        Some(e) => (e, false),
        None => {
            let child = all
                .iter()
                .filter(|e| e.is_folder)
                .flat_map(|f| f.children.iter())
                .find(|c| c.id == app_id)
                .ok_or_else(|| format!("Dock 上没有这个条目: {app_id}"))?;
            (child, true)
        }
    };

    Ok(crate::menu::Target {
        app_id: entry.id.clone(),
        target: entry.target.clone(),
        hwnd: entry.windows.first().map(|w| w.hwnd).unwrap_or(0),
        running: entry.running,
        is_elevated: entry.is_elevated,
        separator: entry.separator,
        is_folder: entry.is_folder,
        in_folder,
        children_count: entry.children.len(),
        // 「打开」还是「启动」、runas 能不能点，都取决于 target 是不是一个目录。
        // 在这里量好（`menu::build` 保持纯函数，单测不用碰文件系统）。
        is_dir: std::path::Path::new(&entry.target).is_dir(),
        // 「清空回收站」是否可点：空的时候置灰
        bin_items: if crate::apps::is_recycle_bin(&entry.target) {
            crate::apps::recycle_bin_items()
        } else {
            0
        },
    })
}

/// 把一个条目**移出文件夹**（右键文件夹里的图标 →「移出文件夹」）。
///
/// 语义是"挪到顶层"，不是删掉 —— 见 `store::move_out_of_folder` 的说明。
#[tauri::command]
pub fn move_out_of_folder(app: tauri::AppHandle, app_id: String) -> Result<(), String> {
    crate::store::move_out_of_folder(&app, &app_id)?;
    crate::drop::refresh_apps(&app);
    refresh_open_folder(&app);
    Ok(())
}

/// 文件夹层还开着的话，**原地刷新**它的内容。
///
/// 用途：在文件夹里右键操作过之后（移出文件夹 / 移出 Dock），
/// 文件夹是留着的（用户要求），但里面的东西变了 —— 不刷的话网格里还留着
/// 已经不在的那一项，点它会去启动一个已经被移走的应用。
///
/// 只在文件夹层可见时动手；锚点用**当前光标**（用户刚在它上面操作过，光标就在那儿），
/// 这样刷新前后位置不跳。
fn refresh_open_folder(app: &tauri::AppHandle) {
    if !crate::panel_window::folder_visible() {
        return;
    }
    let Some(folder_id) = crate::panel_window::current_folder_id() else {
        return;
    };
    let dock_hwnd = app
        .get_webview_window("dock")
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as u64)
        .unwrap_or(0);
    if dock_hwnd == 0 {
        return;
    }
    let Ok(folder) = folder_entry(app, &folder_id) else {
        // 文件夹本身没了（比如被解散）→ 直接关掉这一层
        crate::panel_window::hide_folder();
        return;
    };
    let anchor = cursor_x_in_dock(dock_hwnd).unwrap_or(0.0);
    if let Err(e) = crate::panel_window::show_folder_keep_menu(
        app,
        dock_hwnd,
        &folder.id,
        &folder.display_name,
        &folder.children,
        anchor,
    ) {
        // 空文件夹之类：没什么可展示的，收掉
        log_info!("[浮层] 刷新文件夹失败（{e}），关掉它");
        crate::panel_window::hide_folder();
    }
}

/// 按 id 找文件夹条目（`show_panel_folder` 用）
fn folder_entry(app: &tauri::AppHandle, folder_id: &str) -> Result<AppEntry, String> {
    use tauri::Manager;
    let dock_apps = app
        .state::<crate::store::PrefsState>()
        .0
        .lock()
        .unwrap()
        .pinned
        .clone();
    crate::apps::enumerate_dock_apps(&dock_apps)
        .into_iter()
        .find(|e| e.id == folder_id)
        .ok_or_else(|| format!("Dock 上没有这个文件夹: {folder_id}"))
}
