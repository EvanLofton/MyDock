//! 系统托盘。
//!
//! 用 Tauri 内置的 `tray-icon`（本机 cargo 缓存里有 `tray-icon 0.23.1`），
//! 不引额外插件。
//!
//! 菜单刻意保持精简：只放**真正需要脱离 Dock 界面才能操作**的开关。
//! Dock 本体负责日常操作，托盘只负责「Dock 自己没法表达」的那几件事
//! （自动隐藏、开机自启、退出）。

use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

use crate::store::{self, PrefsState};

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let prefs = app.state::<PrefsState>().0.lock().unwrap().clone();

    let add = MenuItem::with_id(app, "add-app", "添加应用…", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;

    // 「添加系统位置」子菜单：此电脑 / 回收站（表在 `apps::SYSTEM_LOCATIONS`，
    // 加一个新位置只要往表里加一行，这里和右键菜单都会跟着变）。
    //
    // 菜单文字问 Shell 要（中文系统上就是「此电脑」），查不到才用兜底文案 ——
    // 这样菜单上的字和加进 Dock 之后图标下面的字**一定是同一个**。
    let locs: Vec<MenuItem<tauri::Wry>> = crate::apps::SYSTEM_LOCATIONS
        .iter()
        .map(|(target, fallback)| {
            let text = crate::apps::shell_display_name(target)
                .unwrap_or_else(|| (*fallback).to_string());
            // 菜单 id 里带上 target 的 id，点击时反查回真正的 target
            let id = format!("loc:{}", crate::apps::id_for_target(target));
            MenuItem::with_id(app, id, text, true, None::<&str>)
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    let loc_refs: Vec<&dyn IsMenuItem<tauri::Wry>> =
        locs.iter().map(|i| i as &dyn IsMenuItem<tauri::Wry>).collect();
    let loc_menu = Submenu::with_id_and_items(app, "add-loc", "添加系统位置", true, &loc_refs)?;

    let autohide = CheckMenuItem::with_id(
        app,
        "autohide",
        "自动隐藏",
        true,
        prefs.auto_hide,
        None::<&str>,
    )?;
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "开机自启",
        true,
        crate::autostart::is_enabled(),
        None::<&str>,
    )?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Dock", true, None::<&str>)?;
    // Dock 自带的「临时文件夹」：一个真实目录（`%LOCALAPPDATA%\dev.local.dock\临时文件`），
    // 点开就是资源管理器 —— 用户往里丢临时文件。放在最左侧。
    let temp = MenuItem::with_id(app, "add-temp", "添加临时文件夹", true, None::<&str>)?;
    // 「打开日志目录」/「打开配置目录」：这两个目录**不在安装目录下**（见 README），
    // 埋在 %LOCALAPPDATA% / %APPDATA% 里用户找不到 —— 出问题时第一个要找的就是日志。
    let open_logs = MenuItem::with_id(app, "open-logs", "打开日志目录", true, None::<&str>)?;
    let open_cfg = MenuItem::with_id(app, "open-config", "打开配置目录", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &add, &loc_menu, &temp, &settings, &autohide, &autostart, &open_logs, &open_cfg, &sep,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id("dock-tray")
        .menu(&menu)
        .tooltip("Dock")
        // 左键不弹菜单（右击才弹），避免误触
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu);

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    } else {
        log_warn!("[托盘] 没有可用的默认窗口图标，托盘图标可能为空");
    }

    builder.build(app)?;
    log_info!("[托盘] 已创建");
    Ok(())
}

fn on_menu(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref().to_string();
    // 「添加系统位置」的菜单 id 是 `loc:<target 的 id>`，target 原样放在后面
    if let Some(target) = id.strip_prefix("loc:") {
        // 菜单 id 里存的是小写 id，这里换回真正的 target（大小写敏感）
        let target = crate::apps::SYSTEM_LOCATIONS
            .iter()
            .find(|(t, _)| crate::apps::id_for_target(t) == target)
            .map(|(t, _)| *t);
        match target {
            Some(t) => match crate::store::add_system_location(app, t) {
                Ok(_) => {
                    log_info!("[托盘] 已添加系统位置 {t}");
                    crate::drop::refresh_apps(app);
                }
                Err(e) => {
                    log_error!("[托盘] 添加系统位置失败: {e}");
                    crate::drop::toast(app, &e);
                }
            },
            None => log_warn!("[托盘] 未知的系统位置 id: {id}"),
        }
        return;
    }
    match id.as_str() {
        "quit" => {
            log_info!("[托盘] 退出");
            // 任务栏的还原挂在 `main.rs` 的 `RunEvent::Exit` 上（覆盖所有退出路径），
            // 这里不用重复做。
            app.exit(0);
        }
        "settings" => {
            if let Err(e) = crate::settings_window::open(app) {
                log_error!("[托盘] 打开设置失败: {e}");
            }
        }
        // 日志与配置**不在安装目录下**（Program Files 不可写、且升级时不该被清掉），
        // 所以给两个直达入口 —— 出问题时要日志、要备份时要配置。
        "open-logs" => {
            let dir = crate::logging::log_dir();
            let _ = std::fs::create_dir_all(&dir);
            if let Err(e) = crate::apps::launch(&dir.to_string_lossy(), false) {
                log_error!("[托盘] 打开日志目录失败（{}）: {e}", dir.display());
            }
        }
        "open-config" => {
            let dir = crate::store::config_dir(app);
            let _ = std::fs::create_dir_all(&dir);
            if let Err(e) = crate::apps::launch(&dir.to_string_lossy(), false) {
                log_error!("[托盘] 打开配置目录失败（{}）: {e}", dir.display());
            }
        }
        "add-temp" => {
            match store::add_temp_folder(app) {
                Ok(_) => {
                    log_info!("[托盘] 已把临时文件夹放到最左侧");
                    crate::drop::refresh_apps(app);
                }
                Err(e) => {
                    log_error!("[托盘] 添加临时文件夹失败: {e}");
                    crate::drop::toast(app, &e);
                }
            }
        }
        "add-app" => {
            // 文件对话框是模态的，不能占住主线程（Tauri 事件循环）
            let a = app.clone();
            crate::reveal::pause();
            std::thread::spawn(move || {
                match crate::picker::pick_and_pin(&a) {
                    Ok(()) => log_info!("[托盘] 已添加应用"),
                    Err(e) => log_error!("[托盘] 添加应用失败: {e}"),
                }
                crate::reveal::unpause();
            });
        }
        "autohide" => {
            let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
            p.auto_hide = !p.auto_hide;
            // 立刻生效：自动隐藏线程每帧都会读这个标志
            crate::reveal::set_enabled(p.auto_hide);
            if let Err(e) = store::save(app, &p) {
                log_error!("[托盘] 保存配置失败: {e}");
            }
            *app.state::<PrefsState>().0.lock().unwrap() = p.clone();
            log_info!("[托盘] 自动隐藏 -> {}", p.auto_hide);
        }
        "autostart" => {
            let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
            let want = !crate::autostart::is_enabled();
            let result = if want {
                match crate::autostart::current_exe() {
                    Some(exe) => crate::autostart::enable(&exe),
                    None => Err("无法取得自身路径".into()),
                }
            } else {
                crate::autostart::disable()
            };
            match result {
                Ok(()) => {
                    p.launch_at_login = want;
                    if let Err(e) = store::save(app, &p) {
                        log_error!("[托盘] 保存配置失败: {e}");
                    }
                    *app.state::<PrefsState>().0.lock().unwrap() = p;
                    log_info!("[托盘] 开机自启 -> {want}");
                }
                Err(e) => {
                    log_error!("[托盘] 设置开机自启失败: {e}");
                    // 失败时把勾选状态复原，避免界面与实际不一致
                    if let Some(tray) = app.tray_by_id("dock-tray") {
                        let _ = tray.set_visible(true);
                    }
                }
            }
        }
        _ => {}
    }
}
