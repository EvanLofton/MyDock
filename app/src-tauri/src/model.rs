//! 应用模型：Dock 上「一个条目」的纯数据结构。
//!
//! 不依赖 Tauri，便于单测与复用。

use serde::{Deserialize, Serialize};

/// 应用类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppKind {
    /// 普通桌面程序
    Win32,
    /// 商店 / UWP 应用
    Uwp,
    /// 无法识别
    Unknown,
}

/// 对单个窗口的引用（HWND 以 u64 传前端，指针不可跨 IPC 语义化）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowRef {
    pub hwnd: u64,
    pub title: String,
    pub minimized: bool,
}

/// Dock 上的一个应用条目
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppEntry {
    /// 稳定标识。当前实现用**可执行文件全路径的小写形式**；
    /// 打包应用用 `uwp:<AUMID>`（见 `apps.rs`）。
    pub id: String,
    /// 展示名。优先取 exe 的版本信息 FileDescription，退回文件名。
    pub display_name: String,
    pub kind: AppKind,
    /// 启动目标：Win32 是 exe 全路径；打包应用是 `shell:AppsFolder\<AUMID>`
    pub target: String,
    /// 是否以管理员权限运行 —— 决定是否画盾牌角标、置灰写入类操作
    pub is_elevated: bool,
    /// 该应用的某个窗口当前是否在前台
    pub has_foreground: bool,
    /// **当前是否有窗口在运行**。
    /// 固定但未运行的应用 `running = false`、`windows` 为空 ——
    /// 前端据此决定点击是「启动」还是「切换」。
    pub running: bool,
    pub windows: Vec<WindowRef>,
    /// 这一条是**分割线**（纯视觉分组），不是应用。
    ///
    /// 前端据此渲染一条竖线而不是图标；其余字段对它没有意义
    /// （`target` 为空、`running` 恒为 false）。
    #[serde(default)]
    pub separator: bool,
    /// 这一条是**文件夹**（Dock 上的大文件夹）：占一个图标位，点开显示 `children`。
    ///
    /// 它自己不是应用（没有 target、不参与"在不在运行"的匹配）；
    /// `running` 表示**里面有任何东西在运行**，用来点亮运行指示点。
    #[serde(default)]
    pub is_folder: bool,
    /// 文件夹里的条目（**不在顶层列表里**，只有展开时才看得见）。
    /// 非文件夹恒为空。
    #[serde(default)]
    pub children: Vec<AppEntry>,
}

/// 用户放在 Dock 上的一个条目（应用**或**分割线）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PinnedApp {
    pub id: String,
    pub display_name: String,
    pub target: String,
    /// 分割线：纯视觉分组，放在哪里、分几段完全由用户决定。
    ///
    /// 它在列表里就是一个普通条目 —— 能拖动排序、能拖出去删除、能被右键移除，
    /// 所以**不需要**单独维护一份「分割线位置」的数据结构，`pinned` 的顺序就是全部真相。
    /// `#[serde(default)]`：老配置里没有这个字段，读进来就是 `false`（普通应用）。
    #[serde(default)]
    pub separator: bool,
    /// 这一条是**文件夹**（Dock 上的大文件夹，像手机桌面那种）。
    ///
    /// 和分割线一样是**列表里的普通条目**：占一个图标位、能拖动、能拖出去删除。
    /// 区别是它带着 `children`（里面的应用，不占顶层位置）。
    /// 文件夹里**不能再放文件夹**（`add_app_to_folder` 会拒绝）——
    /// 嵌套会让"点开→再点开"变成一条没有尽头的路径，而收益是零。
    #[serde(default)]
    pub is_folder: bool,
    /// 文件夹里的条目。非文件夹恒为空。
    #[serde(default)]
    pub children: Vec<PinnedApp>,
}
