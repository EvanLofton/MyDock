//! P1 窗口层：把 P0 的实测结论落地到真实 Tauri 窗口。
//!
//! 落地清单（每条都对应一个 P0/P1 实测结论）：
//!  1. 补 `WS_EX_NOACTIVATE`（tao 不设）
//!  2. 补 `WS_EX_TOOLWINDOW` / 去 `WS_EX_APPWINDOW`（tao 的 skip_taskbar 未生效）
//!  3. 去掉 `WS_EX_LAYERED`（会削弱毛玻璃）
//!  4. `SetWindowSubclass` 补 `WM_MOUSEACTIVATE → MA_NOACTIVATE`（同进程防夺焦点）
//!  5. 路线 A 毛玻璃 + 自绘圆角 + 去系统边框 + 深色材质
//!  6. 按 `rcWork` × DPI 计算物理矩形并钳制在屏内（避免被系统重定位）
//!
//! **线程约定**（踩过坑，务必遵守）：
//!  - `SetWindowSubclass` 必须由**拥有窗口的线程**调用，否则静默失败 → 窗口层设置放主线程；
//!  - 自检里要 `sleep` 等输入生效，**不能放主线程**，否则事件循环被阻塞、窗口收不到点击
//!    → 自检放后台线程。

// GUI 子系统：**双击启动不弹终端窗口**。
//
// 这里以前是 `cfg_attr(not(debug_assertions), …)` —— debug 构建保留控制台方便看 `println!`。
// 代价是用户每次启动都会多一个终端窗口，而且没有控制台时 `println!` 会因为写失败而 panic。
// 现在输出统一走 `logging`（**先落文件**，控制台镜像写失败一律忽略），
// 所以可以放心一直用 GUI 子系统：看不到窗口，但日志一条不少。
#![windows_subsystem = "windows"]

// ❗`logging` 必须**第一个**声明并带 `#[macro_use]`：日志宏要按文本顺序对后面的模块可见。
// 这样各个文件里只管写 `log_info!(...)`，不必逐个 `use`。
#[macro_use]
mod logging;

/// 应用标识符（**必须与 `tauri.conf.json` 的 `identifier` 完全一致**）。
///
/// 为什么在 Rust 里也留一份：日志目录是**在 Tauri 起来之前**就要打开的
/// （早开日志才能记下启动阶段的问题），那时候还没有 `AppHandle` 可以问
/// `app.path()`。配置目录不在这里 —— 它走 `app.path().app_config_dir()`，
/// 自动跟随 identifier。
///
/// 单测 `logging::tests::identifier_matches_tauri_conf` 会比对这两个值，防止漂移。
pub const IDENTIFIER: &str = "io.github.evanlofton.mydock";

/// 改名前的标识符（`dev.local.dock` → 现在这个）。只用于**一次性数据迁移**。
pub const OLD_IDENTIFIER: &str = "dev.local.dock";
mod apps;
mod autostart;
mod commands;
mod drop;
mod glass;
mod icons;
mod menu;
mod panel_window;
mod model;
mod picker;
mod reveal;
mod prompt_window;
mod settings_window;
mod store;
mod tray;
mod win_layer;
// 自检脚手架：只在 --features selftest 时编译，产品构建里不存在
#[cfg(feature = "selftest")]
mod selftest;

use std::ffi::c_void;
use std::path::PathBuf;
use std::time::Duration;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use windows::Win32::Foundation::HWND;

/// Dock 宽度（逻辑像素）—— 只是**初始猜测**。
///
/// 真正的宽度由前端按图标行宽上报（见 `commands::set_dock_size`），
/// 因为窗口必须与面板严丝合缝，否则会露出灰底。
///
/// 高度同理，但**初始值直接取配置里的 `panel_height`**（见 `setup_window_layer`）——
/// 写死一个常数的话，用户把面板调高之后每次启动第一帧都会矮一截再长回去。
const DOCK_W_LOGICAL: i32 = 520;

unsafe extern "system" fn opponent_proc(
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

/// 子进程模式：只创建一个普通窗口并跑消息循环。
///
/// 用途：给「激活 / 最小化 / 还原」自检提供一个**属于另一个进程**的操作对象。
/// 让 Dock 自己能扮演这个角色，测试就不依赖任何外部程序。
fn hold_window() {
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::UI::WindowsAndMessaging::*;

    let class = win_layer::wide("DockOpponentWindow");
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(opponent_proc),
            hInstance: HINSTANCE(win_layer::hmodule().0),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: windows::core::PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        let h = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            windows::core::PCWSTR(class.as_ptr()),
            windows::core::w!("DOCK-OPPONENT"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            300,
            300,
            520,
            320,
            None,
            None,
            Some(HINSTANCE(win_layer::hmodule().0)),
            None,
        );
        if h.is_err() {
            log_error!("holdwin: 创建窗口失败");
            std::process::exit(1);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 单实例保护。
///
/// Dock 是常驻程序，跑出两个实例会**叠出两个 Dock**（窗口互相覆盖、托盘出现两个图标、
/// 点「退出」只关掉一个），很难排查。`tauri-plugin-single-instance` 不在本机 cargo 缓存里，
/// 所以用命名互斥体自己实现，十几行。
///
/// 用 `Local\` 前缀：作用域限当前登录会话，多用户各自一个实例是合理的。
/// 返回 `Some(句柄)` 表示抢到了；**句柄要活到进程结束**（进程退出时系统自动释放锁）。
fn acquire_single_instance() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::core::w;
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;

    unsafe {
        match CreateMutexW(None, true, w!("Local\\Dock_SingleInstance_Mutex")) {
            Ok(h) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    None
                } else {
                    Some(h)
                }
            }
            // 创建失败就不拦（宁可多开一个，也不要起不来）
            Err(_) => Some(windows::Win32::Foundation::HANDLE::default()),
        }
    }
}

fn main() {
    // 子进程模式要在 Tauri 初始化之前分流
    if std::env::args().nth(1).as_deref() == Some("holdwin") {
        hold_window();
        return;
    }

    // ---- 日志与崩溃取证：**越早越好**，之后所有输出都进日志文件 ----
    //
    // 顺序有讲究：先开日志（这样后面任何一步出问题都留得下痕迹），
    // 再装 panic 钩子与未处理异常过滤器（访问违例这类"真闪退"只有后者拦得住），
    // 最后才让 `DOCK_CRASH_TEST` 有机会人为崩一次（自检这条链路本身）。
    let log_file = logging::init();
    logging::install_panic_hook();
    logging::install_crash_handler();
    log_info!("日志已就绪：{}", log_file.display());
    logging::maybe_crash_for_test();

    // 单实例：抢不到就静默退出（**不能**报错弹窗，否则登录时自启会很烦）
    let Some(_instance_guard) = acquire_single_instance() else {
        log_warn!("[启动] 已有一个 Dock 实例在运行，本次退出");
        std::process::exit(0);
    };

    // Shell 的图标接口（SHCreateItemFromParsingName）要求调用线程已初始化 COM
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::list_apps,
            commands::js_log,
            commands::pick_and_pin_app,
            commands::reorder_dock_apps,
            commands::remove_dock_app,
            commands::add_separator,
            commands::add_system_location,
            commands::get_icon_bgra,
            commands::get_recycle_bin_items,
            commands::prompt_state,
            commands::prompt_ready,
            commands::prompt_submit,
            commands::prompt_cancel,
            commands::activate_app,
            commands::minimize_app,
            commands::close_app,
            commands::launch_app,
            commands::show_panel_menu,
            commands::show_panel_folder,
            commands::hide_panel,
            commands::show_icon_label,
            commands::hide_icon_label,
            commands::panel_state,
            commands::panel_ready,
            commands::panel_invoke,
            commands::panel_launch,
            commands::create_folder,
            commands::add_app_to_folder,
            commands::dissolve_folder,
            commands::move_out_of_folder,
            commands::dock_hwnd,
            commands::get_preferences,
            commands::set_preferences,
            commands::set_dock_size,
            commands::get_dock_info,
            commands::open_settings,
            commands::selftest_report,
        ])
        // 从桌面/资源管理器拖程序到 Dock 上添加。
        //
        // 拖放本身由 Tauri/wry 接（它会把 IDropTarget 注入到每个子窗口，
        // 包括盖住客户区的 WebView2 —— 见 drop.rs 的模块说明），
        // 这里只处理落下的路径。设置窗口也会收到这个事件，按 label 过滤掉。
        .on_window_event(|window, event| {
            if window.label() != "dock" {
                return;
            }
            if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event {
                crate::drop::handle_drop(window.app_handle(), paths);
            }
        })
        .setup(|app| {
            // 载入配置并放进全局状态
            let handle = app.handle().clone();
            let prefs = store::load(&handle);
            reveal::set_enabled(prefs.auto_hide);
            app.manage(store::PrefsState(std::sync::Mutex::new(prefs.clone())));

            // 首次运行（还没有配置文件）：预置一批本机确实装了的常用应用。
            // 这只是**出厂初值** —— 之后 Dock 上有什么、什么顺序，完全由用户决定
            // （拖放 / 添加应用… / 右键移除 / 将来的拖拽排序）。
            if store::is_first_run(&handle) {
                let n = store::seed_default_pins(&handle);
                log_info!("[配置] 首次运行，已预置 {n} 个常用应用");
            }

            // 托盘（失败不应阻断启动）
            if let Err(e) = tray::build(&handle) {
                log_error!("[托盘] 创建失败: {e}");
            }

            let mut builder =
                WebviewWindowBuilder::new(app, "dock", WebviewUrl::App("index.html".into()))
                    .title("Dock")
                    // 高度取配置值：前端挂载后会按实际面板尺寸再上报一次，
                    // 但初值就对的话第一帧不会「矮一截再长回去」。
                    .inner_size(DOCK_W_LOGICAL as f64, prefs.panel_height as f64)
                    .transparent(true)
                    .decorations(false)
                    // ❗**不置顶**（原来是 always_on_top）：Dock 属于**桌面那一层**，
                    // 用户的窗口要盖在它上面（"Dock 只在桌面上，其他窗口都在它之上"）。
                    // 置顶的话它就一直浮在所有窗口上面抢地方，像任务栏那样。
                    // 需要浮在最上面的只有右键菜单 / 文件夹面板（见 panel_window.rs）。
                    .always_on_top(false)
                    .skip_taskbar(true)
                    .resizable(false)
                    .maximizable(false)
                    .minimizable(false)
                    .shadow(false)
                    .focused(false)
                    .visible(true);

            // 仅开发环境用：把 WebView2 数据目录重定向到可写位置。
            // 生产环境不设该变量，走 Tauri 默认的 %LOCALAPPDATA%\{identifier}。
            if let Ok(dir) = std::env::var("DOCK_WEBVIEW_DATA") {
                builder = builder.data_directory(PathBuf::from(dir));
            }

            let _win = builder.build()?;



            // 开发用：启动就把设置窗口打开并留着（默认不开，正常走托盘「设置…」）。
            // 用途：看设置页的实际观感 / 截图核对排版 —— 光靠读代码判断不了好不好看。
            if std::env::var("DOCK_OPENSETTINGS").as_deref() == Ok("1") {
                let h = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1500)); // 等 Dock 窗口就绪
                    if let Err(e) = settings_window::open(&h) {
                        log_error!("[设置] 启动即打开失败: {e}");
                    }
                });
            }
            let handle = app.handle().clone();
            std::thread::spawn(move || supervise(handle));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Tauri 启动失败");
}

/// 在后台线程监督：等句柄就绪 → 把窗口层设置投递到主线程执行 → 回后台跑自检。
fn supervise(app: tauri::AppHandle) {
    let hwnd = match wait_for_hwnd(&app) {
        Some(h) => h,
        None => {
            log_error!("[窗口层] 轮询 6 秒仍未取得窗口句柄，放弃初始化");
            return;
        }
    };

    // 窗口层设置必须在主线程完成（SetWindowSubclass 的线程约束）。
    // HWND 含裸指针不是 Send，跨线程只传地址。
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let app_main = app.clone();
    let addr = hwnd.0 as usize;
    if app
        .run_on_main_thread(move || {
            setup_window_layer(&app_main, HWND(addr as *mut c_void));
            let _ = tx.send(());
        })
        .is_err()
    {
        log_error!("[窗口层] 无法投递到主线程");
        return;
    }
    let _ = rx.recv_timeout(Duration::from_secs(15));

    // 两个浮层窗口（文件夹 + 菜单）**等窗口层就绪之后再建**。
    //
    // 为什么不跟 Dock 窗口一起在 `setup` 里建：每次 `build()` 都是同步创建 WebView2
    // （几百毫秒），连着建三个会把主线程占住几秒 —— 而上面那段窗口层初始化正是
    // **投递到主线程**执行的，于是它被推迟到页面首次上报之后才跑（本轮实测踩到，
    // 症状是 Dock 永久停在初值宽度）。放到这里之后，Dock 的启动顺序与只有一个窗口时一致。
    //
    // 仍然是启动时建（不是第一次右键再建）：右键是高频动作，等 WebView2 初始化
    // 那几百毫秒会明显卡一下。等这一两秒的时间用户根本还没动鼠标。
    {
        let app_panels = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            let a = app_panels.clone();
            let _ = app_panels.run_on_main_thread(move || {
                if let Err(e) = panel_window::create_all(&a) {
                    // 不阻断：浮层打不开只是少两个入口，Dock 本身还能用
                    log_error!("[浮层] 窗口创建失败: {e}");
                }
            });
        });
    }

    // 自检脚手架（焦点矩阵 / 应用操作 / IPC 链 / 应用枚举 dump）只在
    // `--features selftest` 的构建里存在，产品构建根本不编译它。
    #[cfg(feature = "selftest")]
    selftest::run_if_enabled(&app, hwnd);

    // WebView2 会**动态创建子窗口**（宿主窗口、渲染窗口等），
    // 建窗那一刻还看不到它们。这里周期性把 WM_MOUSEACTIVATE 钩子补挂到新窗口上，
    // 否则点击落在子窗口时钩子走不到（R15 的残留风险）。
    // 必须投递到主线程执行 —— SetWindowSubclass 只能由拥有窗口的线程调用。
    // 只在窗口树真的变化时打一行日志；设 DOCK_DEBUG=1 才展开逐窗口明细。
    //
    // Dock 与右键菜单窗口都要挂：菜单窗口同样必须**不夺焦点**
    // （`WS_EX_NOACTIVATE` 只管跨进程，同进程的窗口之间仍会互相激活）。
    let debug = std::env::var("DOCK_DEBUG").as_deref() == Ok("1");
    let app_loop = app.clone();
    std::thread::spawn(move || {
        let mut last = (0u32, 0u32);
        loop {
            std::thread::sleep(Duration::from_secs(3));
            let (tx, rx) = std::sync::mpsc::channel::<((u32, u32), Vec<String>)>();
            let a = app_loop.clone();
            // 注意：`last` 不能直接进内层 move 闭包 —— 那会按值复制，
            // 外层永远看不到更新，导致每轮都重复打印。用 channel 把结果传回来。
            let _ = app_loop.run_on_main_thread(move || {
                let (mut ok, mut total) = (0u32, 0u32);
                let mut detail = Vec::new();
                for label in ["dock", "panel", "menu"] {
                    let Some(w) = a.get_webview_window(label) else {
                        continue;
                    };
                    let Ok(raw) = w.hwnd() else { continue };
                    let h = HWND(raw.0 as *mut c_void);
                    if h.0.is_null() {
                        continue;
                    }
                    let (o, t) = win_layer::hook_tree_once(h);
                    ok += o;
                    total += t;
                    if debug {
                        detail.push(format!("-- {label} 窗口树 --"));
                        detail.extend(win_layer::hook_tree_detail());
                    }
                }
                let _ = tx.send(((ok, total), detail));
            });
            if let Ok((r, detail)) = rx.recv_timeout(Duration::from_secs(2)) {
                if r != last && r.1 > 0 {
                    if debug {
                        log_info!(
                            "[钩子] 窗口树变化：{} / {} 个窗口已挂 WM_MOUSEACTIVATE",
                            r.0, r.1
                        );
                        for line in &detail {
                            log_info!("{line}");
                        }
                    }
                    last = r;
                }
            }
        }
    });
}

fn wait_for_hwnd(app: &tauri::AppHandle) -> Option<HWND> {
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(100));
        let (tx, rx) = std::sync::mpsc::channel::<Option<usize>>();
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || {
            let raw = app2
                .get_webview_window("dock")
                .and_then(|w| w.hwnd().ok())
                .map(|h| h.0 as usize);
            let _ = tx.send(raw);
        });
        if let Ok(Some(addr)) = rx.recv_timeout(Duration::from_millis(2000)) {
            if addr != 0 {
                return Some(HWND(addr as *mut c_void));
            }
        }
    }
    None
}

/// Dock 的窗口层初始化：扩展样式 → 焦点防护 → 毛玻璃 → 定位 → 自动隐藏。
///
/// 只在启动时跑一次。这里的尺寸只是**初值** —— 前端挂载后会按实际图标行宽
/// 调用 `set_dock_size` 上报真实宽度（亚克力铺满整个窗口矩形，所以窗口宽度
/// 必须始终等于面板宽度）。
///
/// 设 `DOCK_DEBUG=1` 会额外打印整棵窗口树的挂钩明细，用于排查
/// 「WebView2 升级后子窗口类名变了，钩子挂不上」这类问题。
fn setup_window_layer(app: &tauri::AppHandle, hwnd: HWND) {
    let prefs = app.state::<store::PrefsState>().0.lock().unwrap().clone();

    // 1. 扩展样式：不进任务栏、不抢焦点、工具窗口
    let (style_before, style_after) = win_layer::apply_dock_ex_style(hwnd);

    // 1.5 沉到 Z 序最底：需求是"Dock 任何时刻都在最底层、不许有任何上升层级的行为"
    //     （用户 2026-09 明确要求）。窗口刚创建时被插在普通窗口带的**最前面**，
    //     于是"启动它之前就存在、之后没被激活过"的窗口会被它压住底部一条 —— 先沉底。
    //
    //     ❗必须在下面第 2 步（装层级钩子）**之前**：那个钩子会拦掉所有 Z 序变更，
    //     装完之后连我们自己都改不动它的层级了 —— 这正是要的效果：
    //     沉一次底，然后它永远不动。
    let sunk = win_layer::sink_to_bottom(hwnd);

    // 2. 焦点防护：给整棵窗口树（含 WebView2 子窗口）挂 WM_MOUSEACTIVATE -> MA_NOACTIVATE
    let icc = win_layer::init_common_controls();
    let (hooked, total) = win_layer::install_mouseactivate_hook_tree(hwnd);

    // 3. 亚克力 + 窗口材质（DWM 圆角 / 无边框 / 深色）
    let glass = glass::apply_acrylic(
        hwnd,
        (
            prefs.glass_rgb[0],
            prefs.glass_rgb[1],
            prefs.glass_rgb[2],
            prefs.glass_alpha,
        ),
    );
    glass::apply_window_material(hwnd);

    // 4. 定位（初值；真实宽度由前端上报）
    //
    // ❗**前端已经上报过就不要再动它。** 这一整段是异步投递到主线程的（见 `supervise`），
    // 启动时创建多个 WebView 会让它排到页面首次上报之后 —— 那时再用 520 的初值定位，
    // 就会把刚上报的真实宽度覆盖掉，而且之后不会有人再上报（宽度没变）
    // → Dock 永久停在初值宽度（面板比内容宽一大截、两边全是空的）。本轮真踩了。
    let reported = win_layer::size_reported();
    let target = if reported {
        win_layer::window_rect(hwnd)
    } else {
        win_layer::layout_dock(
            hwnd,
            DOCK_W_LOGICAL as f64,
            prefs.panel_height as f64,
            prefs.reserve_taskbar,
            prefs.bottom_offset,
        )
    };
    let actual = win_layer::window_rect(hwnd);
    let placed = actual.left == target.left && actual.top == target.top;

    // 5. 自动隐藏：线程常驻，是否真的隐藏由 reveal::ENABLED 决定
    //    （托盘上的「自动隐藏」开关会实时改它，不需要重启）
    if !reported {
        reveal::set_target(target);
    }
    reveal::spawn(hwnd.0 as usize, reveal::RevealParams::default());

    let dpi = win_layer::dpi_of(hwnd);
    log_info!("==================== Dock 已启动 ====================");
    log_info!("  HWND      0x{:X}", hwnd.0 as usize);
    log_info!(
        "  扩展样式  0x{style_before:08X} -> 0x{style_after:08X}  {}",
        win_layer::describe_ex_style(style_after)
    );
    log_info!("  焦点防护  InitCommonControlsEx={icc}，已挂 {hooked}/{total} 个窗口");
    log_info!(
        "  层级      沉底={}（桌面之上、普通窗口之下；之后任何 Z 序变更都会被钩子拦掉）",
        if sunk { "成功" } else { "**失败**" }
    );
    log_info!(
        "  毛玻璃    {glass}  rgba=({},{},{},{})",
        prefs.glass_rgb[0], prefs.glass_rgb[1], prefs.glass_rgb[2], prefs.glass_alpha
    );
    let (scr_w, scr_h) = win_layer::screen_size();
    log_info!(
        "  屏幕      {scr_w}x{scr_h} 物理像素（此刻已声明 DPI 感知，所以是真值）"
    );
    log_info!(
        "  定位      DPI {dpi} ({:.2}x)  物理 ({},{}) {}x{}  {}",
        dpi as f64 / 96.0,
        target.left,
        target.top,
        target.right - target.left,
        target.bottom - target.top,
        if placed { "精确命中" } else { "存在偏差" }
    );
    log_info!(
        "  自动隐藏  {}",
        if reveal::ENABLED.load(std::sync::atomic::Ordering::SeqCst) {
            "启用"
        } else {
            "关闭（托盘可切换）"
        }
    );
    log_info!("  （尺寸为初值，前端会按图标行宽重新上报）");
    // 这一行是为了**不被构建类型骗到**：`cargo build` 会把 `--features selftest`
    // 的产物**原地覆盖**，于是"跑了自检却没输出"看起来像自检坏了。
    // （本轮真的为此白等了一次 6 分钟的运行。）
    log_info!(
        "  构建      自检脚手架{}",
        if cfg!(feature = "selftest") {
            " **开启**（DOCK_SELFTEST / DOCK_MENUTEST / … 有效）"
        } else {
            "关闭（产品构建：那些环境变量全部无效）"
        }
    );
    log_info!("====================================================");

    if std::env::var("DOCK_DEBUG").as_deref() == Ok("1") {
        log_debug!("-- 窗口树挂钩明细（DOCK_DEBUG=1）--");
        for line in win_layer::hook_tree_detail() {
            log_info!("{line}");
        }
    }
}
