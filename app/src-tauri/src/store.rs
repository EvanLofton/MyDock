//! 配置持久化。
//!
//! 自己实现而不是用 `tauri-plugin-store`：本机 cargo 缓存里没有该插件（离线装不上），
//! 而需求只是「读写一个 JSON」，用已在依赖里的 `serde_json` 十几行就够，
//! 还少一个依赖。

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Preferences {
    /// 鼠标离开后自动隐藏
    pub auto_hide: bool,
    /// 离开后多久隐藏（毫秒）
    pub hide_delay_ms: u64,
    /// 图标逻辑像素边长
    pub icon_size: u32,
    /// 面板高度（逻辑像素）。
    ///
    /// ⚠️ **窗口高度必须等于它** —— 亚克力是按整个窗口矩形铺的，窗口比面板高就会
    /// 在下方露出一条灰底。所以这个值由前端算好、经 `set_dock_size` 上报，
    /// Rust 只负责把窗口调成同样高（并保持**底边不动**，也就是往上长）。
    ///
    /// 下限由几何决定：至少要装得下「图标 + 底部间距 + 顶部余量」，否则图标会被裁掉。
    /// 前端会兜底钳制（见 `Dock.tsx` 里 `panelH` 的算法）。
    pub panel_height: u32,
    /// 图标之间的间距（逻辑像素）。
    ///
    /// 这是**方格的**间距；实际看起来的间距还要看图形自己在画布里留多少白，
    /// 前端会做光学补偿（见 `lib/ipc.ts::encodeOptical` 与 `Dock.tsx::gapBetween`）。
    /// 所以这个值调大调小是"整体松紧"，而不是逐对微调。
    pub icon_gap: u32,
    /// 鱼眼放大增量（0 = 关闭放大）
    pub magnification: f64,
    /// 显示运行指示点
    pub show_running_indicators: bool,
    /// 给任务栏预留空间（任务栏自动隐藏时也预留，避免弹出时盖住 Dock）
    pub reserve_taskbar: bool,
    /// Dock 底边距「基准底边」多少**逻辑像素** —— 由用户决定，0 = 贴底。
    ///
    /// 基准底边：任务栏常显时是 `rcWork` 的底（已排除任务栏）；任务栏自动隐藏时
    /// 再往上扣掉任务栏高度（否则它一触底弹出来就盖住 Dock）。
    /// 用户想在 Dock 与屏幕底边之间留多少空间，就看这个值。
    pub bottom_offset: u32,
    /// 开机自启
    pub launch_at_login: bool,
    /// 毛玻璃着色（路线 B 的 alpha 与 RGB）
    pub glass_alpha: u8,
    pub glass_rgb: [u8; 3],
    /// **Dock 上有哪些图标、按什么顺序** —— 就这一个列表说了算。
    ///
    /// 出厂时由 `DEFAULT_PINS` 预置几个（见 `seed_default_pins`），之后完全由用户决定：
    /// 从桌面拖进来、「添加应用…」、右键移除。**这个 Vec 的顺序就是图标的显示顺序**，
    /// 将来做 Dock 内拖拽排序时也只动它。
    ///
    /// 名字里的 "pinned" 是历史遗留（早期模型区分「固定的」和「只是正在运行的」两段，
    /// 那个区分已废弃）—— 现在它等价于「Dock 上的应用列表」，没有对应的「未固定」状态。
    /// 文件名/键名保持不变是为了让已有配置能直接读进来。
    pub pinned: Vec<crate::model::PinnedApp>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            // 默认**不**自动隐藏：Dock 是常驻可见的操作入口，一上来就藏起来
            // 会让人以为程序没启动。想自动隐藏可在托盘菜单或设置页里开。
            auto_hide: false,
            hide_delay_ms: 420,
            icon_size: 44,
            // 面板高度 72 = 图标 44 + 底部间距 8 + 顶部余量 2 + 放大余量 18。
            panel_height: 72,
            // 图标间距：10 是"一眼能看出是两个图标，又不散"的值。
            // 上下限由前端钳制（0 会贴到一起，太大就散成两排的感觉）。
            icon_gap: 10,
            // 鱼眼放大增量（0 = 关闭放大）。
            // 上限由几何决定：图标最高只能到「面板高 - 底部间距 8 - 顶部余量 2」，
            // 默认面板 72 → 62，44px 图标 → 最大 62/44 = 1.409 → 增量上限约 0.41。
            // 前端会把配置值收敛到这个上限（面板高度改了上限也跟着变），
            // 所以这里给一个默认面板下不超限的值即可。
            magnification: 0.35,
            show_running_indicators: true,
            reserve_taskbar: true,
            // 默认贴底。想要「浮起来」就在设置页里调。
            bottom_offset: 0,
            launch_at_login: false,
            // 路线 B 的着色 alpha。实测（背后是左黑右白的锐利边缘，量 10%→90% 对比度）：
            //   alpha 220 → 对比度 25   （几乎看不到模糊，像块死板的深色板）
            //   alpha 170 → 对比度 67
            //   alpha 150 → 对比度 ~85  ← 默认值，兼顾「有玻璃感」与「不糊成一片」
            //   alpha 120 → 对比度 111
            //   alpha  70 → 对比度 153 （几乎全透，失去 Dock 的实体感）
            // 注意：这层之上还有前端 `.dock` 的 rgba(24,24,28,0.34)，
            // 两层一起决定最终观感，调的时候要一起调。
            glass_alpha: 150,
            glass_rgb: [24, 24, 28],
            pinned: Vec::new(),
        }
    }
}

/// 首次运行预置的**规则**表（不是路径表）。
///
/// # 为什么是"规则"而不是"路径"（2026-09 为开源改的）
///
/// 老版本这里写的是**完整路径 + 中文名**（`C:\Program Files (x86)\Microsoft\Edge\…`、
/// 在本机 `Get-StartApps` 查出来的 AUMID、以及"终端/记事本/计算器"这些中文常量）。
/// 那是照着开发机写的，别人机器上会：路径不存在（空图标）、英文系统上名字是中文、
/// 没装的应用也冒出来。现在只留三类**可移植**线索，名字与路径全部现查：
///
/// | 规则 | 线索 | 换机器为什么仍然成立 |
/// |---|---|---|
/// | `SystemExe` | `%SystemRoot%\…` | 所有 Windows 都有，且系统盘不一定是 C: |
/// | `Program`  | `App Paths` 里的 **exe 文件名** | 微软安装规范要求登记，值与安装位置无关 |
/// | `Package`  | 打包应用的**包族名前缀** | 发布者哈希由发布者决定，所有机器相同 |
///
/// 每一条都先**验证存在性**（`app_paths_lookup` 内置存在性检查、`target_exists` 现查），
/// 不存在就跳过 —— 所以没装的应用不会留下点不开的空图标。
/// 显示名用 `apps::discover_name` 从系统读（Shell 显示名 / 文件版本信息），
/// 英文系统上自然就是英文名。
enum PinRule {
    /// 系统自带（相对 `%SystemRoot%`）
    SystemExe(&'static str),
    /// Shell 命名空间项（此电脑 / 回收站）：target 原样使用，名字由 Shell 给
    ShellLocation(&'static str),
    /// 走 `App Paths` 查安装位置（exe 文件名，含扩展名）
    Program(&'static str),
    /// 打包应用：`(包族名前缀, AppId)`
    Package(&'static str, &'static str),
}

/// 首屏预置顺序 = Dock 从左到右。设计取向：**先系统、后应用**，桌面/资源管理器/回收站
/// 是"文件"这一类的入口，浏览器与终端是"最常用的两个程序"。
///
/// 刻意**不**预置记事本/计算器/画图/照片那种收件箱小工具 —— 那是用户自己的选择，
/// 一上来塞 8 个图标会让人第一时间就想删。要加走托盘「添加应用…」。
const DEFAULT_PIN_RULES: &[PinRule] = &[
    // ---- 系统位置（名字由 Shell 给，跟随系统语言）----
    PinRule::ShellLocation("shell:MyComputerFolder"),
    PinRule::ShellLocation("shell:RecycleBinFolder"),
    PinRule::SystemExe(r"explorer.exe"),
    // ---- 常用程序（装了就出现，没装就没有）----
    PinRule::Program("msedge.exe"),
    PinRule::Program("chrome.exe"),
    PinRule::Program("firefox.exe"),
    PinRule::Program("wt.exe"),
    PinRule::Package("Microsoft.WindowsTerminal_8wekyb3d8bbwe", "App"),
    PinRule::Package(
        "windows.immersivecontrolpanel_cw5n1h2txyewy",
        "microsoft.windows.immersivecontrolpanel",
    ),
];

/// 配置文件是否还不存在（也就是首次运行）。
pub fn is_first_run(app: &AppHandle) -> bool {
    !config_path(app).exists()
}

/// 首次运行时预置一批常用应用，让 Dock 一启动就能用 —— 否则它是空的
/// （或者只显示正在运行的应用，那样图标位置会随窗口切换乱跳）。
///
/// 规则见 [`DEFAULT_PIN_RULES`]：**路径与名字全部现查**，装了的才出现。
/// 返回实际预置的条数。已经添加过应用（`pinned` 非空）时什么都不做。
pub fn seed_default_pins(app: &AppHandle) -> usize {
    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
    if !p.pinned.is_empty() {
        return 0;
    }

    let mut pins: Vec<crate::model::PinnedApp> = Vec::new();
    // 同一个 exe 可能被两条规则命中（`wt.exe` 与打包版终端），去重靠 id
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for rule in DEFAULT_PIN_RULES {
        // ① 规则 → target（查不到就返回 None，直接跳过这一条）
        let target = match rule {
            PinRule::SystemExe(rel) => crate::apps::system_exe(rel),
            PinRule::ShellLocation(t) => Some((*t).to_string()),
            PinRule::Program(exe) => crate::apps::app_paths_lookup(exe),
            PinRule::Package(pfn, app_id) => Some(crate::apps::package_target(pfn, app_id)),
        };
        let Some(target) = target else { continue };

        // ② 存在性验证（`app_paths_lookup` 自己查过文件，这里覆盖 shell / 打包应用）
        if !crate::apps::target_exists(&target) {
            continue;
        }
        // ②b `WindowsApps\` 下的路径不能当条目：那个目录 ACL 只允许经 AppsFolder 激活，
        //     直接启动会被拒绝（图标画得出来、点了没反应）。实测 `wt.exe` 就落在那里，
        //     而且它和下面的「打包版终端」是同一条 —— 一并被这条挡掉。
        if crate::apps::is_windowsapps_path(&target) {
            continue;
        }
        let id = crate::apps::id_for_target(&target);
        if !seen.insert(id.clone()) {
            continue;
        }
        // ③ 名字从系统读（打包应用/系统位置取 Shell 显示名，exe 取版本信息）
        pins.push(crate::model::PinnedApp {
            id,
            display_name: crate::apps::discover_name(&target),
            target,
            separator: false,
            is_folder: false,
            children: Vec::new(),
        });
    }

    // 系统位置与应用之间插一条分割线（macOS 观感；右键「移除分割线」即可去掉）。
    // 只在"两边都有内容"时插，免得出现孤零零一条线。
    let has_sys = pins
        .iter()
        .any(|x| crate::apps::is_shell_target(&x.target) || x.target.to_lowercase().ends_with("explorer.exe"));
    let has_app = pins.iter().any(|x| !crate::apps::is_shell_target(&x.target)
        && !x.target.to_lowercase().ends_with("explorer.exe"));
    if has_sys && has_app {
        let idx = pins
            .iter()
            .position(|x| {
                !crate::apps::is_shell_target(&x.target)
                    && !x.target.to_lowercase().ends_with("explorer.exe")
            })
            .unwrap_or(pins.len());
        pins.insert(
            idx,
            crate::model::PinnedApp {
                id: next_id(&p, "sep"),
                display_name: String::new(),
                target: String::new(),
                separator: true,
                is_folder: false,
                children: Vec::new(),
            },
        );
    }

    // 统计要减掉自己插进去的分割线，否则"跳过几条"会说谎（实测差一）
    let seeded_apps = pins.iter().filter(|p| !p.separator).count();
    let skipped = DEFAULT_PIN_RULES.len().saturating_sub(seeded_apps);
    if skipped > 0 {
        log_info!("[配置] 预置规则中有 {skipped} 条本机没有对应应用（或不能直接启动），已跳过");
    }
    if pins.is_empty() {
        return 0;
    }
    let n = pins.len();

    let names: Vec<&str> = pins
        .iter()
        .map(|p| if p.separator { "｜分割线" } else { p.display_name.as_str() })
        .collect();
    log_info!("[配置] 预置（全部来自本机探测）: {}", names.join("、"));

    p.pinned = pins;
    // 写盘失败**不阻断**：这一轮 Dock 仍然可用，只是下次启动还得重新预置
    if let Err(e) = save(app, &p) {
        log_error!("[配置] 预置常用应用写盘失败: {e}");
    }
    *app.state::<PrefsState>().0.lock().unwrap() = p;
    n
}

/// 按 `ids` 给出的顺序重排 Dock 上的图标（Dock 内拖拽排序用）。
///
/// # 校验必须严格
///
/// `ids` 必须与现有列表**完全一致**：不重、不漏、不新增。
/// 顺序写入不能"尽力而为" —— 一个错序的列表会让用户的图标**丢失或重复**，
/// 而这类错误一旦落盘就回不去了。所以任何不一致都**直接拒绝**，一个字节都不写。
pub fn reorder(app: &AppHandle, ids: &[String]) -> Result<(), String> {
    let snapshot = app.state::<PrefsState>().0.lock().unwrap().clone();
    let mut p = snapshot.clone();

    if ids.len() != p.pinned.len() {
        return Err(format!(
            "顺序长度不符（收到 {}，实际 {}）",
            ids.len(),
            p.pinned.len()
        ));
    }

    // 先整体取出来建索引，逐项消费；有剩余或找不到就说明集合不一致
    let mut pool: std::collections::HashMap<String, crate::model::PinnedApp> =
        p.pinned.drain(..).map(|x| (x.id.clone(), x)).collect();
    let mut next = Vec::with_capacity(ids.len());
    for id in ids {
        match pool.remove(id) {
            Some(x) => next.push(x),
            None => return Err(format!("顺序里有不存在的项，或重复项: {id}")),
        }
    }
    if !pool.is_empty() {
        return Err(format!("顺序里有 {} 项被漏掉", pool.len()));
    }

    p.pinned = next;
    save(app, &p)?;
    *app.state::<PrefsState>().0.lock().unwrap() = p;
    Ok(())
}

/// 从 Dock 上移除一个图标。
///
/// （实现在下面 `remove` 里 —— 顶层和文件夹里都会找。这里保留 `unpin` 作为
/// 「只从顶层移除」的底层操作，`reorder` 之类的内部逻辑还用得到。）
pub fn unpin_top(app: &AppHandle, id: &str) -> Result<(), String> {
    unpin(app, id)
}

/// 把一个条目（应用 / 系统位置）变成**文件夹**，它自己成为文件夹里的第一个。
///
/// 「新建文件夹」的**带命名**入口：弹输入框 → 建文件夹。
///
/// 为什么是 `spawn` 而不是直接做：输入框是**模态**的（自带消息循环），
/// 而调用方在主线程（Tauri 事件循环）上 —— 直接调用会把整个 Dock 冻住。
/// 和 `picker.rs` 的文件对话框同一个套路：扔到工作线程，顺便把自动隐藏暂停掉
/// （否则用户正在打字，Dock 自己收下去了）。
pub fn spawn_create_folder(app: &AppHandle, app_id: &str) {
    let app = app.clone();
    let app_id = app_id.to_string();
    crate::reveal::pause();
    std::thread::spawn(move || {
        let name = crate::prompt_window::ask(
            &app,
            "新建文件夹",
            "给这个文件夹起个名字",
            DEFAULT_FOLDER_NAME,
            MAX_FOLDER_NAME_CHARS,
        );
        match name {
            // 取消：什么都不做（用户改主意了）
            None => log_info!("[配置] 新建文件夹：用户取消"),
            Some(n) => match create_folder(&app, &app_id, Some(&n)) {
                Ok(_) => crate::drop::refresh_apps(&app),
                Err(e) => log_error!("[配置] 新建文件夹失败: {e}"),
            },
        }
        crate::reveal::unpause();
    });
}

/// 给文件夹改名字（右键「重命名…」）。返回新名字。
///
/// 规则与新建时一致：去空白、空名字**拒绝**（改名时留空没有意义 —— 那只会得到一个
/// 看不见名字的文件夹；新建时留空是退回默认名，这里没有"默认"可退）、超长截断。
pub fn rename_folder(app: &AppHandle, folder_id: &str, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("名字不能为空".into());
    }
    let new_name: String = name.chars().take(MAX_FOLDER_NAME_CHARS).collect();

    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
    let idx = p
        .pinned
        .iter()
        .position(|x| x.id == folder_id)
        .ok_or_else(|| format!("Dock 上没有这个条目: {folder_id}"))?;
    if !p.pinned[idx].is_folder {
        return Err("它不是文件夹".into());
    }
    let old = std::mem::replace(&mut p.pinned[idx].display_name, new_name.clone());
    save(app, &p)?;
    *app.state::<PrefsState>().0.lock().unwrap() = p;
    log_info!("[配置] 文件夹 {folder_id} 改名：「{old}」→「{new_name}」");
    Ok(new_name)
}

/// 「重命名…」的带输入框入口（和工作线程那套说明见 `spawn_create_folder`）。
pub fn spawn_rename_folder(app: &AppHandle, folder_id: &str) {
    let app = app.clone();
    let folder_id = folder_id.to_string();
    // 默认值 = 它现在的名字（用户多半只想改几个字，全选状态直接打字就替换掉）
    let current = app
        .state::<PrefsState>()
        .0
        .lock()
        .unwrap()
        .pinned
        .iter()
        .find(|p| p.id == folder_id)
        .map(|p| p.display_name.clone())
        .unwrap_or_else(|| DEFAULT_FOLDER_NAME.to_string());

    crate::reveal::pause();
    std::thread::spawn(move || {
        match crate::prompt_window::ask(
            &app,
            "重命名文件夹",
            "改成什么名字",
            &current,
            MAX_FOLDER_NAME_CHARS,
        ) {
            None => log_info!("[配置] 重命名：用户取消"),
            Some(n) => match rename_folder(&app, &folder_id, &n) {
                Ok(_) => crate::drop::refresh_apps(&app),
                Err(e) => log_error!("[配置] 重命名失败: {e}"),
            },
        }
        crate::reveal::unpause();
    });
}

/// 「清空回收站」：同样挪到工作线程 —— 系统那个确认框是模态的。
pub fn spawn_empty_recycle_bin(app: &AppHandle) {
    let app = app.clone();
    crate::reveal::pause();
    std::thread::spawn(move || {
        match crate::apps::empty_recycle_bin() {
            Ok(()) => {
                log_info!("[回收站] 已清空");
                // 图标要换成"空"那一张 —— 前端按"里面有几项"当版本号（BL-8），
                // 这里推一把让它立刻重取，不用等下一次轮询。
                crate::drop::refresh_apps(&app);
            }
            Err(e) => {
                log_error!("[回收站] {e}");
                crate::drop::toast(&app, &e);
            }
        }
        crate::reveal::unpause();
    });
}

/// 把 Dock 的**临时文件夹**放到 Dock 最左侧（不存在就建出来）。
///
/// 为什么是一个"真实目录"而不是 `shell:` 命名空间项：它就是给用户放临时文件用的，
/// 点开要在资源管理器里能直接拖东西进去。路径见 `apps::temp_folder_path`。
/// 重复添加会被拒绝（它已经在 Dock 上了）。
pub fn add_temp_folder(app: &AppHandle) -> Result<String, String> {
    let dir = crate::apps::temp_folder_path()?;
    // 先建出来：图标点开时目录必须存在，否则资源管理器会弹"找不到"
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
    let target = dir.display().to_string();
    add_pinned_at_left(app, &target, TEMP_FOLDER_NAME)
}

/// 新建文件夹时的默认名字（用户没填 / 填了空白时用它）
pub const DEFAULT_FOLDER_NAME: &str = "新建文件夹";
/// Dock 上「临时文件夹」的显示名
pub const TEMP_FOLDER_NAME: &str = "临时文件夹";
/// 文件夹名字最长多少字符（输入框与存储两侧都夹一道，见 `create_folder`）
pub const MAX_FOLDER_NAME_CHARS: usize = 40;

/// 位置不变：文件夹插在原来那条的位置上 —— 用户是"把这个图标变成文件夹"，
/// 不是"在别处新建一个"。
///
/// `name` = 用户在弹出的输入框里填的名字（`None` / 空白 → 用默认的「新建文件夹」）。
pub fn create_folder(app: &AppHandle, app_id: &str, name: Option<&str>) -> Result<String, String> {
    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
    let idx = p
        .pinned
        .iter()
        .position(|x| x.id == app_id)
        .ok_or_else(|| format!("Dock 上没有这个条目: {app_id}"))?;
    if p.pinned[idx].is_folder {
        return Err("它已经是文件夹了".into());
    }
    if p.pinned[idx].separator {
        return Err("分割线不能变成文件夹".into());
    }

    // 名字：用户填的优先；空白/没填就退回默认。长度由输入框自己限死，
    // 但这里再夹一道 —— 将来别的入口（比如重命名）也要走同一个规则。
    let display_name = name
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(MAX_FOLDER_NAME_CHARS).collect::<String>())
        .unwrap_or_else(|| DEFAULT_FOLDER_NAME.to_string());

    let child = p.pinned.remove(idx);
    let id = next_id(&p, "folder");
    p.pinned.insert(
        idx,
        crate::model::PinnedApp {
            id: id.clone(),
            display_name: display_name.clone(),
            target: String::new(),
            separator: false,
            is_folder: true,
            children: vec![child],
        },
    );
    save(app, &p)?;
    *app.state::<PrefsState>().0.lock().unwrap() = p;
    log_info!("[配置] 已新建文件夹 {id}「{display_name}」（含 1 项）");
    Ok(id)
}

/// 把一个顶层条目**放进文件夹**。
///
/// 这是"拖图标到文件夹上"的落点。会从顶层移除、追加到文件夹末尾 ——
/// 于是它的位置由文件夹决定，文件夹本身的位置不动。
pub fn add_to_folder(app: &AppHandle, folder_id: &str, app_id: &str) -> Result<(), String> {
    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
    let fi = p
        .pinned
        .iter()
        .position(|x| x.id == folder_id)
        .ok_or_else(|| format!("找不到文件夹: {folder_id}"))?;
    if !p.pinned[fi].is_folder {
        return Err("目标不是文件夹".into());
    }
    if p.pinned[fi].children.iter().any(|c| c.id == app_id) {
        return Err("它已经在这个文件夹里了".into());
    }
    let ci = p
        .pinned
        .iter()
        .position(|x| x.id == app_id)
        .ok_or_else(|| format!("Dock 上没有这个条目: {app_id}"))?;
    if p.pinned[ci].separator {
        return Err("分割线不能放进文件夹".into());
    }
    if p.pinned[ci].is_folder {
        // 嵌套文件夹会让"点开→再点开"变成一条没有尽头的路径，收益为零
        return Err("文件夹不能放进文件夹".into());
    }
    if ci == fi {
        return Err("不能放进自己".into());
    }

    let child = p.pinned.remove(ci);
    // 移除前面的元素之后，文件夹的下标可能要往前挪一格
    let fi = if ci < fi { fi - 1 } else { fi };
    p.pinned[fi].children.push(child);
    save(app, &p)?;
    *app.state::<PrefsState>().0.lock().unwrap() = p;
    log_info!("[配置] {app_id} 已放进文件夹 {folder_id}");
    Ok(())
}

/// **解散文件夹**：里面的条目回到顶层，放在文件夹原来的位置上。
///
/// 这是"我不想用它了，但里面的东西还要"的出口 —— 没有它，用户只能一个个
/// 拖出来（而拖拽目前只在 Dock 里有效，见 README 的已知限制）。
pub fn dissolve_folder(app: &AppHandle, folder_id: &str) -> Result<usize, String> {
    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
    let fi = p
        .pinned
        .iter()
        .position(|x| x.id == folder_id)
        .ok_or_else(|| format!("找不到文件夹: {folder_id}"))?;
    if !p.pinned[fi].is_folder {
        return Err("它不是文件夹".into());
    }
    let folder = p.pinned.remove(fi);
    let n = folder.children.len();
    for (k, c) in folder.children.into_iter().enumerate() {
        p.pinned.insert(fi + k, c);
    }
    save(app, &p)?;
    *app.state::<PrefsState>().0.lock().unwrap() = p;
    log_info!("[配置] 已解散文件夹 {folder_id}（放出 {n} 项）");
    Ok(n)
}

/// **把文件夹里的一个条目移出来**，放到该文件夹**后面**的顶层位置。
///
/// 为什么不复用 `remove`：用户看到的菜单项写的是「移出文件夹」——
/// 那读起来是"挪到外面去"，不是"删掉"。删掉是另一件事（顶层才有「移出 Dock」）。
/// 放到文件夹**后面**（而不是末尾）是为了让用户看得见它去哪了 —— 一长排图标的另一头
/// 等于没有反馈。
///
/// **空文件夹顺手解散**（BL-17）：移出最后一项之后，文件夹就只剩一个壳 ——
/// 点它只会得到一句"这个文件夹是空的"。所以这一项直接**占住文件夹原来的位置**
/// （而不是它后面），用户看到的是"文件夹变成了那个图标"，正合直觉。
pub fn move_out_of_folder(app: &AppHandle, child_id: &str) -> Result<(), String> {
    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
    let dissolved = move_out_of_folder_in(&mut p.pinned, child_id)?;
    save(app, &p)?;
    *app.state::<PrefsState>().0.lock().unwrap() = p;
    if dissolved {
        log_info!("[配置] {child_id} 是文件夹里最后一项 → 文件夹已解散，该项原地接替");
    } else {
        log_info!("[配置] {child_id} 已移出文件夹，放到顶层");
    }
    Ok(())
}

/// `move_out_of_folder` 的**纯逻辑**部分（不碰 AppHandle，便于单测）。
///
/// 返回 `true` 表示"移出之后文件夹空了，于是顺手解散"。
/// 抽出来是因为它有两条分支（还有 / 解散）和三个前置校验（在不在文件夹里、
/// 顶层有没有同 id），这些正是最容易写错的地方 —— 而它们**不需要**一个 AppHandle 就能测。
fn move_out_of_folder_in(
    pinned: &mut Vec<crate::model::PinnedApp>,
    child_id: &str,
) -> Result<bool, String> {
    let Some(fi) = pinned
        .iter()
        .position(|f| f.is_folder && f.children.iter().any(|c| c.id == child_id))
    else {
        return Err("它不在任何文件夹里".into());
    };
    let Some(ci) = pinned[fi].children.iter().position(|c| c.id == child_id) else {
        return Err("它不在任何文件夹里".into());
    };
    // 顶层已经有同 id 的条目就不再放：会出现两个同名图标，而且"移除"只删得掉一个
    if pinned.iter().any(|x| x.id == child_id) {
        return Err("Dock 上已经有它了".into());
    }
    let child = pinned[fi].children.remove(ci);
    if pinned[fi].children.is_empty() {
        // 最后一项：把文件夹整个换掉（原地接替它的位置），不留空壳
        pinned.remove(fi);
        pinned.insert(fi, child);
        return Ok(true);
    }
    pinned.insert(fi + 1, child);
    Ok(false)
}

/// 从 Dock 上移除一个条目 —— **顶层和文件夹里都找**。
///
/// 为什么合并成一个：用户眼里的"移除"不分层级。
/// 分成两个函数就意味着每个调用方都要知道"这条在不在文件夹里"，那是一定会漏的。
///
/// ⚠️ 对文件夹里的条目来说这是**彻底删掉**（连文件夹里的位置一起没）。
/// 想只是"挪出来"请用 `move_out_of_folder`。
pub fn remove(app: &AppHandle, id: &str) -> Result<(), String> {
    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
    let before = p.pinned.len();
    p.pinned.retain(|x| x.id != id);
    let mut hit = p.pinned.len() != before;
    for f in p.pinned.iter_mut().filter(|x| x.is_folder) {
        let n = f.children.len();
        f.children.retain(|c| c.id != id);
        hit |= f.children.len() != n;
    }
    if hit {
        save(app, &p)?;
        *app.state::<PrefsState>().0.lock().unwrap() = p;
    }
    Ok(())
}

/// 把一个**系统位置**（此电脑 / 回收站…）放到 Dock 的**最左侧**。
///
/// 为什么放最左：这类条目是"位置"而不是"应用"，放在一排应用的最前面最好找。
/// 放进去之后它就是**普通条目** —— 可以拖动排序、拖出删除、右键移除，
/// 「最左」只是加入时的落点，不是被固定住的区域（与「位置完全由用户决定」这条模型一致）。
///
/// 已经在列表里时**移到最左**而不是报错：菜单项承诺的就是"放到最左侧"，
/// 这样点两次的结果一致，用户随时可以再把它拖回去。
pub fn add_system_location(app: &AppHandle, target: &str) -> Result<String, String> {
    if !crate::apps::is_shell_location(target) {
        return Err(format!("不是系统位置: {target}"));
    }
    let id = crate::apps::id_for_target(target);
    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();

    match p.pinned.iter().position(|x| x.id == id) {
        Some(i) => {
            let e = p.pinned.remove(i);
            p.pinned.insert(0, e);
        }
        None => {
            // 名字问 Shell 要（中文系统上就是「此电脑」），查不到才用兜底文案
            let name = crate::apps::shell_display_name(target)
                .unwrap_or_else(|| crate::apps::fallback_name_for(target));
            p.pinned.insert(
                0,
                crate::model::PinnedApp {
                    id: id.clone(),
                    display_name: name,
                    target: target.to_string(),
                    separator: false,
                    is_folder: false,
                    children: Vec::new(),
                },
            );
        }
    }

    save(app, &p)?;
    *app.state::<PrefsState>().0.lock().unwrap() = p;
    log_info!("[配置] 系统位置 {target} 已放到最左侧");
    Ok(id)
}

/// 把一个**普通路径**放到 Dock 最左侧（临时文件夹就是这么一个）。
///
/// 与 `add_system_location` 的区别：那个认的是 `shell:` 命名空间项（名字问 Shell 要），
/// 这里是一个真实目录，名字是我们给的。已经在列表里就**挪到最左**（幂等），
/// 而不是报错 —— 用户的动作是"把它放最左边"，重来一次应该得到同样的结果。
fn add_pinned_at_left(app: &AppHandle, target: &str, display_name: &str) -> Result<String, String> {
    let id = crate::apps::id_for_target(target);
    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
    match p.pinned.iter().position(|x| x.id == id) {
        Some(i) => {
            let e = p.pinned.remove(i);
            p.pinned.insert(0, e);
        }
        None => p.pinned.insert(
            0,
            crate::model::PinnedApp {
                id: id.clone(),
                display_name: display_name.to_string(),
                target: target.to_string(),
                separator: false,
                is_folder: false,
                children: Vec::new(),
            },
        ),
    }
    save(app, &p)?;
    *app.state::<PrefsState>().0.lock().unwrap() = p;
    log_info!("[配置] {display_name}（{target}）已放到最左侧");
    Ok(id)
}

/// 加一条**分割线**（纯视觉分组）。
///
/// `after` = 插在哪个条目后面；`None` 或找不到该条目时**追加到末尾**。
///
/// 为什么找不到锚点也照样加：用户的动作是「添加分割线」，锚点只是个位置提示。
/// 这里唯一不能接受的结果是「点了没反应」—— 用户没法判断是程序坏了还是自己点错了。
///
/// 返回新条目的 id（前端只是拿去做日志/自检，界面刷新走 `list_apps`）。
pub fn add_separator(app: &AppHandle, after: Option<&str>) -> Result<String, String> {
    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
    let id = next_id(&p, "sep");
    let entry = crate::model::PinnedApp {
        id: id.clone(),
        // 分割线没有名字、没有启动目标 —— 它就是列表里的一个普通位置
        display_name: String::new(),
        target: String::new(),
        separator: true,
        is_folder: false,
        children: Vec::new(),
    };
    match after.and_then(|a| p.pinned.iter().position(|x| x.id == a)) {
        Some(i) => p.pinned.insert(i + 1, entry),
        None => p.pinned.push(entry),
    }
    save(app, &p)?;
    *app.state::<PrefsState>().0.lock().unwrap() = p;
    log_info!("[配置] 已添加分割线 {id}");
    Ok(id)
}

/// 生成一个当前配置里不存在的 id。
///
/// 用「毫秒时间戳 + 序号」而不是自增整数：自增需要持久化计数器（多一个字段、
/// 多一处可能不一致的状态），而时间戳天然单调、又不可能和应用 id 撞
/// （应用 id 是 exe 全路径小写或 `uwp:<AUMID>`，都不以 `sep-` / `folder-` 开头）。
fn next_id(p: &Preferences, prefix: &str) -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    for n in 0..1000u32 {
        let id = format!("{prefix}-{ms}-{n}");
        // 顶层和文件夹里都要查重（同一个 id 出现在两处会让"移除"删错东西）
        let taken = p.pinned.iter().any(|x| x.id == id)
            || p.pinned
                .iter()
                .any(|x| x.children.iter().any(|c| c.id == id));
        if !taken {
            return id;
        }
    }
    format!("{prefix}-{ms}-x")
}

// `set_discovered()` 已随「两段式模型」一起删除 —— 见 `apps.rs::enumerate_dock_apps`
// 的说明：Dock 上显示什么、按什么顺序，只由 `pinned` 决定，运行状态不参与。

/// 固定一个应用到 Dock。重复固定会被忽略。
pub fn pin(app: &AppHandle, entry: crate::model::PinnedApp) -> Result<(), String> {
    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
    if p.pinned.iter().any(|x| x.id == entry.id) {
        return Ok(());
    }
    p.pinned.push(entry);
    save(app, &p)?;
    *app.state::<PrefsState>().0.lock().unwrap() = p;
    Ok(())
}

/// 从 Dock 移除一个固定项
pub fn unpin(app: &AppHandle, id: &str) -> Result<(), String> {
    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
    let before = p.pinned.len();
    p.pinned.retain(|x| x.id != id);
    if p.pinned.len() != before {
        save(app, &p)?;
        *app.state::<PrefsState>().0.lock().unwrap() = p;
    }
    Ok(())
}

// `is_pinned()` 已删除：新模型下「在不在 Dock 上」就是「在不在 `pinned` 里」，
// 而唯一需要问这个问题的场景（右键菜单显示「固定」还是「移除」）已经不存在了 ——
// 能右键到的图标必然已经在列表里，菜单只有「从 Dock 移除」。

#[derive(Default)]
pub struct PrefsState(pub Mutex<Preferences>);

/// 配置文件路径。
///
/// 正常情况是 `%APPDATA%\{identifier}\config.json`；
/// 用 `DOCK_CONFIG_DIR` 可重定向（受限沙箱环境下写 AppData 会被拒）。
pub fn config_path(app: &AppHandle) -> PathBuf {
    if let Ok(dir) = std::env::var("DOCK_CONFIG_DIR") {
        return PathBuf::from(dir).join("config.json");
    }
    app.path()
        .app_config_dir()
        .map(|d| d.join("config.json"))
        .unwrap_or_else(|_| PathBuf::from("dock-config.json"))
}

/// 配置文件**所在目录**（托盘「打开配置目录」用；与 [`config_path`] 同一套规则）。
pub fn config_dir(app: &AppHandle) -> PathBuf {
    if let Ok(dir) = std::env::var("DOCK_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// 设置开机自启（**唯一入口**）：写注册表 + 落配置 + 同步托盘勾选。
///
/// 为什么收敛成一个函数：托盘菜单和设置页都能改这项，两边各写一遍必然出现
/// "界面显示已开启、注册表里其实没有"这类不一致 —— 实测托盘的旧代码在失败时
/// 只调了 `set_visible(true)`（对勾选状态毫无作用），界面就撒谎了。
pub fn set_launch_at_login(app: &AppHandle, want: bool) -> Result<(), String> {
    if want {
        let exe = crate::autostart::current_exe().ok_or_else(|| "无法取得自身路径".to_string())?;
        crate::autostart::enable(&exe)?;
    } else {
        crate::autostart::disable()?;
    }

    let mut p = app.state::<PrefsState>().0.lock().unwrap().clone();
    p.launch_at_login = want;
    save(app, &p)?;
    *app.state::<PrefsState>().0.lock().unwrap() = p;

    crate::tray::sync_check(app, "autostart", want);
    log_info!("[配置] 开机自启 -> {want}");
    Ok(())
}

/// 旧 identifier 的**一次性数据迁移**（2026-09 改名 `dev.local.dock` → 现在这个时加的）。
///
/// 改名会同时挪动三个位置，缺一个老用户就会觉得"东西丢了"：
///   1. 配置：`%APPDATA%\dev.local.dock\config.json` → `%APPDATA%\<新 id>\config.json`
///      —— 不迁的话升级上来是**空 Dock**；
///   2. 临时文件夹：`%LOCALAPPDATA%\dev.local.dock\临时文件` → `%LOCALAPPDATA%\<新 id>\临时文件`
///      —— 里面可能有用户放的文件，**复制**过去（不是 move：老实例可能还在跑，
///      突然把目录搬走会让它点开一个不存在的路径）；
///   3. 日志：**不迁**。老日志留在老目录里，新日志写新目录（路径写在日志会话头里，
///      出事时看托盘「打开日志目录」给的那个位置就行）。
///
/// 只在"新位置还没有、旧位置有"时动手 —— 幂等，重复启动无副作用。
/// `DOCK_CONFIG_DIR` 存在时整体跳过：那是自检/沙箱，跑在配置副本上，
/// 不该动用户的真实数据。
pub fn migrate_old_identifier(app: &AppHandle) {
    if std::env::var("DOCK_CONFIG_DIR").is_ok() {
        return;
    }

    // ① 配置
    let new_cfg = config_path(app);
    if !new_cfg.exists() {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let old = PathBuf::from(&appdata)
                .join(crate::OLD_IDENTIFIER)
                .join("config.json");
            if old.exists() {
                if let Some(dir) = new_cfg.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match std::fs::copy(&old, &new_cfg) {
                    Ok(_) => log_info!(
                        "[迁移] 旧标识符的配置已搬到新位置：{} → {}",
                        old.display(),
                        new_cfg.display()
                    ),
                    Err(e) => log_warn!("[迁移] 复制旧配置失败（照常使用默认值）: {e}"),
                }
            }
        }
    }

    // ② 配置里**指向老目录的路径**要改到新位置。
    //
    // ❗这一步必须独立于上面的"复制"、每次都跑（幂等）：因为老路径可能已经
    // 通过一次迁移进了新配置文件里。实测踩到的后果：配置里那条「临时文件夹」
    // 还指着 `…\dev.local.dock\临时文件`，而 `add_temp_folder` 按 **target**
    // 判断"在不在" → 找不到 → 又加了一条 → Dock 上出现两个临时文件夹
    //（自检的「测试结束后列表已还原」就是这么红的：29 项变 30 项）。
    if let Ok(la) = std::env::var("LOCALAPPDATA") {
        rewrite_old_paths(
            &config_path(app),
            &PathBuf::from(&la).join(crate::OLD_IDENTIFIER),
            &PathBuf::from(&la).join(crate::IDENTIFIER),
        );
    }

    // ③ 临时文件夹（复制，不搬走）
    if let Ok(new_tmp) = crate::apps::temp_folder_path() {
        if !new_tmp.exists() {
            if let Ok(la) = std::env::var("LOCALAPPDATA") {
                let old_tmp = PathBuf::from(&la).join(crate::OLD_IDENTIFIER).join("临时文件");
                if old_tmp.is_dir() {
                    match copy_dir(&old_tmp, &new_tmp) {
                        Ok(n) => log_info!(
                            "[迁移] 临时文件夹已复制到新位置（{n} 个文件）：{}",
                            new_tmp.display()
                        ),
                        Err(e) => log_warn!("[迁移] 临时文件夹复制失败（老目录原样留着）: {e}"),
                    }
                }
            }
        }
    }
}

/// 把配置里所有以 `old_prefix` 开头的 `target` 改成 `new_prefix`（含文件夹里的子项）。
///
/// 走 JSON 解析而不是字符串替换：配置文件里的路径是**转义过**的
/// （`C:\\Users\\…`），直接对原文做 replace 一定匹配不上。
fn rewrite_old_paths(cfg: &Path, old_prefix: &Path, new_prefix: &Path) {
    let Ok(text) = std::fs::read_to_string(cfg) else {
        return;
    };
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let old_s = old_prefix.to_string_lossy().to_string();
    let new_s = new_prefix.to_string_lossy().to_string();
    let mut changed = 0u32;
    if let Some(pinned) = v.get_mut("pinned").and_then(|p| p.as_array_mut()) {
        fix_targets(pinned, &old_s, &new_s, &mut changed);
    }
    if changed == 0 {
        return;
    }
    match serde_json::to_string_pretty(&v) {
        Ok(out) => match std::fs::write(cfg, out) {
            Ok(_) => log_info!(
                "[迁移] 配置里 {changed} 条指向老目录的路径已改到新位置（{}）",
                new_prefix.display()
            ),
            Err(e) => log_warn!("[迁移] 写回配置失败: {e}"),
        },
        Err(e) => log_warn!("[迁移] 配置序列化失败: {e}"),
    }
}

fn fix_targets(items: &mut [serde_json::Value], old: &str, new: &str, n: &mut u32) {
    let old_lower = old.to_lowercase();
    for item in items.iter_mut() {
        let target = item
            .get("target")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        let id = item
            .get("id")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();

        if target.starts_with(old) {
            // ① target 指着老目录 → 改到新目录，并按新 target 重算 id
            let replaced = target.replacen(old, new, 1);
            item["id"] = serde_json::Value::String(crate::apps::id_for_target(&replaced));
            item["target"] = serde_json::Value::String(replaced);
            *n += 1;
        } else if id.starts_with(&old_lower) && !target.is_empty() {
            // ② 补漏：早期版本只改了 target、没动 id，于是留下 `id != id_for_target(target)`
            //    的条目（全项目的约定是二者自洽）。这里按 target 重算一次 —— 幂等。
            item["id"] = serde_json::Value::String(crate::apps::id_for_target(&target));
            *n += 1;
        }

        if let Some(children) = item.get_mut("children").and_then(|c| c.as_array_mut()) {
            fix_targets(children, old, new, n);
        }
    }
}

/// 递归复制目录（只用于上面那次一次性迁移；不做符号链接、权限继承等花活）。
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(to)?;
    let mut n = 0usize;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            n += copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
            n += 1;
        }
    }
    Ok(n)
}

/// 读取配置。文件不存在或解析失败一律退回默认值 —— 配置坏了不应该让程序起不来。
pub fn load(app: &AppHandle) -> Preferences {
    // 先做旧标识符的一次性迁移（幂等），再读配置
    migrate_old_identifier(app);
    let p = config_path(app);
    match std::fs::read_to_string(&p) {
        Ok(s) => match serde_json::from_str::<Preferences>(&s) {
            Ok(v) => {
                log_info!("[配置] 已加载 {}", p.display());
                v
            }
            Err(e) => {
                log_warn!("[配置] 解析失败，使用默认值: {e}");
                Preferences::default()
            }
        },
        Err(_) => {
            log_info!("[配置] 未找到 {}，使用默认值", p.display());
            Preferences::default()
        }
    }
}

pub fn save(app: &AppHandle, prefs: &Preferences) -> Result<(), String> {
    let p = config_path(app);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let s = serde_json::to_string_pretty(prefs).map_err(|e| format!("序列化失败: {e}"))?;
    std::fs::write(&p, s).map_err(|e| format!("写入 {} 失败: {e}", p.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PinnedApp;

    fn app(id: &str) -> PinnedApp {
        PinnedApp {
            id: id.into(),
            display_name: id.into(),
            target: format!("C:\\fake\\{id}.exe"),
            separator: false,
            is_folder: false,
            children: Vec::new(),
        }
    }

    fn folder(id: &str, children: Vec<PinnedApp>) -> PinnedApp {
        PinnedApp {
            id: id.into(),
            display_name: "新建文件夹".into(),
            target: String::new(),
            separator: false,
            is_folder: true,
            children,
        }
    }

    /// 「移出文件夹」的两条分支 + 三个前置校验（BL-17）。
    ///
    /// 这一段是纯逻辑，所以能这样测 —— 之前它和 `AppHandle` 缠在一起，只能靠自检慢慢跑。
    #[test]
    fn move_out_keeps_folder_until_last_child_then_dissolves_in_place() {
        // 布局：[A] [文件夹(B,C)] [D]  —— 文件夹在下标 1
        let mut p = vec![
            app("A"),
            folder("F", vec![app("B"), app("C")]),
            app("D"),
        ];

        // ① 移出 B：文件夹还在（1 项），B 落在文件夹**后面**（下标 2）
        assert_eq!(move_out_of_folder_in(&mut p, "B"), Ok(false));
        assert!(p[1].is_folder, "还有一项时文件夹必须留着");
        assert_eq!(p[1].children.len(), 1);
        assert_eq!(p[1].children[0].id, "C");
        assert_eq!(p[2].id, "B", "移出来的项要落在文件夹后面（看得见它去哪了）");
        assert_eq!(p.len(), 4, "移出 = 文件夹还留着 + 顶层多了一个");

        // ② 移出最后的 C：文件夹**原地解散**，C 接替它的位置（下标 1），不留空壳
        assert_eq!(move_out_of_folder_in(&mut p, "C"), Ok(true));
        assert_eq!(p.len(), 4, "解散不改变条目总数（少一个文件夹、多一个应用）");
        assert!(!p.iter().any(|x| x.is_folder), "不该留下空文件夹");
        assert_eq!(p[1].id, "C", "最后一项应该原地接替文件夹的位置");

        // ③ 不在任何文件夹里 → 明确报错，而不是静默什么都不做
        let e = move_out_of_folder_in(&mut p, "Z").unwrap_err();
        assert!(e.contains("不在任何文件夹里"), "实得：{e}");

        // ④ 顶层已经有同 id → 拒绝（否则会出现两个同名图标，而「移出 Dock」只删得掉一个）
        let mut dup = vec![app("X"), folder("F", vec![app("X")])];
        let e = move_out_of_folder_in(&mut dup, "X").unwrap_err();
        assert!(e.contains("已经有它了"), "实得：{e}");
    }

    /// 预置规则必须**在任何 Windows 上都能落地**（开源后的第一道门）。
    ///
    /// 这里断言的是"与机器无关的那部分"：
    /// - 规则表里有系统位置与系统自带程序（所有 Windows 都有）；
    /// - 规则里**不许出现具体安装路径**（那是照着开发机写的老毛病：
    ///   换台机器就是点不开的空图标）；
    /// - 打包应用的线索只能是**包族名前缀**（发布者哈希对所有机器相同）。
    #[test]
    fn default_pin_rules_are_machine_independent() {
        // ① 每条规则都能解析出"线索"，且线索里不含盘符 / 用户目录
        for rule in DEFAULT_PIN_RULES {
            let hint = match rule {
                PinRule::SystemExe(rel) => (*rel).to_string(),
                PinRule::ShellLocation(t) => (*t).to_string(),
                PinRule::Program(exe) => (*exe).to_string(),
                PinRule::Package(pfn, _) => (*pfn).to_string(),
            };
            assert!(!hint.is_empty(), "规则线索不能为空");
            let lower = hint.to_lowercase();
            assert!(
                !lower.contains(":\\") && !lower.contains("users\\") && !lower.contains("program files"),
                "预置规则里不许写具体安装路径（会换机器即失效）：{hint}"
            );
        }

        // ② 系统位置与 explorer 这两类必然存在（否则首屏会是空的）
        assert!(
            DEFAULT_PIN_RULES
                .iter()
                .any(|r| matches!(r, PinRule::ShellLocation(_))),
            "至少要预置系统位置"
        );
        assert!(
            crate::apps::system_exe("explorer.exe").is_some(),
            "任何 Windows 都有 %SystemRoot%\\explorer.exe"
        );

        // ③ 名字必须来自系统，不能是写死的中文常量
        let explorer = crate::apps::system_exe("explorer.exe").unwrap();
        let name = crate::apps::discover_name(&explorer);
        assert!(!name.trim().is_empty(), "显示名不能为空");
        assert!(
            !name.to_lowercase().ends_with(".exe"),
            "exe 的显示名要走版本信息（实测踩过：Shell 显示名对 exe 只会给回文件名）—— 实得 {name:?}"
        );

        // ④ WindowsApps 下的路径不许当条目（ACL 不允许直接启动）
        assert!(crate::apps::is_windowsapps_path(
            r"C:\Program Files\WindowsApps\Microsoft.WindowsTerminal_1.0_x64__8wekyb3d8bbwe\wt.exe"
        ));
        assert!(!crate::apps::is_windowsapps_path(r"C:\Windows\explorer.exe"));
    }
}
