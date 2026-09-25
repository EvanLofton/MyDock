//! 把桌面 / 资源管理器里的程序**拖到 Dock 上**添加。
//!
//! # 拖放是怎么被接到的
//!
//! 没有自己实现 `IDropTarget` —— Tauri/wry 已经默认装好了：wry 会
//! `EnumChildWindows` 把 `IDropTarget` **注入到每一个子窗口**（包括盖住我们客户区的
//! WebView2 窗口），并把 WebView2 自带的 `AllowExternalDrop` 关掉，
//! 然后通过 `WindowEvent::DragDrop` 把路径交给我们。
//! 这一点对本项目很关键：Dock 是 `WS_EX_NOACTIVATE` 的工具窗口，客户区又完全被
//! WebView2 覆盖，如果拖放要落在**顶层窗口**上，我们根本收不到。
//!
//! 所以这里只负责「拿到路径 → 认身份 → 去重 → 加到 Dock 右侧 → 通知前端刷新」。
//!
//! # 接受什么
//!
//! | 拖进来的东西 | 处理 |
//! |---|---|
//! | `.exe` | 直接用它的全路径当 target |
//! | `.lnk` | 解析出真实目标；指向商店应用的快捷方式走 IDList → `shell:AppsFolder\...` |
//! | `.url` | **Internet 快捷方式**（Steam 的「添加桌面快捷方式」就是这个）：保留快捷方式路径当 target，启动交给 `ShellExecute`，图标按里面的 `IconFile=` 取（见 `icons.rs`） |
//! | 其他文件 / 文件夹 | 拒绝，并明确告诉用户为什么 |
//!
//! # 身份与去重
//!
//! id 一律用 `apps::id_for_target()` 算 —— **和运行时枚举完全相同的那条规则**。
//! 这是整个功能的关键：只有当 id 一致，拖进来的程序和「已经在跑的那个实例」
//! 才会合并成一条，白点也才会立刻亮起来。用别的办法算 id 就会出现两个图标。

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

/// 一次路径的处理结果，用来给用户一句明确的话。
pub enum Outcome {
    Added { name: String, running: bool },
    Already { name: String },
    Rejected { name: String, why: String },
}

/// 处理一次拖放（可能一次拖进来多个文件）。
pub fn handle_drop(app: &AppHandle, paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }
    log_info!("[拖放] 收到 {} 个路径", paths.len());

    // 提示语一律用**文件名**，不用全路径 —— Dock 上的 toast 只有一行、
    // 全路径会把真正的原因挤掉（实测反馈：拖个 pdf 提示长得看不完）。
    let mut added: Vec<(String, bool)> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    for p in paths {
        match handle_one(app, p) {
            Outcome::Added { name, running } => {
                log_info!("[拖放] 已添加 {name}（运行中={running}）");
                added.push((name, running));
            }
            Outcome::Already { name } => {
                log_info!("[拖放] 已在 Dock 上: {name}");
                notes.push(format!("{name} 已在 Dock 上"));
            }
            Outcome::Rejected { name, why } => {
                // 日志里留全路径便于排查，提示里只用文件名
                log_info!("[拖放] 拒绝 {} ({name}): {why}", p.display());
                notes.push(format!("{name}：{why}"));
            }
        }
    }

    // 给用户一句话。**长度必须有上限**：一次拖进来 5 个不合适的文件时
    // 不能把 5 条原因全串起来。
    let msg = if added.is_empty() {
        match notes.len() {
            0 => String::new(),
            1 => notes[0].clone(),
            n => format!("{}（另有 {} 个未添加）", notes[0], n - 1),
        }
    } else if notes.is_empty() {
        if added.len() == 1 {
            let (name, running) = &added[0];
            if *running {
                format!("已添加 {name}（运行中）")
            } else {
                format!("已添加 {name}")
            }
        } else {
            format!("已添加 {} 个程序", added.len())
        }
    } else {
        format!("已添加 {} 个，{} 个未添加", added.len(), notes.len())
    };
    if !msg.is_empty() {
        toast(app, &msg);
    }

    if !added.is_empty() {
        // 立刻刷新 Dock 的图标列表：不要等下一次轮询（最多 1.5 秒），
        // 否则用户会觉得「拖了没反应」。
        refresh_apps(app);
    }
}

/// 处理一个路径。抽出来是为了让自检可以直接调用（见 `selftest::drive_drop_test`）。
pub fn handle_one(app: &AppHandle, path: &Path) -> Outcome {
    // 提示语里用文件名即可，全路径太长（见 `handle_drop` 里的说明）
    let file_name = || {
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    };

    let target = match resolve_target(path) {
        Ok(t) => t,
        Err(why) => {
            return Outcome::Rejected {
                name: file_name(),
                why,
            }
        }
    };

    let id = crate::apps::id_for_target(&target);
    let name = display_name(&target, &id);

    // 唯一性：已经在固定列表里就什么都不做（不是错误，只是没必要加两次）
    if app
        .state::<crate::store::PrefsState>()
        .0
        .lock()
        .unwrap()
        .pinned
        .iter()
        .any(|p| p.id == id)
    {
        return Outcome::Already { name };
    }

    if let Err(e) = crate::store::pin(
        app,
        crate::model::PinnedApp {
            id: id.clone(),
            display_name: name.clone(),
            target,
            separator: false,
            is_folder: false,
            children: Vec::new(),
        },
    ) {
        return Outcome::Rejected {
            name: file_name(),
            why: format!("写入配置失败: {e}"),
        };
    }

    // 这个程序现在在不在跑？决定是「立刻亮白点」还是「等它启动」。
    // 枚举一次约 2.6ms，只在拖放时发生，可以接受。
    let running = crate::apps::enumerate_apps().iter().any(|a| a.id == id);

    Outcome::Added { name, running }
}

/// 路径 → 可启动的 target（exe 全路径、`shell:AppsFolder\<AUMID>`，或 `.url` 快捷方式路径）。
pub fn resolve_target(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Err("路径不存在".into());
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "exe" => Ok(path.display().to_string()),
        "lnk" => resolve_lnk(path),
        // **`.url`（Internet 快捷方式）**：Steam 的「添加桌面快捷方式」生成的就是它。
        //
        // target 保留**快捷方式文件路径**，而不是把里面的 `URL=` 抠出来当 target：
        //  1. 图标：`IconFile=` 指着游戏自己的图标，走文件路径才取得到
        //     （裸的 `steam://…` 在 Shell 里**根本没有图标**，实测取不到）；
        //  2. 启动：`ShellExecuteEx` 打开 `.url` 本来就会执行里面的 URL，
        //     协议解析交给系统，我们不用自己维护一张协议表；
        //  3. 「打开文件所在位置」仍然成立（能定位到这个快捷方式）。
        "url" => match crate::apps::url_shortcut_value(&path.display().to_string(), "URL") {
            Some(url) => {
                log_info!("[拖放] .url 快捷方式 → {url}");
                Ok(path.display().to_string())
            }
            // 空壳 `.url` 要**说清原因**：加进去只会点了没反应
            None => Err("这个 .url 里没有 URL".into()),
        },
        // 目录、pdf、图片……拖进来没有意义。
        // 原因要**短**：这是给 Dock 上那一行 toast 用的（见 handle_drop）。
        "" => Err("文件夹不能添加".into()),
        _ => Err("只接受 .exe / .lnk / .url".into()),
    }
}

/// 解析 `.lnk` 的真实目标。
///
/// 两条路：
///  1. `GetPath` —— 普通快捷方式（指向某个 exe）
///  2. `GetIDList` + `SHGetNameFromIDList` —— 指向**商店应用**的快捷方式，
///     目标不是文件路径而是 shell 命名空间项，`GetPath` 会返回空串。
///     这条路拿到的是 `shell:AppsFolder\<AUMID>`，正好是我们要的 target。
fn resolve_lnk(path: &Path) -> Result<String, String> {
    use windows::core::{Interface, PCWSTR};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistFile, STGM_READ,
    };
    use windows::Win32::UI::Shell::{
        IShellLinkW, SHGetNameFromIDList, SIGDN_DESKTOPABSOLUTEPARSING, ShellLink,
    };

    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| {
                format!("无法创建 ShellLink 对象（COM 未初始化？）: {e}")
            })?;

        let pf: IPersistFile = link.cast().map_err(|e| format!("接口转换失败: {e}"))?;
        let w = crate::apps::wide(&path.to_string_lossy());
        pf.Load(PCWSTR(w.as_ptr()), STGM_READ)
            .map_err(|e| format!("读取快捷方式失败: {e}"))?;

        // 1) 普通快捷方式：拿目标文件路径
        let mut buf = [0u16; 1024];
        // SLGP_RAWPATH = 4：要原始路径，不要系统偏好的短路径/UNC 形式
        if link.GetPath(&mut buf, std::ptr::null_mut(), 4).is_ok() {
            let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
            if n > 0 {
                let s = String::from_utf16_lossy(&buf[..n]);
                if !s.is_empty() {
                    return Ok(s);
                }
            }
        }

        // 2) 商店应用快捷方式：目标藏在 IDList 里
        if let Ok(pidl) = link.GetIDList() {
            if !pidl.is_null() {
                let r = SHGetNameFromIDList(pidl, SIGDN_DESKTOPABSOLUTEPARSING);
                CoTaskMemFree(Some(pidl as *const core::ffi::c_void));
                if let Ok(pw) = r {
                    if !pw.is_null() {
                        let s = pw.to_string().unwrap_or_default();
                        CoTaskMemFree(Some(pw.0 as *const core::ffi::c_void));
                        if !s.is_empty() {
                            return Ok(s);
                        }
                    }
                }
            }
        }

        Err("快捷方式解析不出目标，请用托盘「添加应用…」".into())
    }
}

/// 展示名。
///
/// - 打包应用（`shell:AppsFolder\...`）：`file_description()` 读不了这种「路径」，
///   会退化成把整串 AUMID 当名字，所以改为问 Shell 要显示名。
/// - Win32：用 exe 的版本信息 `FileDescription`（和 `apps.rs` 里同一条路）。
fn display_name(target: &str, id: &str) -> String {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY};

    if target.starts_with("shell:") {
        let w = crate::apps::wide(target);
        unsafe {
            if let Ok(item) =
                SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(w.as_ptr()), None)
            {
                if let Ok(pw) = item.GetDisplayName(SIGDN_NORMALDISPLAY) {
                    if !pw.is_null() {
                        let s = pw.to_string().unwrap_or_default();
                        // GetDisplayName 用 CoTaskMemAlloc 分配，必须自己释放
                        windows::Win32::System::Com::CoTaskMemFree(Some(
                            pw.0 as *const core::ffi::c_void,
                        ));
                        if !s.is_empty() {
                            return s;
                        }
                    }
                }
            }
        }
        // 兜底：AUMID 的 `!` 之后那一段，总比整串 AUMID 强
        return id
            .rsplit('!')
            .next()
            .unwrap_or(id)
            .trim_start_matches("uwp:")
            .to_string();
    }

    crate::apps::display_name_for(target)
}

/// 让 Dock 页面立刻重读应用列表（不等下一次轮询）。
///
/// 拖放添加应用、右键菜单里的动作（加分割线 / 移除 / 启动）都走这里 ——
/// 「谁改了列表谁负责通知界面重画」，别让界面靠轮询去发现。
pub fn refresh_apps(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("dock") {
        let _ = w.eval("window.__dockRefreshApps && window.__dockRefreshApps();");
    }
}

/// 在 Dock 上弹一句提示（复用前端已有的 toast）。
///
/// 公开是因为右键菜单也要用它：**菜单里没有显示错误的位置** ——
/// 动作一失败菜单就关了，错误信息只能在 Dock 上出现，绝不能静默丢弃。
pub fn toast(app: &AppHandle, msg: &str) {
    if let Some(w) = app.get_webview_window("dock") {
        // 用 serde_json 转义，避免消息里的引号/反斜杠把 JS 字符串搞坏
        let lit = serde_json::to_string(msg).unwrap_or_else(|_| "\"\"".into());
        let _ = w.eval(&format!("window.__dockToast && window.__dockToast({lit});"));
    }
}
