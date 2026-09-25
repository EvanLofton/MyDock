//! 右键菜单的**内容与动作**。
//!
//! 与 `panel_window.rs` 的分工：本模块只回答两个问题 ——
//!   1. 「右键这个条目，该列出哪些菜单项？」
//!   2. 「点了某一项，该干什么？」
//! 窗口怎么创建、怎么定位、什么时候显示/隐藏，全在 `panel_window.rs`。
//!
//! # 为什么不用原生 `TrackPopupMenu`
//!
//! 早期版本用的是 Win32 原生弹出菜单（唯一的好处是「它自己能超出 Dock 窗口边界」）。
//! 但外观完全无法定制（Windows 原生样式，和 Dock 的玻璃观感是两种东西），
//! 所以改成**独立的菜单窗口**：一个贴着菜单大小的 Tauri 窗口，页面里随便画。
//!
//! 更根本的原因见 `panel_window.rs` 的模块说明：原生菜单自带模态消息循环，
//! 会把调用线程整个卡住；独立窗口没有这个问题。

#![allow(dead_code)]

use std::ffi::c_void;

use tauri::AppHandle;
use windows::Win32::Foundation::HWND;

use crate::apps;

/// 菜单项的 id。前端只把它们原样回传，不在前端做任何判断 ——
/// 「哪一项可点、点了干什么」全部由 Rust 决定，前端只是渲染器。
pub mod id {
    pub const ACTIVATE: &str = "activate";
    pub const MINIMIZE: &str = "minimize";
    pub const CLOSE: &str = "close";
    pub const RUNAS: &str = "runas";
    pub const ADD_SEP: &str = "add-separator";
    pub const REMOVE: &str = "remove";
    pub const REVEAL: &str = "reveal";
    /// 把一个应用变成文件夹
    pub const MAKE_FOLDER: &str = "make-folder";
    /// 给文件夹改名字（弹输入框）
    pub const RENAME: &str = "rename";
    /// 清空回收站（只对回收站出现；危险操作）
    pub const EMPTY_BIN: &str = "empty-bin";
    /// 解散文件夹（里面的条目回到顶层）
    pub const DISSOLVE: &str = "dissolve-folder";
    /// 把文件夹里的一个条目**挪到顶层**（不是删掉）
    pub const MOVE_OUT: &str = "move-out";
}

/// 被右键的那个条目（Dock 上的一个图标，或一条分割线、一个文件夹）
#[derive(Debug, Clone, Default)]
pub struct Target {
    pub app_id: String,
    /// 启动目标：Win32 是 exe 全路径；打包应用是 `shell:AppsFolder\<AUMID>`
    pub target: String,
    /// 该应用某个窗口的句柄；0 = 没有窗口
    pub hwnd: u64,
    /// 当前是否有窗口在运行（决定第一组是「启动」还是窗口操作）
    pub running: bool,
    pub is_elevated: bool,
    /// 这一条是分割线（不是应用）
    pub separator: bool,
    /// 这一条是文件夹
    pub is_folder: bool,
    /// 这一条在**文件夹里面**（决定「移除」的文案与语义）
    pub in_folder: bool,
    /// 文件夹里有几项（只用于「移出 Dock（含 N 项）」这类**说清后果**的文案）
    pub children_count: usize,
    /// `target` 是一个**目录**（Dock 上的「临时文件夹」那种）。
    ///
    /// 由 `commands::menu_target` 量好放进来 —— 这样 `build()` 保持**纯函数**，
    /// 单测不用去碰文件系统。
    pub is_dir: bool,
    /// 回收站里现在有几项（只对回收站有意义，用来决定「清空回收站」是否可点）。
    /// 同样由 `menu_target` 填。
    pub bin_items: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    /// 危险操作（从 Dock 移除）—— 渲染成红字
    pub danger: bool,
    /// 视觉分隔线：`label` 为空，不可点击
    pub divider: bool,
}

impl MenuItem {
    pub fn new(id: &str, label: &str) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            enabled: true,
            danger: false,
            divider: false,
        }
    }

    fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    fn divider() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            enabled: false,
            danger: false,
            divider: true,
        }
    }
}

/// 按被右键的对象生成菜单项。
///
/// 顺序就是用户提的顺序：启动 → 以管理员身份运行 → 添加分割线 → 移出 Dock →
/// 打开文件所在位置。运行中时第一项展开成三个窗口操作。
pub fn build(t: &Target) -> Vec<MenuItem> {
    // 分割线只有一件事可做。给它「启动」「打开文件所在位置」是没有意义的
    // （它没有 target），列出来只会让人以为点了会有反应。
    if t.separator {
        return vec![MenuItem::new(id::REMOVE, "移除分割线").danger()];
    }

    // 文件夹：点图标就能展开，所以菜单里不重复给「展开」；
    // 重点是**出口**。这里有两个出口，语义必须分清楚：
    //   「解散文件夹」= 里面的东西回到顶层（不丢东西）
    //   「移出 Dock」= 连里面的东西一起消失 —— **文案里写明有几项**，
    //   否则用户以为只是把文件夹这个"壳"拿掉，一按就丢了一堆图标。
    // 「重命名…」排在最前面：改名字是用户右键文件夹时最常想做的事，
    // 而它和下面两个出口不是一类（它不动结构），所以中间隔一条分隔线。
    if t.is_folder {
        let remove_label = if t.children_count > 0 {
            format!("移出 Dock（含里面 {} 项）", t.children_count)
        } else {
            "移出 Dock".to_string()
        };
        return vec![
            MenuItem::new(id::RENAME, "重命名…"),
            MenuItem::divider(),
            MenuItem::new(id::DISSOLVE, "解散文件夹（放回 Dock）"),
            MenuItem::divider(),
            MenuItem::new(id::REMOVE, &remove_label).danger(),
        ];
    }

    let mut v = Vec::with_capacity(12);
    if t.running {
        v.push(MenuItem::new(id::ACTIVATE, "切换到前台"));
        v.push(MenuItem::new(id::MINIMIZE, "最小化"));
        v.push(MenuItem::new(id::CLOSE, "关闭窗口"));
    } else {
        // 「此电脑」「回收站」这类不是程序，说「打开」才准确；
        // 裸协议 URL（`steam://…`、`https://…`）同理；**目录**同理
        // （Dock 上的「临时文件夹」就是个目录，说"启动"很怪）。
        // `.url` 快捷方式**仍然说「启动」**：用户眼里它就是一个游戏/程序。
        let label = if apps::is_shell_location(&t.target)
            || apps::is_url_scheme(&t.target)
            || t.is_dir
        {
            "打开"
        } else {
            "启动"
        };
        v.push(MenuItem::new(id::ACTIVATE, label));
    }
    v.push(MenuItem::divider());

    // 清空回收站：只有回收站才有这一项，而且它是**不可撤销**的
    // （永久删除里面的东西），所以单独一组 + 红字，并且空的时候置灰
    // （"点了没反应"比"看得见但点不动"更让人困惑）。
    if apps::is_recycle_bin(&t.target) {
        let mut empty = MenuItem::new(id::EMPTY_BIN, "清空回收站").danger();
        if t.bin_items == 0 {
            empty = empty.disabled();
        }
        v.push(empty);
        v.push(MenuItem::divider());
    }

    // 已经提权的进程再 runas 一次没有意义，置灰而不是隐藏 ——
    // 隐藏会让人以为「这个程序不支持以管理员身份运行」。
    // 系统位置（此电脑 / 回收站）同样没有"以管理员身份打开"这回事，也置灰。
    // `.url` / 协议 URL 也置灰：runas 提权的是**协议处理器**（Steam），
    // 不是在提权游戏 —— 点了会得到一个"提权后的 Steam"，纯属误导。
    let mut ras = MenuItem::new(id::RUNAS, "以管理员身份运行");
    if t.is_elevated
        || apps::is_shell_location(&t.target)
        || apps::is_url_shortcut(&t.target)
        || apps::is_url_scheme(&t.target)
        // 目录（临时文件夹）没有"以管理员身份打开一个文件夹"这回事
        || t.is_dir
    {
        ras = ras.disabled();
    }
    v.push(ras);
    v.push(MenuItem::divider());

    // 组织类操作。已经在文件夹里的条目不给「新建文件夹」（不支持嵌套）。
    // 文案末尾的「…」是 Windows 的约定：**这一项会弹出对话框**（这里是要用户填名字）。
    if !t.in_folder {
        v.push(MenuItem::new(id::MAKE_FOLDER, "新建文件夹…"));
    }
    v.push(MenuItem::new(id::ADD_SEP, "添加分割线"));

    // 「移除」按层级分**两种语义**，不能共用一个菜单项：
    //   在文件夹里 → 「移出文件夹」= 挪到顶层（不删东西）
    //   在顶层     → 「移出 Dock」  = 从 Dock 上删掉
    // 第一版让两者共用 REMOVE、都走 `store::remove`（彻底删），
    // 于是"移出文件夹"把图标直接删了 —— 文案和行为不一致是最不该有的错。
    if t.in_folder {
        v.push(MenuItem::new(id::MOVE_OUT, "移出文件夹").danger());
    } else {
        v.push(MenuItem::new(id::REMOVE, "移出 Dock").danger());
    }

    // 打包应用（UWP）的 target 是 `shell:AppsFolder\...`，它不是文件路径，
    // `/select` 对它没有意义 —— 置灰，而不是点了报错。
    // 裸协议 URL（`steam://…`）同理：没有"文件所在位置"。
    // （`.url` 快捷方式**是**文件，定位到那个快捷方式是有意义的，所以留着。）
    let mut reveal = MenuItem::new(id::REVEAL, "打开文件所在位置");
    if t.target.starts_with("shell:") || t.target.is_empty() || apps::is_url_scheme(&t.target) {
        reveal = reveal.disabled();
    }
    v.push(reveal);

    v
}

/// 执行一个菜单项。
///
/// 返回 `Err` 时调用方必须把消息显示给用户（前端的 toast）—— 菜单里没有
/// 「操作结果」的位置，静默失败会让用户以为程序坏了。
pub fn run(app: &AppHandle, t: &Target, item_id: &str) -> Result<(), String> {
    let hwnd = HWND(t.hwnd as *mut c_void);
    match item_id {
        id::ACTIVATE => {
            if t.running {
                if t.hwnd == 0 {
                    return Err("这个应用没有可切换的窗口".into());
                }
                apps::activate(hwnd)
            } else {
                apps::launch(&t.target, false)
            }
        }
        id::MINIMIZE => {
            if t.hwnd == 0 {
                return Err("这个应用没有可最小化的窗口".into());
            }
            apps::minimize(hwnd)
        }
        id::CLOSE => {
            if t.hwnd == 0 {
                return Err("这个应用没有可关闭的窗口".into());
            }
            apps::close_window(hwnd)
        }
        id::RUNAS => apps::launch(&t.target, true),
        // 插在这个条目**后面**：用户在哪儿点的右键，分割线就加在哪儿
        id::ADD_SEP => crate::store::add_separator(app, Some(&t.app_id)).map(|_| ()),
        // 顶层和文件夹里都找 —— 用户眼里的"移除"不分层级
        id::REMOVE => crate::store::remove(app, &t.app_id),
        // 只是**挪出来**，不删（文案承诺的就是这个）
        id::MOVE_OUT => crate::store::move_out_of_folder(app, &t.app_id),
        // 命名要用户敲字 → 得开一个**能拿焦点**的窗口（浮层窗口是 NOACTIVATE，
        // 打不了字，见 `prompt_window.rs`）。那个框对 Rust 侧是模态的，所以整件事挪到工作线程：
        // 这条命令是主线程调的，占住它 = Dock 卡住。
        // 菜单此刻已经关了（`panel_invoke` 先收菜单），所以用户看到的是"弹出输入框"。
        id::MAKE_FOLDER => {
            crate::store::spawn_create_folder(app, &t.app_id);
            Ok(())
        }
        // 重命名同样要用户敲字 → 同一个输入框，同样挪到工作线程
        // （默认值就是它现在的名字，改几个字就行）。
        id::RENAME => {
            crate::store::spawn_rename_folder(app, &t.app_id);
            Ok(())
        }
        // 清空回收站：`SHEmptyRecycleBinW` 会弹**系统自己的确认框**并阻塞到用户回答，
        // 所以也不能占主线程（和文件对话框、命名输入框同一个道理）。
        id::EMPTY_BIN => {
            crate::store::spawn_empty_recycle_bin(app);
            Ok(())
        }
        id::DISSOLVE => crate::store::dissolve_folder(app, &t.app_id).map(|_| ()),
        id::REVEAL => reveal_in_explorer(&t.target),
        _ => Err(format!("未知的菜单项: {item_id}")),
    }
}

/// 在资源管理器中定位到该文件
pub fn reveal_in_explorer(path: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};

    // 打包应用的 target 是 shell:AppsFolder\...，explorer /select 对它没有意义
    // （正常情况下这一项已经被置灰，这里是第二道防线）
    if path.starts_with("shell:") || path.is_empty() {
        return Ok(());
    }
    let file = apps::wide("explorer.exe");
    let params = apps::wide(&format!("/select,\"{path}\""));
    unsafe {
        let mut sei = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpFile: windows::core::PCWSTR(file.as_ptr()),
            lpParameters: windows::core::PCWSTR(params.as_ptr()),
            nShow: windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL.0 as i32,
            ..Default::default()
        };
        ShellExecuteExW(&mut sei).map_err(|e| format!("打开资源管理器失败: {e}"))
    }
}

/// 菜单项的**估算**文字宽度（逻辑像素）。
///
/// 为什么在 Rust 侧估：窗口尺寸必须在**显示之前**就定好 ——
/// 先按旧尺寸显示、等页面量完再改，会看到菜单"跳"一下（见 `panel_window.rs` 的显示时序）。
/// 所以宽度不能依赖页面上报。
///
/// 用**偏大**的估算：估大了只是右边多几像素空白（看不出来），
/// 估小了文字会被省略号截断（一眼就看出来）。CJK 按 13px 计是 13px 字号下的满宽。
fn text_width(s: &str) -> f64 {
    s.chars()
        .map(|c| if (c as u32) >= 0x2E80 { 13.0 } else { 7.0 })
        .sum()
}

/// 菜单宽度（逻辑像素）：按最长的一项算，再做区间钳制。
pub fn menu_width(items: &[MenuItem]) -> f64 {
    let widest = items
        .iter()
        .filter(|i| !i.divider)
        .map(|i| text_width(&i.label))
        .fold(0.0f64, f64::max);
    (widest + 28.0).clamp(168.0, 300.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(running: bool, separator: bool) -> Target {
        Target {
            app_id: "c:\\windows\\notepad.exe".into(),
            target: r"C:\Windows\notepad.exe".into(),
            hwnd: 0x1234,
            running,
            is_elevated: false,
            separator,
            is_folder: false,
            in_folder: false,
            children_count: 0,
            is_dir: false,
            bin_items: 0,
        }
    }

    #[test]
    fn separator_menu_only_has_remove() {
        let v = build(&target(false, true));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, id::REMOVE);
        assert!(v[0].danger && v[0].enabled);
    }

    #[test]
    fn not_running_offers_launch_and_runas() {
        let v = build(&target(false, false));
        assert_eq!(v[0].label, "启动");
        assert!(v.iter().any(|i| i.id == id::RUNAS && i.enabled));
        assert!(v.iter().any(|i| i.id == id::ADD_SEP));
        assert!(v.iter().any(|i| i.id == id::REVEAL && i.enabled));
    }

    #[test]
    fn running_offers_window_ops() {
        let v = build(&target(true, false));
        let ids: Vec<&str> = v.iter().map(|i| i.id.as_str()).collect();
        assert!(ids.contains(&id::ACTIVATE));
        assert!(ids.contains(&id::MINIMIZE));
        assert!(ids.contains(&id::CLOSE));
        assert_eq!(ids[0], id::ACTIVATE);
    }

    #[test]
    fn elevated_console_is_greyed_out() {
        let mut t = target(false, false);
        t.is_elevated = true;
        let v = build(&t);
        let ras = v.iter().find(|i| i.id == id::RUNAS).unwrap();
        assert!(!ras.enabled, "已提权时「以管理员身份运行」必须置灰");
    }

    #[test]
    fn uwp_cannot_reveal() {
        let mut t = target(false, false);
        t.target = "shell:AppsFolder\\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into();
        let v = build(&t);
        let r = v.iter().find(|i| i.id == id::REVEAL).unwrap();
        assert!(!r.enabled, "打包应用没有文件位置可打开");
    }

    #[test]
    fn system_location_says_open_and_cannot_run_as_admin() {
        let mut t = target(false, false);
        t.target = "shell:MyComputerFolder".into();
        let v = build(&t);
        let first = &v[0];
        assert_eq!(first.id, id::ACTIVATE);
        assert_eq!(first.label, "打开", "系统位置不是程序，该说「打开」");
        let ras = v.iter().find(|i| i.id == id::RUNAS).unwrap();
        assert!(!ras.enabled, "系统位置没有「以管理员身份打开」这回事");
        let reveal = v.iter().find(|i| i.id == id::REVEAL).unwrap();
        assert!(!reveal.enabled, "系统位置没有文件所在位置可打开");
    }

    #[test]
    fn packaged_app_still_says_launch() {
        // `shell:AppsFolder\...` 也是 shell: 开头，但它是**应用**，不能被当成系统位置
        let mut t = target(false, false);
        t.target = "shell:AppsFolder\\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into();
        let v = build(&t);
        assert_eq!(v[0].label, "启动");
    }

    #[test]
    fn folder_menu_offers_rename_dissolve_and_remove_only() {
        let mut t = target(false, false);
        t.is_folder = true;
        t.children_count = 3;
        let v = build(&t);
        let ids: Vec<&str> = v.iter().map(|i| i.id.as_str()).collect();
        // 重命名排在最前（右键文件夹最常想做的事），和两个"出口"之间用分隔线隔开
        assert_eq!(ids, vec![id::RENAME, "", id::DISSOLVE, "", id::REMOVE]);
        assert!(v[0].label.ends_with('…'), "会弹输入框的项要带省略号（Windows 约定）");
        assert!(v[4].danger, "移出 Dock 是危险操作");
        // ❗文案必须**说清后果**：移出文件夹 = 连里面的东西一起没
        assert!(
            v[4].label.contains("3"),
            "「移出 Dock」要写明里面有几项，实得 {:?}",
            v[4].label
        );
        // 文件夹没有 target，「启动 / 打开文件所在位置」不该出现
        assert!(!ids.contains(&id::ACTIVATE));
        assert!(!ids.contains(&id::REVEAL));
    }

    /// 回收站：多一项「清空回收站」，而且**空的时候要置灰**。
    #[test]
    fn recycle_bin_offers_empty_and_greys_it_when_empty() {
        let mut t = target(false, false);
        t.target = "shell:RecycleBinFolder".into();
        t.bin_items = 4;
        let v = build(&t);
        let empty = v.iter().find(|i| i.id == id::EMPTY_BIN).expect("要有清空回收站");
        assert!(empty.enabled, "回收站里有东西时应该可点");
        assert!(empty.danger, "不可撤销的破坏性操作要红字");
        assert_eq!(empty.label, "清空回收站");
        // 第一项仍然是「打开」（回收站不是程序）
        assert_eq!(v[0].label, "打开");
        // runas 对系统位置没有意义
        assert!(!v.iter().find(|i| i.id == id::RUNAS).unwrap().enabled);

        // 空回收站 → 置灰（"点了没反应"比"看得见点不动"更让人困惑）
        t.bin_items = 0;
        let v2 = build(&t);
        assert!(!v2.iter().find(|i| i.id == id::EMPTY_BIN).unwrap().enabled);
        // 别的条目不该有这一项
        let t3 = target(false, false);
        assert!(!build(&t3).iter().any(|i| i.id == id::EMPTY_BIN));
    }

    /// 目录（Dock 上的「临时文件夹」）：说「打开」不说「启动」，runas 置灰。
    #[test]
    fn directory_target_says_open_and_cannot_run_as_admin() {
        let mut t = target(false, false);
        t.target = r"C:\Users\x\AppData\Local\dev.local.dock\临时文件".into();
        t.is_dir = true;
        let v = build(&t);
        assert_eq!(v[0].id, id::ACTIVATE);
        assert_eq!(v[0].label, "打开", "目录不是程序，说「打开」才准确");
        assert!(!v.iter().find(|i| i.id == id::RUNAS).unwrap().enabled);
        assert!(
            v.iter().find(|i| i.id == id::REVEAL).unwrap().enabled,
            "它是真实路径，「打开文件所在位置」有意义"
        );
    }

    #[test]
    fn child_inside_folder_moves_out_instead_of_being_deleted() {
        let mut t = target(false, false);
        t.in_folder = true;
        let v = build(&t);
        let ids: Vec<&str> = v.iter().map(|i| i.id.as_str()).collect();
        // ❗「移出文件夹」必须是**另一个菜单项**：它只是挪到顶层，不删东西。
        // 第一版让两者共用 REMOVE（走 store::remove = 彻底删），
        // 于是"移出文件夹"把图标删了 —— 文案和行为不一致。
        assert!(
            ids.contains(&id::MOVE_OUT),
            "文件夹里的条目要有「移出文件夹」，实得 {ids:?}"
        );
        assert!(
            !ids.contains(&id::REMOVE),
            "文件夹里**不该**出现「移出 Dock」（那是删掉，得先移出来再做）"
        );
        let mv = v.iter().find(|i| i.id == id::MOVE_OUT).unwrap();
        assert_eq!(mv.label, "移出文件夹");
        assert!(mv.danger);
        assert!(
            !v.iter().any(|i| i.id == id::MAKE_FOLDER),
            "不支持嵌套文件夹，所以文件夹里的条目没有「新建文件夹」"
        );
    }

    #[test]
    fn top_level_app_can_become_folder() {
        let v = build(&target(false, false));
        assert!(v.iter().any(|i| i.id == id::MAKE_FOLDER));
    }

    #[test]
    fn width_is_clamped_and_grows_with_labels() {
        let short = vec![MenuItem::new("a", "启动")];
        let long = vec![MenuItem::new("a", "以管理员身份运行并以极高的权限打开这个程序")];
        assert!(menu_width(&short) >= 168.0);
        assert!(menu_width(&long) <= 300.0);
        assert!(menu_width(&long) > menu_width(&short));
    }

    #[test]
    fn every_offered_item_is_handled_by_run() {
        // 菜单里出现的 id 必须在 run() 里有分支 —— 否则点击会静默无反应。
        // （run() 会真的去操作系统，所以这里只比对 id 集合，不实际执行。）
        let all = build(&target(true, false));
        let known = [
            id::ACTIVATE,
            id::MINIMIZE,
            id::CLOSE,
            id::RUNAS,
            id::ADD_SEP,
            id::REMOVE,
            id::REVEAL,
            id::MAKE_FOLDER,
            id::DISSOLVE,
        ];
        for i in all.iter().filter(|i| !i.divider) {
            assert!(known.contains(&i.id.as_str()), "未处理的菜单项 {}", i.id);
        }
    }
}
