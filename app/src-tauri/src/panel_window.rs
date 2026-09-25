//! Dock 的**浮层窗口** —— 右键菜单和文件夹面板都住在这里。
//!
//! # 为什么是独立窗口
//!
//! Dock 窗口只有 **56~72 逻辑像素高**，页面里画的任何浮层都会被窗口裁掉
//! （窗口是透明的 ≠ 内容能画到窗口外）。所以浮层必须有自己的窗口，
//! 尺寸贴合内容本身。
//!
//! # 为什么菜单和文件夹**共用**一个窗口
//!
//! 两者要的是同一套东西：贴 Dock 上方、以光标为中心锚定、不抢焦点、
//! 点别处/Esc 关闭、显示前先让页面渲染完（避免闪一下旧内容）。
//! 各建一个窗口就是把这一整套（含看门线程）抄两遍，而且还要额外处理
//! 「菜单和文件夹同时开着」这种没人想要的组合。
//! 共用一个窗口之后：**同一时刻只可能有一个浮层**，那是本来就该有的行为。
//!
//! # 为什么不用原生 `TrackPopupMenu`
//!
//! 原生菜单是最省事的方案，但有两个不可接受的代价：
//!  1. **外观无法定制** —— 与 Dock 的玻璃观感是两种东西；
//!  2. `TrackPopupMenu` 自带**模态消息循环**，会把调用它的线程整个卡住
//!     （旧实现里必须专门丢到 `spawn_blocking` 去跑，否则 Tauri 的事件循环被卡死）。
//!
//! # 显示/隐藏逻辑（这是本模块的核心，改动前务必读完）
//!
//! **显示**分两段，中间隔着一次页面回调：
//!
//! ```text
//!   右键/点文件夹 → show_*()  ── 算出内容与矩形、写好状态 ─┐
//!                                                        │ eval("__dockPanel(seq)")
//!                                                        ▼
//!                                             页面渲染 → invoke("panel_ready", seq)
//!                                                        │
//!                                                        ▼
//!                                               SetWindowPos + SWP_SHOWWINDOW
//! ```
//!
//! 为什么不直接显示：窗口尺寸是 Rust 按内容算的，而**页面内容是异步渲染的**。
//! 先显示再渲染，会看到窗口里先是上一次的内容、几毫秒后才换成新的（一次闪烁）。
//! 所以由页面在渲染完成后回调 `panel_ready`，Rust 收到才把窗口显示出来。
//! 同时挂一个 `READY_TIMEOUT` 兜底 —— 页面万一出问题，点击也不能毫无反应。
//!
//! **隐藏**有四条路径，全部收敛到 `hide()`（幂等）：
//!
//! | 触发 | 实现 |
//! |------|------|
//! | 选了某一项 / 点了文件夹里的图标 | `panel_invoke` 先取目标、再 `hide()`、最后执行 |
//! | 按了 Esc | 看门线程轮询 `GetAsyncKeyState` |
//! | 点了浮层和 Dock 以外的地方 | 看门线程轮询鼠标按下 + 光标位置 |
//! | 点了 Dock 上的东西 | Dock 页面的 `mousedown` 调 `hide_panel` |
//!
//! 为什么靠**轮询**判断「点了别处」：浮层窗口是 `WS_EX_NOACTIVATE` 的，
//! 它**永远不会成为前台窗口**，因此收不到 `WM_KILLFOCUS`／失活通知
//! ——「点外面自动消失」这件事没有消息可等。而这条特性又是必须的
//! （浮层夺走焦点会让用户正在打字的窗口失焦）。看门线程只在浮层打开期间存在，
//! 16ms 一次，几乎不耗 CPU，关闭时立刻退出。
//!
//! 注意看门线程**故意忽略落在 Dock 窗口上的按下**：那一路由 Dock 页面自己处理
//! （它可能是在右键另一个图标 = 浮层换内容）。两边都管就会互相打架，
//! 表现为「浮层一闪就没了」。

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use windows::Win32::Foundation::{HWND, POINT};
// `ClientToScreen` 在 Gdi 模块里（不在 WindowsAndMessaging），别按直觉找
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON, VK_RBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::menu::{self, MenuItem, Target};
use crate::model::AppEntry;

/// 窗口标签。Rust 与前端都靠它找窗口，不要散落字面量。
pub const LABEL: &str = "panel";

// ---------------------------------------------------------------- 浮层几何
//
// ⚠️ 这一组是**唯一真相**：Rust 用它们算窗口尺寸，页面用它们渲染。
// 两边各写一份的话，唯一的后果是最后一行被裁掉或底部多一块空白，
// 所以它们通过 `PanelPayload` 一起发给页面，页面直接用，CSS 里不再另写一份。

/// 菜单单项高度（逻辑像素）
pub const ROW_H: f64 = 26.0;
/// 菜单内部分隔线占的高度
pub const DIVIDER_H: f64 = 9.0;
/// 浮层内边距（菜单用上下；文件夹网格另有 `FOLDER_PAD`）
pub const PAD: f64 = 5.0;
/// 文件夹网格：列数。4 列是"一眼能扫完"和"不长条"的折中。
pub const FOLDER_COLS: u32 = 4;
/// 文件夹网格：格子之间、以及格子与面板边缘的间距
pub const FOLDER_GAP: f64 = 6.0;
pub const FOLDER_PAD: f64 = 12.0;
/// 文件夹网格最多显示几行（再多就滚动）。上限同时受屏幕可用高度约束。
pub const FOLDER_MAX_ROWS: u32 = 5;
/// 浮层底边与 Dock 顶边的间距
const GAP_ABOVE_DOCK: f64 = 8.0;
/// 浮层亚克力不透明度的下限/上限。
///
/// 下限的存在理由：Dock 的透明度是用户可调的（可以调到几乎全透），
/// 但浮层上有**文字和图标**，透到一定程度就读不清了；上限则是因为
/// 实测 alpha ≥ 220 时模糊基本消失（见 `glass::apply_acrylic` 的标定表）。
const PANEL_ACRYLIC_MIN: u8 = 150;
const PANEL_ACRYLIC_MAX: u8 = 200;
/// 悬停标签的底色往白里提多少（0 = 保持 Dock 的着色，1 = 纯白）。
///
/// 为什么单独给标签提亮：它只有一行字、尺寸又小，用 Dock 那个深色调（默认 `[24,24,28]`）
/// 渲染出来是一块中灰玻璃压着白字 —— 实测底色亮度只有 ~147，
/// 白字与它对比不足 2:1，看着"发闷、发暗"（用户原话："名称提示框的颜色有点暗"）。
///
/// 提到 0.62 之后底色亮度约 165-200，于是 `is_dark` **自己**就把文字翻成深色
/// （见它的 WCAG 推导），对比度反而从 ~1.9 升到 8 以上。
/// 菜单和文件夹**不动**：那里有图标和整行高亮，深色底才是对的设计。
const LABEL_BRIGHTEN: f64 = 0.62;
/// 页面渲染完成回调的兜底超时（见模块说明）
const READY_TIMEOUT: Duration = Duration::from_millis(250);

/// 浮层要渲染的内容。用 `kind` 区分三种形态 —— 页面上是同一个 React 树。
///
/// ⚠️ **每个变体都要单独写 `rename_all`**：枚举上的 `rename_all = "camelCase"`
/// 只改变**变体名**（`Menu` → `menu`），**不改变体里的字段名**。
/// 漏了它的后果是：Rust 发 `row_h` / `divider_h`，页面读 `rowH` / `dividerH`
/// 全是 `undefined` → 行高失效，菜单挤成一条（本轮就是这么坏的，而且类型检查
/// 和已有的断言都看不出来，因为 `items` 恰好两边同名）。
/// 见下面的 `payload_field_names_are_camel_case` 单测。
#[derive(Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PanelContent {
    /// 右键菜单：一列菜单项
    #[serde(rename_all = "camelCase")]
    Menu {
        items: Vec<MenuItem>,
        row_h: f64,
        divider_h: f64,
    },
    /// 文件夹：一个图标网格
    #[serde(rename_all = "camelCase")]
    Folder {
        title: String,
        cols: u32,
        cell: f64,
        gap: f64,
        items: Vec<FolderItem>,
    },
    /// 悬停标签：图标上方那个小气泡（macOS 的观感）
    #[serde(rename_all = "camelCase")]
    Label { text: String, font_size: f64 },
}

/// 悬停标签的字号与内边距（逻辑像素）
pub const LABEL_FONT: f64 = 13.0;
pub const LABEL_PAD_X: f64 = 10.0;
pub const LABEL_PAD_Y: f64 = 5.0;

/// 文字宽度估算（逻辑像素）。
///
/// 窗口尺寸必须在**显示之前**定好（见模块说明的显示时序），所以不能等页面上报。
/// 估算按**偏大**取：大了只是右边多几像素空白，小了文字会被裁掉（一眼就看出来）。
fn text_width(s: &str, font_size: f64) -> f64 {
    s.chars()
        .map(|c| {
            if (c as u32) >= 0x2E80 {
                font_size
            } else {
                font_size * 0.58
            }
        })
        .sum()
}

/// 文件夹网格里的一格
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderItem {
    pub id: String,
    pub label: String,
    pub target: String,
    /// 在运行（画运行点；点击时决定是"启动"还是"切到前台"）
    pub running: bool,
}

/// 页面要渲染的全部内容
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelPayload {
    /// 每次显示自增。页面回调时带回来，用来丢弃过期的回调
    pub seq: u64,
    pub content: PanelContent,
    /// 背景偏暗 → 用浅色文字（由 Dock 的着色亮度决定，见 `is_dark`）
    pub dark: bool,
    pub pad: f64,
    /// 浮层宽度（逻辑像素）
    pub width: f64,
    /// 页面遮罩用的颜色，与 Rust 侧的亚克力同色（两层一起决定观感）
    pub glass_rgb: [u8; 3],
}

/// 当前这一次显示的全部状态
struct Open {
    /// 当前浮层对应的**条目 id**：菜单是那个应用，文件夹是那个文件夹。
    /// 用来实现「同一个图标再点一次 = 关掉」。
    target_id: String,
    payload: PanelPayload,
}

/// 一个浮层窗口的全部状态。
///
/// # 为什么要有"槽"这个概念
///
/// 右键菜单和文件夹面板是**两层叠着**的：在文件夹里右键一个图标，
/// 菜单要盖在文件夹上面，文件夹**不能关**；点外面先关菜单、再点才关文件夹。
///
/// 于是同一套「创建 / 定位 / 显示时序 / 看门线程 / 暂停自动隐藏」要跑在**两个窗口**上。
/// 把状态收进结构体、开两个实例，比把那套逻辑抄两遍可靠得多 ——
/// 抄两遍的话，"点外面该关哪一层"这种规则就有两个地方要实现。
pub struct Slot {
    /// 窗口标签（Rust 侧取窗口、自检断言都用它）
    label: &'static str,
    /// 窗口标题（只用于诊断）
    title: &'static str,
    hwnd: AtomicUsize,
    visible: AtomicBool,
    /// 每次 `hide()` 自增。看门线程凭它判断「自己服务的那一次显示已经结束了」
    gen: AtomicU64,
    seq: AtomicU64,
    cur: Mutex<Option<Open>>,
    /// 已经算好、等页面渲染完就显示的矩形 `(left, top, right, bottom)`
    pending: Mutex<Option<(u64, (i32, i32, i32, i32))>>,
}

impl Slot {
    const fn new(label: &'static str, title: &'static str) -> Self {
        Self {
            label,
            title,
            hwnd: AtomicUsize::new(0),
            visible: AtomicBool::new(false),
            gen: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            cur: Mutex::new(None),
            pending: Mutex::new(None),
        }
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd.load(Ordering::SeqCst) as *mut c_void)
    }

    fn is_visible(&self) -> bool {
        self.visible.load(Ordering::SeqCst)
    }

    /// 原始 HWND 数值（0 = 还没建）。给"当别的浮层的定位基准"用。
    fn hwnd_raw(&self) -> u64 {
        self.hwnd.load(Ordering::SeqCst) as u64
    }
}

/// **文件夹层**（下面那层）
static FOLDER: Slot = Slot::new("panel", "Dock 文件夹");
/// **菜单层**（盖在文件夹上面的那层）。
///
/// 它同时承担**悬停标签**（`PanelContent::Label`）—— 三种浮层内容共用这一个窗口，
/// 省掉第三个 WebView（每个窗口一个 WebView2 进程，实测约 +50MB）。
/// 所以窗口标题是个中性的名字，别叫"菜单"。
static MENU: Slot = Slot::new("menu", "Dock 浮层");

/// 最上面那一层 —— 关闭 / Esc 先作用于它。
///
/// 这就是"逐层关闭"的全部实现：先关菜单、菜单没了再关文件夹。
fn top_slot() -> Option<&'static Slot> {
    if MENU.is_visible() {
        Some(&MENU)
    } else if FOLDER.is_visible() {
        Some(&FOLDER)
    } else {
        None
    }
}

/// 按窗口标签取槽：每个浮层窗口自己回调 `panel_ready` / `panel_state`，
/// Rust 得知道是哪一个在问（两个窗口共用同一个页面）。
fn slot_for_label(label: &str) -> Option<&'static Slot> {
    match label {
        "panel" => Some(&FOLDER),
        "menu" => Some(&MENU),
        _ => None,
    }
}

/// 某个浮层窗口读自己要渲染的内容（`commands::panel_state` 用）
pub fn payload_for_label(label: &str) -> Option<PanelPayload> {
    slot_for_label(label)?.payload()
}

/// 某个浮层页面渲染完成（`commands::panel_ready` 用）
pub fn ready_for_label(label: &str, app: &AppHandle, seq: u64) {
    if let Some(slot) = slot_for_label(label) {
        slot.ready(app, seq);
    }
}

/// 关掉**菜单层**（`commands::panel_invoke` 用：菜单里的动作做完，只收菜单）
pub fn hide_menu() {
    MENU.hide();
}

/// 关掉**文件夹层**（刷新失败之类：没什么可展示的就收掉它）
pub fn hide_folder() {
    FOLDER.hide();
}

// ---------------------------------------------------------------- 创建

/// 创建两个浮层窗口（**启动时**调用一次，创建后保持隐藏）。
///
/// 为什么启动就建而不是第一次右键时再建：WebView2 的窗口初始化要几百毫秒，
/// 第一次右键会明显卡一下 —— 而右键是高频动作，这个代价不能接受。
/// 隐藏的窗口不参与合成，不渲染就没有开销。
///
/// 代价是每个窗口一个 WebView2 进程（实测多一个约 +50MB 私有内存，见 README）。
/// 但两层叠着显示**必须**是两个窗口：一个窗口的话，窗口矩形就得是两个面板的并集，
/// 而亚克力是铺满整个窗口矩形的 —— 并集里那些空的地方会糊成一片玻璃。
pub fn create_all(app: &AppHandle) -> Result<(), String> {
    for slot in [&FOLDER, &MENU] {
        slot.create(app)?;
    }
    Ok(())
}

impl Slot {
    fn create(&self, app: &AppHandle) -> Result<(), String> {
        if app.get_webview_window(self.label).is_some() {
            return Ok(());
        }
        let win = WebviewWindowBuilder::new(app, self.label, WebviewUrl::App("panel.html".into()))
            .title(self.title)
            // 尺寸只是初值：每次显示都会按内容重算（见 `place`）
            .inner_size(200.0, 120.0)
            .transparent(true)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .shadow(false)
            .focused(false)
            .visible(false)
            .build()
            .map_err(|e| format!("创建浮层窗口 {} 失败: {e}", self.label))?;

        let raw = win.hwnd().map_err(|e| e.to_string())?;
        let hwnd = HWND(raw.0 as *mut c_void);
        self.hwnd.store(raw.0 as usize, Ordering::SeqCst);

        // 与 Dock 完全相同的扩展样式：不夺焦点（WS_EX_NOACTIVATE）、不进任务栏
        let (before, after) = crate::win_layer::apply_dock_ex_style(hwnd);
        // DWM 圆角 + 去掉系统边框。亚克力铺满整个窗口矩形，圆角只能交给 DWM（见 glass.rs）
        crate::glass::apply_window_material(hwnd);

        log_info!(
            "[浮层] {} 窗口已创建 0x{:X}  扩展样式 0x{before:08X} -> 0x{after:08X}",
            self.label,
            raw.0 as usize
        );
        Ok(())
    }
}

// ---------------------------------------------------------------- 显示

/// 显示**悬停标签**（鼠标停在 Dock 图标上时，图标上方那个小气泡）。
///
/// # 为什么用浮层窗口，而不是 `title` 属性
///
/// `title` 走的是 WebView2/系统原生 tooltip —— 方角、系统配色、出现在光标右下角，
/// 和 Dock 的玻璃观感完全不是一套（用户："很丑"）。
/// macOS 的做法是**图标上方**一个同材质的小气泡，所以这里自己画：
/// 复用浮层窗口（**菜单层**，不新增 WebView）+ 一个 `Label` 形态的内容。
///
/// 同一条目标重复调用（鼠标在同一图标上移动）不会重新弹出，避免闪烁。
///
/// `above_folder` = 这个标签是**文件夹格里**的图标要的：那就该贴在文件夹面板上方，
/// 而不是跑回屏幕底部贴 Dock（标签会离图标很远）。此时 `anchor_x` 是**文件夹面板**的客户端坐标。
pub fn show_label(
    app: &AppHandle,
    dock_hwnd: u64,
    target_id: &str,
    text: &str,
    anchor_x: f64,
    above_folder: bool,
) -> Result<bool, String> {
    // 菜单开着的时候不弹标签：同一个窗口，弹了会把菜单顶掉。
    // （用户正看着菜单，标签也没意义。）
    let menu_open = MENU
        .cur
        .lock()
        .unwrap()
        .as_ref()
        .map(|o| matches!(o.payload.content, PanelContent::Label { .. }))
        == Some(false)
        && MENU.is_visible();
    if menu_open {
        return Ok(false);
    }
    // 同一个图标 + 同样的文字 → 已经在显示了，什么都不用做
    if MENU.is_visible() {
        let same = MENU
            .cur
            .lock()
            .unwrap()
            .as_ref()
            .map(|o| o.target_id == target_id)
            .unwrap_or(false);
        if same {
            return Ok(true);
        }
    }

    // 贴在文件夹面板上方时，base 换成文件夹窗口自己。
    // 拿不到句柄（理论上不会：面板正显示着）就退回 Dock —— 至少标签还在。
    let folder_hwnd = if above_folder { FOLDER.hwnd_raw() } else { 0 };
    let base_hwnd = if folder_hwnd != 0 { folder_hwnd } else { dock_hwnd };

    let width_log = text_width(text, LABEL_FONT) + 2.0 * LABEL_PAD_X;
    let height_log = LABEL_FONT + 2.0 * LABEL_PAD_Y;
    let content = PanelContent::Label {
        text: text.to_string(),
        font_size: LABEL_FONT,
    };
    let label = format!("标签「{text}」");
    MENU.present(app, base_hwnd, target_id, label, content, width_log, height_log, anchor_x)
}

/// 关掉悬停标签（鼠标移开）。**只关标签**：真的是菜单就留着。
pub fn hide_label() {
    let is_label = MENU
        .cur
        .lock()
        .unwrap()
        .as_ref()
        .map(|o| matches!(o.payload.content, PanelContent::Label { .. }))
        .unwrap_or(false);
    if is_label {
        MENU.hide();
    }
}

/// 显示右键菜单。
///
/// `keep_folder` = 这次右键是**从文件夹里**发起的（点文件夹里的图标）：
/// 那时文件夹要留在下面（用户明确要求"文件夹不应该关"）；
/// 从 Dock 上右键则相反 —— 目标已经不是那个文件夹了，留着没意义。
pub fn show_menu(
    app: &AppHandle,
    dock_hwnd: u64,
    target: Target,
    anchor_x: f64,
    keep_folder: bool,
) -> Result<bool, String> {
    if !keep_folder {
        FOLDER.hide();
    }
    // 同一个图标再次右键 = 关掉。macOS 的右键菜单就是这个行为，
    // 而且没有它的话「菜单挡住了图标，再右键一次想取消」就只能靠点别处。
    //
    // ⚠️ 但**只有"已经显示着同一条目的真菜单"才算再点一次**。
    // 悬停标签用的是同一个窗口、同一条 `target_id` —— 把它也算成"上一次的菜单"的话，
    // 右键一个正显示名字的图标会变成"关掉标签"，菜单永远弹不出来（真踩到了：
    // 自检里「右键 → 菜单窗口已显示」报红，量到的"菜单矩形"其实是 74x29 的标签）。
    if MENU.same_menu_open(&target.app_id) {
        MENU.hide();
        return Ok(false);
    }

    let items = menu::build(&target);
    if items.is_empty() {
        return Err("这个条目没有可用的菜单项".into());
    }
    let width_log = menu::menu_width(&items);
    let height_log = PAD * 2.0
        + items
            .iter()
            .map(|i| if i.divider { DIVIDER_H } else { ROW_H })
            .sum::<f64>();
    let n = items.len();
    let content = PanelContent::Menu {
        items,
        row_h: ROW_H,
        divider_h: DIVIDER_H,
    };
    let label = format!("菜单 目标={} 项数={n}", target.app_id);
    MENU.present(app, dock_hwnd, &target.app_id, label, content, width_log, height_log, anchor_x)
}

/// 显示**文件夹面板**（Dock 上的大文件夹点开后展开的图标网格）。
///
/// `children` 是文件夹里的条目（`enumerate_dock_apps` 已经带上运行态了）。
pub fn show_folder(
    app: &AppHandle,
    dock_hwnd: u64,
    folder_id: &str,
    title: &str,
    children: &[AppEntry],
    anchor_x: f64,
) -> Result<bool, String> {
    // 展开文件夹时把菜单收掉：菜单是"对某个图标的一次性操作"，
    // 而文件夹是"一个持续的视图"，两者不该同时第一次出现
    MENU.hide();
    show_folder_keep_menu(app, dock_hwnd, folder_id, title, children, anchor_x)
}

/// 和 `show_folder` 一样，但**不动菜单层**。
///
/// 用途：文件夹里的菜单做完动作之后，把文件夹**原地刷新**一次
/// （条目可能少了一个），此时菜单层已经自己关了，不该再去碰它。
pub fn show_folder_keep_menu(
    app: &AppHandle,
    dock_hwnd: u64,
    folder_id: &str,
    title: &str,
    children: &[AppEntry],
    anchor_x: f64,
) -> Result<bool, String> {
    let items: Vec<FolderItem> = children
        .iter()
        .filter(|c| !c.separator)
        .map(|c| FolderItem {
            id: c.id.clone(),
            label: c.display_name.clone(),
            target: c.target.clone(),
            running: c.running,
        })
        .collect();
    if items.is_empty() {
        return Err("这个文件夹是空的".into());
    }
    if FOLDER.toggle_off_if_same(folder_id) {
        return Ok(false);
    }

    let (width_log, height_log) = folder_size(app, items.len());
    let content = PanelContent::Folder {
        title: title.to_string(),
        cols: FOLDER_COLS,
        cell: folder_cell(app),
        gap: FOLDER_GAP,
        items,
    };
    let label = format!("文件夹 {folder_id}（{title}）");
    FOLDER.present(app, dock_hwnd, folder_id, label, content, width_log, height_log, anchor_x)
}

impl Slot {
    /// 「同一个条目再点一次」= 关掉自己。返回 `true` 表示这次点击被当成关闭。
    ///
    /// 用于**文件夹层**（它只承载一种内容）。菜单层要的是 `same_menu_open`
    /// —— 那个窗口还会被悬停标签借用，不能只比 id。
    fn toggle_off_if_same(&self, id: &str) -> bool {
        if !self.is_visible() {
            return false;
        }
        let same = self
            .cur
            .lock()
            .unwrap()
            .as_ref()
            .map(|o| o.target_id == id)
            .unwrap_or(false);
        if same {
            self.hide();
            return true;
        }
        false
    }

    /// 当前是不是**已经显示着同一条目的真菜单**？
    ///
    /// 悬停标签（`Label`）不算：它是跟着鼠标走的名字气泡，不是"上一次的菜单"。
    fn same_menu_open(&self, id: &str) -> bool {
        if !self.is_visible() {
            return false;
        }
        self.cur
            .lock()
            .unwrap()
            .as_ref()
            .map(|o| o.target_id == id && matches!(o.payload.content, PanelContent::Menu { .. }))
            .unwrap_or(false)
    }
}

/// 浮层格子边长：跟着图标尺寸走，但夹在一个可点的区间里。
fn folder_cell(app: &AppHandle) -> f64 {
    let icon = app
        .state::<crate::store::PrefsState>()
        .0
        .lock()
        .unwrap()
        .icon_size as f64;
    (icon + 14.0).clamp(40.0, 72.0)
}

/// 文件夹面板的逻辑尺寸。
///
/// 行数受**屏幕可用高度**约束（面板必须能整个放在 Dock 上方），
/// 超出的部分由页面滚动 —— 所以算出来的高度是"最多显示这么多行"。
fn folder_size(app: &AppHandle, n: usize) -> (f64, f64) {
    let cell = folder_cell(app);
    let cols = FOLDER_COLS as f64;
    let rows_wanted = ((n as f64) / cols).ceil().max(1.0);
    let width = cols * cell + (cols - 1.0) * FOLDER_GAP + 2.0 * FOLDER_PAD;

    // 屏幕可用高度（物理）→ 逻辑；再扣掉 Dock 面板与间隙
    let wa = crate::win_layer::work_area();
    let scale = app
        .get_webview_window(LABEL)
        .and_then(|w| w.hwnd().ok())
        .map(|h| crate::win_layer::dpi_of(HWND(h.0 as *mut c_void)) as f64 / 96.0)
        .unwrap_or(1.0);
    let panel_h_log = app
        .state::<crate::store::PrefsState>()
        .0
        .lock()
        .unwrap()
        .panel_height as f64;
    let avail_log = (wa.bottom - wa.top) as f64 / scale - panel_h_log - GAP_ABOVE_DOCK - 24.0;
    let max_rows = (((avail_log - 2.0 * FOLDER_PAD + FOLDER_GAP) / (cell + FOLDER_GAP)).floor())
        .clamp(1.0, FOLDER_MAX_ROWS as f64);
    let rows = rows_wanted.min(max_rows);
    let height = rows * cell + (rows - 1.0) * FOLDER_GAP + 2.0 * FOLDER_PAD;
    (width, height)
}

/// 真正把浮层摆出来：算矩形 → 上亚克力 → 写状态 → 通知页面 → 等页面回调再显示。
impl Slot {
    #[allow(clippy::too_many_arguments)]
    fn present(
        &self,
        app: &AppHandle,
        base_hwnd: u64,
        target_id: &str,
        label: String,
        content: PanelContent,
        width_log: f64,
        height_log: f64,
        anchor_x: f64,
    ) -> Result<bool, String> {
        let win = app
            .get_webview_window(self.label)
            .ok_or_else(|| format!("浮层窗口 {} 不存在（启动时创建失败？）", self.label))?;
        let mhwnd = self.hwnd();
        if mhwnd.0.is_null() {
            return Err("浮层窗口句柄无效".into());
        }
        let base = HWND(base_hwnd as *mut c_void);
        if base.0.is_null() {
            return Err("浮层要贴的那个窗口句柄无效".into());
        }

        let prefs = app
            .state::<crate::store::PrefsState>()
            .0
            .lock()
            .unwrap()
            .clone();

        let (w, h) = crate::win_layer::logical_to_physical(mhwnd, width_log, height_log);
        let rect = place(base, mhwnd, anchor_x, w, h);

        // 色调跟随 Dock，但**不透明度有下限** —— 浮层上有文字和图标，
        // 必须在任意桌面内容上可读。
        //
        // 悬停标签例外：它单独往白里提一档（理由见 `LABEL_BRIGHTEN`）。
        // 提亮之后下面那句 `is_dark(tint)` 会自己把页面文字翻成深色 ——
        // 页面不判断深浅，只认 `dark` 这一个开关。
        let tint = match content {
            PanelContent::Label { .. } => brighten(prefs.glass_rgb, LABEL_BRIGHTEN),
            _ => prefs.glass_rgb,
        };
        let alpha = prefs
            .glass_alpha
            .clamp(PANEL_ACRYLIC_MIN, PANEL_ACRYLIC_MAX);
        let glass = crate::glass::apply_acrylic(mhwnd, (tint[0], tint[1], tint[2], alpha));

        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let payload = PanelPayload {
            seq,
            content,
            // 用**提亮后**的色调判深浅：标签提亮之后这里自动变成 false（→ 深色文字），
            // 页面的遮罩色也跟着用同一个值，两处不会各判一次、各得到相反结论。
            dark: is_dark(tint),
            pad: PAD,
            width: width_log,
            glass_rgb: tint,
        };
        log_info!(
            "[浮层] {} seq={seq} {label} 逻辑 {width_log:.0}x{height_log:.0} 物理 ({},{}) {}x{}  {glass}",
            self.label, rect.0, rect.1, w, h
        );

        *self.cur.lock().unwrap() = Some(Open {
            target_id: target_id.to_string(),
            payload,
        });
        *self.pending.lock().unwrap() = Some((seq, rect));

        // 让页面先渲染，渲染完它回调 panel_ready，那时才真正显示（见模块说明）
        //
        // ⚠️ 通知失败必须把状态清干净：否则「提示出错 + 250ms 后浮层还是弹出来了」
        // 两件事会同时发生，用户看到的是自相矛盾的行为。
        if let Err(e) = win.eval(&format!("window.__dockPanel && window.__dockPanel({seq});")) {
            *self.pending.lock().unwrap() = None;
            *self.cur.lock().unwrap() = None;
            return Err(format!("通知浮层页面失败: {e}"));
        }

        // 兜底：页面没回调也要显示出来。
        // 闭包拿不到 `&self`（Slot 是静态、但闭包要 'static），所以把 label 带过去查。
        let a = app.clone();
        let label_owned = self.label;
        std::thread::spawn(move || {
            std::thread::sleep(READY_TIMEOUT);
            if let Some(slot) = slot_for_label(label_owned) {
                if let Some(r) = slot.take_pending(seq) {
                    slot.apply_pending(&a, r);
                }
            }
        });

        Ok(true)
    }

    /// 页面渲染完成（`commands::panel_ready`）。
    pub fn ready(&self, app: &AppHandle, seq: u64) {
        if let Some(r) = self.take_pending(seq) {
            self.apply_pending(app, r);
        }
    }

    fn take_pending(&self, seq: u64) -> Option<(i32, i32, i32, i32)> {
        let mut g = self.pending.lock().unwrap();
        match *g {
            Some((s, r)) if s == seq => {
                *g = None;
                Some(r)
            }
            _ => None,
        }
    }

    /// 定位并显示窗口。只在这里 `SWP_SHOWWINDOW`。
    fn apply_pending(&self, app: &AppHandle, rect: (i32, i32, i32, i32)) {
        let h = self.hwnd();
        if h.0.is_null() {
            return;
        }
        // `swap` 的返回值 = 「之前是不是已经显示着」。决定要不要重新申请暂停、
        // 重新起看门线程 —— 换一个图标右键时浮层一直是打开的，不能重复申请
        // （重复 pause 而只 unpause 一次的话，自动隐藏就永久失效了）。
        let was_visible = self.visible.swap(true, Ordering::SeqCst);
        unsafe {
            let _ = SetWindowPos(
                h,
                Some(HWND_TOPMOST),
                rect.0,
                rect.1,
                rect.2 - rect.0,
                rect.3 - rect.1,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        if !was_visible {
            // 浮层打开期间禁止自动隐藏（设计文档 §4.3 的硬要求）。
            // `reveal::pause` 是**计数**的，所以两层各 pause 一次、各 unpause 一次，正确。
            crate::reveal::pause();
            ensure_watchdog(app);
        }
    }
}

/// 计算浮层该出现在哪儿。
///
/// `base` = **浮层要贴的那个窗口**：菜单 / 文件夹贴 Dock，文件夹里的悬停标签贴**文件夹面板自己**
/// （标签该在那个图标上方，而不是跑回 Dock 上方）。`anchor_x` 是 `base` 客户端里的逻辑横坐标。
///
/// 默认贴在 base **上方**（Dock 在屏幕底部，浮层在它下面没有空间），
/// 水平以锚点为中心 —— 光标/图标中心才是用户眼里「我指的地方」。
fn place(base: HWND, panel: HWND, anchor_x: f64, w: i32, h: i32) -> (i32, i32, i32, i32) {
    use crate::win_layer::{dpi_of, logical_px, window_rect, work_area};

    let dr = window_rect(base);
    let scale = dpi_of(base) as f64 / 96.0;
    // 页面报的是**客户端逻辑像素**，先换算成物理再交给 ClientToScreen
    let mut pt = POINT {
        x: (anchor_x * scale).round() as i32,
        y: 0,
    };
    unsafe {
        let _ = ClientToScreen(base, &mut pt);
    }
    let wa = work_area();
    let gap = logical_px(panel, GAP_ABOVE_DOCK);

    let x = (pt.x - w / 2).clamp(wa.left + 4, (wa.right - w - 4).max(wa.left + 4));
    let mut y = dr.top - h - gap;
    if y < wa.top + 4 {
        // 上方放不下（Dock/面板被调得很高）：改放到下方，仍钳在屏内
        y = (dr.bottom + gap).min((wa.bottom - h - 4).max(wa.top + 4));
    }
    (x, y, x + w, y + h)
}

/// 背景偏暗 → 文字用浅色。
///
/// 阈值 0.18 是**算出来的**，不是拍的：白字与背景的对比度 = 1.05 / (L + 0.05)，
/// 要 ≥ 4.5（WCAG AA 正文）就得 L ≤ 0.183。反过来背景更亮时用黑字，
/// 在同一分界上对比度约为 (L + 0.05) / 0.05 ≥ 4.6。所以两侧在这一刀上都合格。
fn is_dark(rgb: [u8; 3]) -> bool {
    let lin = |c: u8| {
        let v = c as f64 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let l = 0.2126 * lin(rgb[0]) + 0.7152 * lin(rgb[1]) + 0.0722 * lin(rgb[2]);
    l < 0.18
}

/// 把颜色朝白色提亮 `t`（0 = 原色，1 = 纯白）。逐通道线性插值。
///
/// 只给悬停标签用（见 `LABEL_BRIGHTEN`）。做的是"提亮"而不是"加白"，
/// 所以纯色（R=G=B）提亮后仍是纯色，不会偏色。
fn brighten(rgb: [u8; 3], t: f64) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let f = |c: u8| (c as f64 + (255.0 - c as f64) * t).round() as u8;
    [f(rgb[0]), f(rgb[1]), f(rgb[2])]
}

// ---------------------------------------------------------------- 隐藏

impl Slot {
    /// 关闭**这一层**。**幂等**，可以从任意线程、任意次数调用。
    pub fn hide(&self) {
        // 先作废待显示与当前状态：这样页面晚到的 `panel_ready` / `panel_state`
        // 都会拿到「已经关了」，不会把浮层又显示出来
        *self.pending.lock().unwrap() = None;
        *self.cur.lock().unwrap() = None;

        if self.visible.swap(false, Ordering::SeqCst) {
            let h = self.hwnd();
            if !h.0.is_null() {
                unsafe {
                    // 直接用 Win32 隐藏：本函数会被看门线程调用，
                    // 而 ShowWindow 对窗口所属线程之外的调用是安全的（会转投递）
                    let _ = ShowWindow(h, SW_HIDE);
                }
            }
            crate::reveal::unpause();
        }
        // 无论之前是否显示着，都要让看门线程退役：
        // 否则「关掉后 16ms 内又打开」会让新旧两个看门线程同时跑
        self.gen.fetch_add(1, Ordering::SeqCst);
    }

    /// 页面读取当前浮层内容。返回 `None` 表示浮层已经关了（页面应清空）。
    pub fn payload(&self) -> Option<PanelPayload> {
        self.cur.lock().unwrap().as_ref().map(|o| o.payload.clone())
    }

    /// 当前浮层对应的**条目 id**（自检与日志用）：菜单是那个应用，文件夹是那个文件夹。
    pub fn current_target_id(&self) -> Option<String> {
        self.cur.lock().unwrap().as_ref().map(|o| o.target_id.clone())
    }

    /// 当前浮层渲染的菜单项（仅菜单形态；自检用）。
    pub fn current_menu_items(&self) -> Option<Vec<MenuItem>> {
        self.cur.lock().unwrap().as_ref().and_then(|o| match &o.payload.content {
            PanelContent::Menu { items, .. } => Some(items.clone()),
            _ => None,
        })
    }

    /// 当前浮层渲染的文件夹条目（仅文件夹形态；自检用）
    pub fn current_folder_items(&self) -> Option<Vec<FolderItem>> {
        self.cur.lock().unwrap().as_ref().and_then(|o| match &o.payload.content {
            PanelContent::Folder { items, .. } => Some(items.clone()),
            _ => None,
        })
    }
}

/// 关掉**所有**浮层（Dock 页面上任何一次按下都调它：那一下是给 Dock 的）。
pub fn hide_all() {
    // 从上往下关，顺序和用户的直觉一致（先菜单、后文件夹）
    MENU.hide();
    FOLDER.hide();
}

// ---------------------------------------------------------------- 给命令层 / 自检的门面

/// 菜单层是否显示着（自检断言"右键菜单打开了"用的是它）
#[allow(dead_code)]
pub fn is_visible() -> bool {
    MENU.is_visible()
}

/// 文件夹层是否显示着
#[allow(dead_code)]
pub fn folder_visible() -> bool {
    FOLDER.is_visible()
}

/// 菜单层当前显示的是不是**悬停标签**（自检断言"名字气泡弹出来了"用的是它）。
///
/// 标签和菜单共用菜单层窗口，所以"窗口可见"不足以说明是标签 —— 必须看内容形态。
#[allow(dead_code)]
pub fn label_visible() -> bool {
    MENU.is_visible()
        && MENU
            .cur
            .lock()
            .unwrap()
            .as_ref()
            .map(|o| matches!(o.payload.content, PanelContent::Label { .. }))
            .unwrap_or(false)
}

/// 菜单层当前对应的条目 id
#[allow(dead_code)]
pub fn current_target_id() -> Option<String> {
    MENU.current_target_id()
}

/// 菜单层当前渲染的菜单项
#[allow(dead_code)]
pub fn current_menu_items() -> Option<Vec<MenuItem>> {
    MENU.current_menu_items()
}

/// 文件夹层当前渲染的条目
#[allow(dead_code)]
pub fn current_folder_items() -> Option<Vec<FolderItem>> {
    FOLDER.current_folder_items()
}

/// 文件夹层当前对应的文件夹 id（`commands::refresh_open_folder` 用）
#[allow(dead_code)]
pub fn current_folder_id() -> Option<String> {
    FOLDER.current_target_id()
}

/// 看门线程是否已经开着。
///
/// ❗**整个栈只能有一个看门线程。** 两层各起一个会互相偷事件：
/// `GetAsyncKeyState` 的 0x0001 位（"上次查询之后按过"）是**一次性的**，
/// 谁先查谁拿到 —— 而合成点击（按下+抬起短于一帧）只会被其中一个线程看到。
/// 如果是"没轮到关"的那一层拿走了，这次点击就凭空消失
/// （本轮实测症状：点外面关不掉菜单，再点一次却把菜单关了）。
static WATCHDOG: AtomicBool = AtomicBool::new(false);

/// 确保看门线程在跑（浮层显示时调用；已经开着就什么都不做）。
///
/// # 逐层关闭的规矩（用户明确要求过）
///
/// | 动作 | 结果 |
/// |---|---|
/// | Esc | 关菜单；菜单没了再关文件夹 |
/// | 点菜单外、但不是文件夹 | 关菜单 —— **文件夹留着** |
/// | 再点一次外面 | 这时菜单没了，关文件夹 |
///
/// 实现就是每个 tick 取一次 `top_slot()`，触发时关**它**。
/// 因为每 tick 都重新取，层数变化（菜单关了）下一 tick 就自动轮到文件夹。
fn ensure_watchdog(app: &AppHandle) {
    if WATCHDOG.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        // 上一帧的按键状态。要的是**边沿**（刚按下）而不是电平 ——
        // 否则「按住左键从浮层上划过去」也会被当成一次点击
        let (mut last_l, mut last_r, mut last_esc) = (false, false, false);
        loop {
            std::thread::sleep(Duration::from_millis(16));

            // 没有浮层了 → 退役。
            //
            // ⚠️ 先清标志**再复查一次**：`ensure_watchdog` 可能正好卡在
            // "看到标志还是 true 于是没起新线程"的瞬间，而这一刻我们决定退出 ——
            // 复查一次就不会漏（漏了的后果是浮层再也关不掉）。
            let Some(top) = top_slot() else {
                WATCHDOG.store(false, Ordering::SeqCst);
                if top_slot().is_some() {
                    continue;
                }
                return;
            };

            let (l, r, esc) = read_buttons();
            if esc.fresh(last_esc) {
                log_info!("[浮层] Esc → 关闭最上面那层（{}）", top.label);
                top.hide();
                (last_l, last_r, last_esc) = (l.down, r.down, esc.down);
                continue;
            }
            if l.fresh(last_l) || r.fresh(last_r) {
                let c = cursor();
                // "在里面"要按**所有已显示的浮层**算：菜单开着时点文件夹里的图标
                // 仍然是"在里面"（那一格会去启动应用，由文件夹页面处理），
                // 不该被当成"点外面"。
                let in_any_panel = [&FOLDER, &MENU]
                    .iter()
                    .filter(|s| s.is_visible())
                    .any(|s| {
                        crate::win_layer::point_in(&crate::win_layer::window_rect(s.hwnd()), c)
                    });
                let in_dock = dock_rect(&app)
                    .map(|d| crate::win_layer::point_in(&d, c))
                    .unwrap_or(false);
                // 落在 Dock 上的按下交给 Dock 页面自己处理（可能是右键另一个图标），
                // 两边都管就会互相打架 —— 表现为浮层一闪就没了
                if !in_any_panel && !in_dock {
                    log_info!(
                        "[浮层] 点到别处 ({},{}) → 关闭最上面那层（{}）",
                        c.x, c.y, top.label
                    );
                    top.hide();
                }
            }
            (last_l, last_r, last_esc) = (l.down, r.down, esc.down);
        }
    });
}

fn dock_rect(app: &AppHandle) -> Option<windows::Win32::Foundation::RECT> {
    let w = app.get_webview_window("dock")?;
    let raw = w.hwnd().ok()?;
    if raw.0.is_null() {
        return None;
    }
    Some(crate::win_layer::window_rect(HWND(raw.0 as *mut c_void)))
}

fn cursor() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

/// 一个键/鼠标键的当前状态。
#[derive(Clone, Copy)]
struct KeyState {
    /// 现在按着
    down: bool,
    /// 上一次查询之后**按过**（`GetAsyncKeyState` 的最低位）
    pressed_since: bool,
}

impl KeyState {
    /// 「刚刚按下」——`prev` 是上一次查询到的 `down`。
    ///
    /// ❗为什么要看最低位：合成点击（`SendInput` 的按下 + 抬起）几乎在同一瞬间完成，
    /// 按下的时间可能**短于一帧**（16ms），只比较 `down` 的边沿会整个漏掉这次点击
    /// （自检第一版就漏了：菜单开着，点了别处却关不掉）。
    /// 最低位表示「上次查询之后按过」，由本次查询清零，所以短到 1ms 的按下也能被看到。
    ///
    /// 用最低位的前提是**只有本线程在查**这几个键（否则别的调用方会把这一位读走）。
    /// 本进程里只有这个看门线程查键盘鼠标状态，成立。
    fn fresh(&self, prev_down: bool) -> bool {
        self.pressed_since || (self.down && !prev_down)
    }
}

/// 当前左键 / 右键 / Esc 的状态。
fn read_buttons() -> (KeyState, KeyState, KeyState) {
    unsafe {
        let get = |vk: i32| {
            let s = GetAsyncKeyState(vk) as u16;
            KeyState {
                down: s & 0x8000 != 0,
                pressed_since: s & 0x0001 != 0,
            }
        };
        (
            get(VK_LBUTTON.0 as i32),
            get(VK_RBUTTON.0 as i32),
            get(VK_ESCAPE.0 as i32),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_background_gets_light_text() {
        assert!(is_dark([24, 24, 28]), "默认的石墨色应该用浅色文字");
        assert!(is_dark([0, 0, 0]));
    }

    #[test]
    fn light_background_gets_dark_text() {
        // 用户当前配置的浅灰
        assert!(!is_dark([170, 170, 177]));
        assert!(!is_dark([255, 255, 255]));
    }

    /// **前端读哪些字段名，这里就钉哪些字段名。**
    ///
    /// 为什么必须有这条：`PanelContent` 是 tag 枚举，而枚举上的
    /// `rename_all = "camelCase"` 只改变体名、**不改变体里的字段名**。
    /// 漏了变体级的 `rename_all` 时 Rust 会发 `row_h`，页面读 `rowH` 拿到 undefined ——
    /// 表现是"菜单行高失效、挤成一条"，而 **tsc 和所有既有断言都发现不了**
    /// （`items` 恰好两边同名，所以数据看起来是通的）。本轮真踩了。
    #[test]
    fn payload_field_names_are_camel_case() {
        let menu = PanelContent::Menu {
            items: vec![MenuItem::new("activate", "启动")],
            row_h: ROW_H,
            divider_h: DIVIDER_H,
        };
        let v = serde_json::to_value(&menu).unwrap();

        // 变体名（页面按 `content.kind` 分支）
        assert_eq!(v["kind"], "menu");
        // 变体里的字段：页面读 rowH / dividerH（不是 row_h）
        assert!(v.get("rowH").is_some(), "缺 rowH（页面读的就是这个名字）");
        assert!(v.get("dividerH").is_some(), "缺 dividerH");
        assert!(v.get("row_h").is_none(), "出现了 snake_case 的 row_h —— 变体级 rename_all 漏了");
        // 菜单项自身的字段（MenuItem 上的 rename_all 负责）
        assert!(v["items"][0].get("label").is_some());
        assert!(v["items"][0].get("danger").is_some());

        let folder = PanelContent::Folder {
            title: "常用".into(),
            cols: FOLDER_COLS,
            cell: 52.0,
            gap: FOLDER_GAP,
            items: vec![FolderItem {
                id: "a".into(),
                label: "Edge".into(),
                target: "x".into(),
                running: true,
            }],
        };
        let v2 = serde_json::to_value(&folder).unwrap();
        assert_eq!(v2["kind"], "folder");
        for k in ["title", "cols", "cell", "gap", "items"] {
            assert!(v2.get(k).is_some(), "文件夹形态缺字段 {k}");
        }
        assert!(v2["items"][0].get("running").is_some());

        // 整个 payload 的字段（页面读 seq / content / dark / pad / width / glassRgb）
        let payload = PanelPayload {
            seq: 1,
            content: menu,
            dark: true,
            pad: PAD,
            width: 168.0,
            glass_rgb: [1, 2, 3],
        };
        let v3 = serde_json::to_value(&payload).unwrap();
        for k in ["seq", "content", "dark", "pad", "width", "glassRgb"] {
            assert!(v3.get(k).is_some(), "面板 payload 缺字段 {k}");
        }
    }

    #[test]
    fn threshold_is_wcag_aa_on_both_sides() {
        // 分界点两侧的对比度都必须 ≥ 4.5:1，这一刀才不亏待任何一种背景。
        // 边界值**按定义算出来**，不写死魔数 —— 写死的话改了阈值测试还在"通过"。
        let lin = |c: u8| {
            let v = c as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let contrast = |a: [u8; 3], b_lum: f64| {
            let l = 0.2126 * lin(a[0]) + 0.7152 * lin(a[1]) + 0.0722 * lin(a[2]);
            let (hi, lo) = if l > b_lum { (l, b_lum) } else { (b_lum, l) };
            (hi + 0.05) / (lo + 0.05)
        };
        const WHITE: f64 = 1.0;
        const BLACK: f64 = 0.0;

        // 中灰扫描：找到「第一个被判为亮」的灰阶
        let first_light = (0u16..=255)
            .map(|v| v as u8)
            .find(|&v| !is_dark([v, v, v]))
            .expect("总得有个灰阶被判为亮");

        // 它上面那一级还是「暗」：必须白字够清楚
        let last_dark = first_light - 1;
        assert!(is_dark([last_dark, last_dark, last_dark]));
        assert!(
            contrast([last_dark, last_dark, last_dark], WHITE) >= 4.4,
            "被判为暗的最亮灰 {last_dark} 用白字只有 {:.2}:1",
            contrast([last_dark, last_dark, last_dark], WHITE)
        );
        // 它本身是「亮」：必须黑字够清楚
        assert!(
            contrast([first_light, first_light, first_light], BLACK) >= 4.5,
            "被判为亮的最暗灰 {first_light} 用黑字只有 {:.2}:1",
            contrast([first_light, first_light, first_light], BLACK)
        );
    }

    /// 悬停标签的底色必须**真的变亮**，而且要亮到让 `is_dark` 翻面。
    ///
    /// 这两条缺一不可：
    ///  - 只提亮但没翻面 → 白字压在中灰上，比原来更糊（这正是要防的"改了更糟"）；
    ///  - 没提亮（`brighten` 写错成恒等）→ 用户看到的还是那块发暗的玻璃。
    #[test]
    fn label_tint_is_bright_enough_to_flip_text_color() {
        // 默认着色，也是用户报"有点暗"的那一套
        let dock_tint = [24u8, 24, 28];
        assert!(is_dark(dock_tint), "前提：深色 Dock 着色被判为暗");

        let label = brighten(dock_tint, LABEL_BRIGHTEN);
        assert!(
            !is_dark(label),
            "标签底色 {label:?} 还在深色区 → 页面会用白字，对比不足"
        );
        assert!(
            label[0] > dock_tint[0] + 100,
            "提亮幅度太小：{dock_tint:?} → {label:?}（用户要的是「明显亮一点」）"
        );

        // 提亮到纯白是极限，不能溢出
        assert_eq!(brighten([255, 255, 255], 0.5), [255, 255, 255]);
        assert_eq!(brighten([0, 0, 0], 0.0), [0, 0, 0]);
    }
}
