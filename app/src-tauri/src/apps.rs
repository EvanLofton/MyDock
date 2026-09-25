//! Windows Shell 集成：窗口枚举、身份识别、窗口操作、启动。
//!
//! 本模块是整个项目里最「脏」的部分 —— Windows 没有 macOS 那样的「应用」抽象，
//! 只有进程、窗口和 AUMID 三样东西，需要自己拼出「应用」概念。
//!
//! 枚举规则的每一条都对应一个真实的坑，注释里写明了原因。

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;

use windows::core::{BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::model::{AppEntry, AppKind, PinnedApp, WindowRef};

// ---------------------------------------------------------------- 工具

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 取窗口所属进程的可执行文件全路径
fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// 取窗口类名（只用于 `DOCK_DEBUG=1` 的枚举明细）
fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// 从**进程**读 AUMID（包标识）。
///
/// 为什么必须有这个兜底：`PKEY_AppUserModel_ID` 是挂在**窗口**上的属性，
/// 只有被 ApplicationFrameHost 托管的 UWP 才稳定带上它。
///
/// 而 MSIX 打包应用里有一类**自带进程和窗口**的 —— 终端、新版记事本、画图、照片
/// 都是 —— 它们的窗口读不到这个属性。于是它们被当成普通 Win32 应用按 exe 路径识别，
/// 结果「列表里那条终端」和「运行起来的那个终端」身份对不上，**同一程序显示成两个图标**
/// （实测复现：终端跑起来后，`list_apps` 里同时出现
/// `uwp:microsoft.windowsterminal_...` 和
/// `c:\program files\windowsapps\...\windowsterminal.exe` 两条）。
///
/// 进程自己带着包标识，`GetApplicationUserModelId` 问它就能拿到同一个 AUMID。
/// 非打包进程会返回 `APPMODEL_ERROR_NO_APPLICATION`，此时返回 None 退回按路径识别。
fn process_aumid(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
    use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;

    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        // ❗第一次调用传 `None`（= NULL 缓冲区）时，返回的是
        // **ERROR_INSUFFICIENT_BUFFER(122)，不是 ERROR_SUCCESS**，
        // 同时把需要的长度写进 `len`。这是 Windows 「先问长度再取内容」类 API 的
        // 统一约定（GetPackageFullName / GetCurrentPackageFullName 也一样）。
        //
        // 曾经只认 ERROR_SUCCESS —— 结果这个函数**恒返回 None**，兜底形同虚设，
        // 表现就是「预置的记事本」和「运行起来的记事本」变成两个图标。
        let mut len = 0u32;
        let rc = GetApplicationUserModelId(h, &mut len, None);
        if (rc != ERROR_INSUFFICIENT_BUFFER && rc != ERROR_SUCCESS) || len == 0 {
            let _ = CloseHandle(h);
            return None;
        }

        let mut buf = vec![0u16; len as usize];
        let rc = GetApplicationUserModelId(h, &mut len, Some(PWSTR(buf.as_mut_ptr())));
        let _ = CloseHandle(h);
        if rc != ERROR_SUCCESS {
            return None;
        }

        // len 含结尾的 NUL
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..end]);
        (!s.is_empty()).then_some(s)
    }
}

/// 是否打印枚举明细（`DOCK_DEBUG=1`）。
///
/// 排查「同一个应用显示成两个图标」「该有运行点却没有」这类身份识别问题时，
/// 必须能看见**每个窗口算出来的身份是什么、为什么**，光看 Dock 上的结果无从下手。
fn debug_on() -> bool {
    std::env::var("DOCK_DEBUG").as_deref() == Ok("1")
}

/// 该进程是否以管理员权限运行。
///
/// 关键：用 `PROCESS_QUERY_LIMITED_INFORMATION` 打开 —— 从普通权限进程
/// **查询**更高完整性级别的进程令牌是允许的（任务管理器就是这么做的）。
pub fn is_process_elevated(pid: u32) -> bool {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(h, TOKEN_QUERY, &mut token).is_ok();
        let _ = CloseHandle(h);
        if !opened {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

/// UWP 应用被挂起时窗口仍然存在，但已被 DWM "cloak"。
/// 不检查这个会看到一堆幽灵窗口。
fn is_cloaked(hwnd: HWND) -> bool {
    let mut v: u32 = 0;
    unsafe {
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut v as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
    v != 0
}

fn title_of(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// 从版本信息里取 FileDescription（比 exe 文件名友好得多，例如「记事本」而不是「notepad」）。
///
/// **带缓存**（BL-3）：这一步是 `enumerate_apps()` 里最贵的一环（实测 2.6ms 里有 1.9ms 花在
/// 读版本资源上），而它每一轮都在为**同一批 exe** 重复做同样的事。
/// 缓存键是 exe 全路径（小写），值是 `Option<String>` ——
/// **`None` 也要缓存**：读不到版本信息是常态（很多小工具没有），不缓存的话每轮都白读一次。
fn file_description(path: &str) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    // path(小写) -> Option<FileDescription>
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();

    let key = path.to_lowercase();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().unwrap().get(&key) {
        return hit.clone();
    }
    let v = file_description_uncached(path);
    // ⚠️ 缓存不会自己失效：exe 被就地升级（版本信息变了）不会反映出来。
    // 这是可以的 —— 名字只在**新条目**进来时读一次，用户看到的是列表里存的名字。
    // 真要更稳妥的话得带 mtime，但那要点文件时间，收益不成比例。
    cache.lock().unwrap().insert(key, v.clone());
    v
}

fn file_description_uncached(path: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };

    unsafe {
        let w = wide(path);
        let mut dummy = 0u32;
        let size = GetFileVersionInfoSizeW(PCWSTR(w.as_ptr()), Some(&mut dummy));
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(
            PCWSTR(w.as_ptr()),
            Some(0),
            size,
            data.as_mut_ptr() as *mut core::ffi::c_void,
        )
        .ok()?;

        // 先取翻译表（语言 + 代码页）
        let mut trans_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut trans_len = 0u32;
        let sub = wide("\\VarFileInfo\\Translation");
        if !VerQueryValueW(
            data.as_ptr() as *const core::ffi::c_void,
            PCWSTR(sub.as_ptr()),
            &mut trans_ptr,
            &mut trans_len,
        )
        .as_bool()
            || trans_len < 4
        {
            return None;
        }
        let trans = std::slice::from_raw_parts(trans_ptr as *const u16, 2);
        let (lang, cp) = (trans[0], trans[1]);

        let key = wide(&format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription"));
        let mut val_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut val_len = 0u32;
        if !VerQueryValueW(
            data.as_ptr() as *const core::ffi::c_void,
            PCWSTR(key.as_ptr()),
            &mut val_ptr,
            &mut val_len,
        )
        .as_bool()
            || val_len == 0
        {
            return None;
        }
        let s = std::slice::from_raw_parts(val_ptr as *const u16, val_len as usize);
        let s = String::from_utf16_lossy(s).trim_end_matches('\0').trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }
}

// ---------------------------------------------------------------- 窗口枚举

#[derive(Default)]
struct RawWindow {
    /// 存 isize 而不是 HWND —— HWND 含裸指针、不是 Send/Sync，
    /// 放进 `static Mutex` 会编译不过。
    hwnd: isize,
    title: String,
    pid: u32,
}

static CANDIDATES: Mutex<Vec<RawWindow>> = Mutex::new(Vec::new());

unsafe extern "system" fn enum_cb(hwnd: HWND, _lp: LPARAM) -> BOOL {
    unsafe {
        // 1. 必须可见
        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;

        // 2. 工具窗口（托盘气泡、输入法候选等）不算；但带 APPWINDOW 的例外
        if ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0 {
            return TRUE;
        }

        // 3. 有 owner 的通常是对话框/弹出窗，不算独立应用条目
        let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
        if !owner.0.is_null() && ex & WS_EX_APPWINDOW.0 == 0 {
            return TRUE;
        }

        // 4. 被 DWM cloak 的（挂起的 UWP）跳过
        if is_cloaked(hwnd) {
            return TRUE;
        }

        // 5. 无标题的跳过（多数是无意义的辅助窗）
        let title = title_of(hwnd);
        if title.is_empty() {
            return TRUE;
        }

        // 6. 跳过我们自己
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            return TRUE;
        }

        CANDIDATES.lock().unwrap().push(RawWindow {
            hwnd: hwnd.0 as isize,
            title,
            pid,
        });
    }
    TRUE
}

/// 给一个 exe 路径取友好展示名（优先版本信息 FileDescription，退回文件名）
pub fn display_name_for(path: &str) -> String {
    file_description(path).unwrap_or_else(|| {
        std::path::Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string())
    })
}

/// UWP 应用的宿主进程名。仅作为**取不到 AUMID 时的兜底**判据。
const UWP_HOST: &str = "applicationframehost.exe";

/// `PKEY_AppUserModel_ID` —— 这个常量在 windows crate 里位于
/// `Win32::Storage::EnhancedStorage`（位置很反直觉），为避免多开一个 feature，
/// 这里按原值手写一份。
const PKEY_APP_USER_MODEL_ID: windows::Win32::Foundation::PROPERTYKEY =
    windows::Win32::Foundation::PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    };

/// 读窗口的 AppUserModelID（AUMID）。///
/// 这是身份解析的**第一优先级**：Windows 没有「应用」抽象，
/// 但打包应用（UWP / MSIX）会把自己的 AUMID 挂在窗口属性上。
/// 格式为 `PackageFamilyName!AppId`（**含 `!`**），
/// 这就给了我们区分「打包应用」与「普通 Win32 程序」的可靠判据。
fn window_aumid(hwnd: HWND) -> Option<String> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::System::Com::StructuredStorage::{
        PROPVARIANT, PropVariantClear, PropVariantToStringAlloc,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::{
        IPropertyStore, SHGetPropertyStoreForWindow,
    };

    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let mut pv: PROPVARIANT = store.GetValue(&PKEY_APP_USER_MODEL_ID).ok()?;
        let name = PropVariantToStringAlloc(&pv).ok()?;
        let out = name.to_string().ok();
        // 必须自己释放：PropVariantToStringAlloc 用 CoTaskMemAlloc 分配。
        // 这是每次枚举都走的路径，漏了会持续泄漏 —— 稳定性验收会挂在这上面。
        CoTaskMemFree(Some(name.0 as *const core::ffi::c_void));
        let _ = PropVariantClear(&mut pv);
        out.filter(|s| !s.is_empty())
    }
}

/// 打包应用（UWP / MSIX）的 AUMID 一定含 `!`
fn is_packaged(aumid: &str) -> bool {
    aumid.contains('!')
}

/// 打包应用的图标不能用宿主进程的 exe 取（那是 ApplicationFrameHost 的图标），
/// 而要用 shell 命名空间路径 `shell:AppsFolder\<AUMID>`。
fn apps_folder_path(aumid: &str) -> String {
    format!("{APPS_FOLDER_PREFIX}{aumid}")
}

/// `shell:AppsFolder\` —— 打包应用的 Shell 命名空间前缀。
const APPS_FOLDER_PREFIX: &str = "shell:AppsFolder\\";

// ---------------------------------------------------------------- 系统位置
//
// 「此电脑」「回收站」这类东西**不是应用**：没有 exe、没有窗口、永远不算"运行中"。
// 它们是 Shell 命名空间项，Dock 只要能做三件事就能收它们：
//
//   名字 → `shell_display_name`（问 Shell 要系统语言的名字）
//   图标 → `icons::extract_icon`（走 `IShellItemImageFactory`，它认 shell 路径）
//   打开 → `launch`（`ShellExecuteEx` 的 `lpFile` 直接吃 `shell:` 路径）
//
// 三条路都已实测（见本模块的 `system_locations_resolve` 单测）。

// ---------------------------------------------------------------- 首次运行预置用到的探测
//
// 为什么要有这几个函数（2026-09 为开源做准备时补的）：
// 早期版本把"常用应用"的**完整路径**写死在代码里（`C:\Program Files (x86)\Microsoft\Edge\…`）
// 和一堆中文显示名 —— 那是照着开发机写的，换台机器就会出现：
//   ① 装在别的盘/目录 → 路径不存在 → 留下一个点不开的空图标；
//   ② 英文系统上名字还是中文；
//   ③ 没装的应用照样出现在 Dock 上。
// 现在只保留两类**与机器无关**的线索，其余全部现查：
//   - `App Paths` 注册表里的 **exe 文件名**（微软安装规范要求所有 GUI 程序登记它，
//     值与安装位置无关，换机器/换盘都成立）；
//   - 打包应用的**包族名**（`Microsoft.WindowsTerminal_8wekyb3d8bbwe` ——
//     后面那串发布者哈希由发布者决定，**所有机器上相同**）。
// 显示名一律从系统读（Shell 显示名 / 文件版本信息），代码里不再有中文应用名常量。

/// 从注册表 `App Paths` 查某个 exe 的真实安装路径。
///
/// 查四组（先用户后本机、先 64 位视图后 `WOW6432Node`）：32 位程序（Chrome 常见）
/// 只登记在 `WOW6432Node` 下，64 位进程直接读 `SOFTWARE\…` 是读不到的。
///
/// 返回值已经过 `expand_env` + 存在性检查 —— 拿到的一定是能启动的真实文件。
pub fn app_paths_lookup(exe_name: &str) -> Option<String> {
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_WOW64_32KEY, RegCloseKey,
        RegOpenKeyExW, RegQueryValueExW,
    };

    for (root, extra) in [
        (HKEY_CURRENT_USER, None),
        (HKEY_CURRENT_USER, Some(KEY_WOW64_32KEY)),
        (HKEY_LOCAL_MACHINE, None),
        (HKEY_LOCAL_MACHINE, Some(KEY_WOW64_32KEY)),
    ] {
        for sub in [
            format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe_name}"),
            // 有些安装程序只登记不带扩展名的名字
            format!(
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{}",
                exe_name.trim_end_matches(".exe")
            ),
        ] {
            let w = wide(&sub);
            let mut key = windows::Win32::System::Registry::HKEY::default();
            let access = KEY_QUERY_VALUE | extra.unwrap_or_default();
            let rc = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), None, access, &mut key) };
            if rc.0 != 0 {
                continue;
            }
            // 默认值（名字传 null）= 可执行文件全路径
            let mut buf = [0u8; 2048];
            let mut len = buf.len() as u32;
            let rc = unsafe {
                RegQueryValueExW(
                    key,
                    PCWSTR::null(),
                    None,
                    None,
                    Some(buf.as_mut_ptr()),
                    Some(&mut len),
                )
            };
            unsafe {
                let _ = RegCloseKey(key);
            }
            if rc.0 != 0 {
                continue;
            }
            let raw = String::from_utf16_lossy(unsafe {
                std::slice::from_raw_parts(buf.as_ptr() as *const u16, (len as usize) / 2)
            });
            let p = expand_env(raw.trim_end_matches('\0').trim().trim_matches('"'));
            if std::path::Path::new(&p).is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// 系统目录下的自带程序（`%SystemRoot%\explorer.exe` 这类）。
///
/// 只对**所有 Windows 都必然存在**的东西拼路径 —— 别的应用一律走 [`app_paths_lookup`]。
pub fn system_exe(rel: &str) -> Option<String> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let p = format!("{root}\\{}", rel.trim_start_matches('\\'));
    std::path::Path::new(&p).is_file().then_some(p)
}

/// 打包应用（Store / 收件箱应用）的 target：`shell:AppsFolder\<包族名>!<AppId>`。
///
/// 存在性交给调用方用 [`target_exists`] 验证 —— 没装的机器上它自然为 `false`。
pub fn package_target(pfn: &str, app_id: &str) -> String {
    format!("{APPS_FOLDER_PREFIX}{pfn}!{app_id}")
}

/// 给一个**已确认存在**的 target 起个"系统里的名字"（预置时用，避免写死中文名）。
///
/// 两条路必须分开走（实测踩过：对 exe 调 Shell 显示名只会拿回**文件名本身**，
/// 于是 Dock 上出现 `msedge.exe` 这种名字）：
///
/// - `shell:` 目标（打包应用 / 系统位置）→ **Shell 显示名**（"设置"、"此电脑"，跟随系统语言）；
/// - 普通 exe → **版本信息里的产品名**（`display_name_for`，"Microsoft Edge"），退回文件名。
pub fn discover_name(target: &str) -> String {
    if is_shell_target(target) {
        if let Some(n) = shell_display_name(target) {
            if !n.trim().is_empty() {
                return n;
            }
        }
        return target.to_string();
    }
    display_name_for(target)
}

/// 这个 target 在 `WindowsApps` 下吗？（打包应用的真实安装目录）
///
/// **不能拿它当 Dock 条目**：那个目录的 ACL 只允许通过 AppsFolder 激活，
/// 直接 `ShellExecute` 会被拒绝（图标画得出来，点了没反应）。
/// 打包应用一律走 [`package_target`] 生成的 `shell:AppsFolder\…`。
pub fn is_windowsapps_path(target: &str) -> bool {
    let t = target.to_lowercase();
    t.contains(r"\windowsapps\") || t.contains("/windowsapps/")
}

/// 这个 target 是**普通文件路径**（exe）还是 Shell 命名空间项
pub fn is_shell_target(target: &str) -> bool {
    target.to_lowercase().starts_with("shell:")
}

// ---------------------------------------------------------------- .url 快捷方式
//
// Steam 的「添加桌面快捷方式」生成的不是 .lnk 而是 **`.url`（Internet 快捷方式）**：
//
// ```ini
// [InternetShortcut]
// URL=steam://rungameid/431960
// IconFile=D:\Steam\steamapps\common\wallpaper_engine\launcher.exe
// IconIndex=0
// ```
//
// 它和普通程序有两点不同，都要单独处理：
//   1. **图标**：Shell 对 `.url` 一律给那个通用的「Internet 快捷方式」图标
//      （实测 `SHGetFileInfo` 返回蓝地球+箭头，不是游戏图标），
//      真正想要的图标在 `IconFile=` 里 —— 见 `url_icon_source`；
//   2. **提权没有意义**：`runas` 会去提权**协议处理器**（Steam），不是在提权游戏。

/// 是不是 `.url`（Internet 快捷方式）文件。
pub fn is_url_shortcut(target: &str) -> bool {
    target.to_lowercase().ends_with(".url")
}

/// 是不是**裸协议 URL**（`steam://rungameid/431960`、`https://…`）。
///
/// 这类 target 也能启动（`ShellExecuteEx` 认识协议），但没有图标、
/// 也没有「在资源管理器里显示」可言 —— 界面上要区别对待。
pub fn is_url_scheme(target: &str) -> bool {
    !is_url_shortcut(target) && target.contains("://")
}

/// 读 `.url` 里的一个键（`URL` / `IconFile` / `IconIndex`…）。
///
/// 用 `GetPrivateProfileStringW` 而不是自己读文件：`.url` 就是 INI，
/// 这个 API 会自己处理 ANSI / UTF-16 / BOM 三种编码 ——
/// 自己 `read_to_string` 遇到 GBK 写的 `.url`（中文游戏名、中文路径）会直接失败。
pub fn url_shortcut_value(path: &str, key: &str) -> Option<String> {
    use windows::Win32::System::WindowsProgramming::GetPrivateProfileStringW;

    let file = wide(path);
    let section = wide("InternetShortcut");
    let key = wide(key);
    let mut buf = [0u16; 1024];
    let n = unsafe {
        GetPrivateProfileStringW(
            PCWSTR(section.as_ptr()),
            PCWSTR(key.as_ptr()),
            PCWSTR::null(),
            Some(&mut buf),
            PCWSTR(file.as_ptr()),
        )
    };
    if n == 0 {
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..n as usize]);
    let s = s.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// 回收站里现在有多少项（所有驱动器合计）。取不到就返回 `0`。
///
/// 用途（BL-8）：回收站的图标有**空 / 满两态**（`CLSID\{645FF040-…}\DefaultIcon`
/// 下的 `(Default)` 与 `empty` 是两个不同图标），但图标在**前端按 target 缓存**，
/// 不主动作废就永远停在第一次取到的那张。所以把这个计数当"版本号"传给前端：
/// 数字一变，缓存键就变，图标自然重取。
///
/// 另外它也决定右键菜单里「清空回收站」是否可点（空的时候置灰）。
pub fn recycle_bin_items() -> u32 {
    use windows::Win32::UI::Shell::{SHQUERYRBINFO, SHQueryRecycleBinW};

    let mut info = SHQUERYRBINFO {
        cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
        ..Default::default()
    };
    // `None` = 所有驱动器合计（图标的两态就是按"有没有东西"来的，不分盘）
    unsafe {
        if SHQueryRecycleBinW(PCWSTR::null(), &mut info).is_err() {
            return 0;
        }
    }
    info.i64NumItems.max(0) as u32
}

/// 这个 target 是不是**回收站**（只有它才有「清空回收站」）。
pub fn is_recycle_bin(target: &str) -> bool {
    target.eq_ignore_ascii_case("shell:RecycleBinFolder")
}

/// **清空回收站**（不可撤销：里面的东西被永久删除）。
///
/// ⚠️ 刻意**不传** `SHERB_NOCONFIRMATION` —— 让 Windows 自己弹那句
/// 「确定要永久删除这 N 个项目吗？」。这是系统级的破坏性操作，
/// 我们不该用一句自己的文案替用户承担这个决定。
///
/// 调用会**阻塞到用户回答**（确认框是模态的），所以必须从工作线程调。
pub fn empty_recycle_bin() -> Result<(), String> {
    use windows::Win32::UI::Shell::{
        SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW,
    };

    unsafe {
        SHEmptyRecycleBinW(None, PCWSTR::null(), SHERB_NOPROGRESSUI | SHERB_NOSOUND)
            .map_err(|e| format!("清空回收站失败: {e}"))
    }
}

/// Dock 用的**临时文件夹**路径（`%LOCALAPPDATA%\<identifier>\临时文件`），需要时创建。
///
/// 为什么放 LocalAppData 而不是"文档"或 %TEMP%：
/// - 放"文档"会往用户的资料目录里塞东西；
/// - `%TEMP%` 是给**程序**用的临时目录，系统会随手清、也塞满别人拉的垃圾；
/// - 这里是"应用自己管的一块地"，跟着 Dock 的标识符走，卸载时一起删才合理。
///
/// 名字用中文是为了让用户在资源管理器里一眼认出来（它会被打开给用户看）。
///
/// ⚠️ 标识符改过一次（`dev.local.dock` → 现在这个），老目录由
/// `store::migrate_old_identifier` 一次性搬过来 —— 里面可能有用户放的文件，不能丢。
pub fn temp_folder_path() -> Result<std::path::PathBuf, String> {
    use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    let raw = unsafe {
        SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None)
            .map_err(|e| format!("取 LocalAppData 失败: {e}"))?
    };
    let base = unsafe { raw.to_string() }.map_err(|e| format!("路径转换失败: {e}"))?;
    // SHGetKnownFolderPath 用 CoTaskMemAlloc 分配，必须自己释放
    unsafe {
        windows::Win32::System::Com::CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void));
    }
    Ok(std::path::Path::new(&base)
        .join(crate::IDENTIFIER)
        .join("临时文件"))
}

/// `.url` 指定的图标来源（`IconFile=` 那一条），拿不到就 `None`。///
/// 只认**文件路径**形式（exe / ico / dll）：拿它去走正常的取图标流程。
/// `IconIndex` 暂时忽略 —— 见 `icons::extract_icon` 的说明。
pub fn url_icon_source(target: &str) -> Option<String> {
    if !is_url_shortcut(target) {
        return None;
    }
    let src = url_shortcut_value(target, "IconFile")?;
    // 快捷方式里存的是**环境变量展开前**也可能存的路径（`%ProgramFiles%\…`）
    let expanded = expand_env(&src);
    let p = std::path::Path::new(&expanded);
    if p.is_file() { Some(expanded) } else { None }
}

/// 展开 `%VAR%` 形式的环境变量（不认识原样返回）。
fn expand_env(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 是不是**系统位置**（此电脑 / 回收站…）。
///
/// 注意要和打包应用区分开：两者的 target 都以 `shell:` 开头，
/// 但 `shell:AppsFolder\<AUMID>` 是**应用**（该说「启动」、可以提权），
/// 而 `shell:MyComputerFolder` 是**位置**（该说「打开」、提权没有意义）。
pub fn is_shell_location(target: &str) -> bool {
    is_shell_target(target) && !target.starts_with(APPS_FOLDER_PREFIX)
}

/// 取 Shell 命名空间项的**系统语言**显示名。
///
/// 为什么不复用 `display_name_for`：那条路是「exe 版本信息 → 文件名」，
/// 对 `shell:MyComputerFolder` 只会得到 `MyComputerFolder` 这种内部名。
/// 这里问 Shell 自己，于是中文系统上是「此电脑」、英文系统上是 "This PC"。
pub fn shell_display_name(target: &str) -> Option<String> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY};

    let w = wide(target);
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok()?;
        let name = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
        let out = name.to_string().ok().filter(|s| !s.is_empty());
        // `GetDisplayName` 用 CoTaskMemAlloc 分配，必须自己释放
        CoTaskMemFree(Some(name.0 as *const core::ffi::c_void));
        out
    }
}

/// Dock 上可以直接放进去的**系统位置**：`(target, 取不到系统名时的兜底文案)`。
///
/// 顺序就是「用户会想先放哪个」的顺序，加到 Dock 上时也按这个顺序插到最左侧。
pub const SYSTEM_LOCATIONS: &[(&str, &str)] = &[
    ("shell:MyComputerFolder", "此电脑"),
    ("shell:RecycleBinFolder", "回收站"),
];

/// 系统位置的兜底名（Shell 查询失败时用；正常情况查得到）
pub fn fallback_name_for(target: &str) -> String {
    SYSTEM_LOCATIONS
        .iter()
        .find(|(t, _)| t.eq_ignore_ascii_case(target))
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| target.to_string())
}

/// 由 `target` 反推稳定的应用 id。
///
/// ⚠️ 规则必须和 `enumerate_apps` **完全一致**，否则同一条目会变成两个
/// （预置的应用图标和真正运行起来之后的图标各显示一个）。
///
/// - `shell:AppsFolder\<AUMID>`（打包应用）→ `uwp:<AUMID 小写>`
/// - 其他（exe 路径 / `shell:MyComputerFolder` 这类系统位置）→ 全小写
///
/// 判据是「target 是不是 shell:AppsFolder 形式」，这跟运行时
/// 「窗口 AUMID 里含不含 `!`」是等价的 —— 只有打包应用才会用这个 target。
/// （注意 `explorer.exe` / `msedge.exe` 的 AUMID 是 `Microsoft.Windows.Explorer` /
/// `MSEdge`，**不含 `!`**，所以它们走 exe 路径这条，不走打包那条。）
pub fn id_for_target(target: &str) -> String {
    match target.strip_prefix(APPS_FOLDER_PREFIX) {
        Some(aumid) => format!("uwp:{}", aumid.to_lowercase()),
        None => target.to_lowercase(),
    }
}

fn is_uwp_host(path: &str) -> bool {
    path.to_lowercase().ends_with(UWP_HOST)
}

/// 枚举当前所有「有窗口的应用」，按应用聚合。
pub fn enumerate_apps() -> Vec<AppEntry> {
    CANDIDATES.lock().unwrap().clear();
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(0));
    }

    let raw = std::mem::take(&mut *CANDIDATES.lock().unwrap());
    let foreground = unsafe { GetForegroundWindow() };

    // 身份归并优先级（对应文档 §4.2）：
    //   1. **窗口**的 AUMID，且是打包应用（含 `!`）→ 用 AUMID 当身份，
    //      target 用 shell:AppsFolder 路径
    //   2. **进程**的 AUMID —— 自带窗口的 MSIX 应用（终端 / 新版记事本 / 画图 /
    //      照片…）窗口上读不到属性，只有进程带着包标识。少了这一条，它们会被
    //      按 exe 路径识别，于是「预置的」和「运行起来的」变成两个图标。
    //      详见 `process_aumid` 的说明。
    //   3. 否则用 exe 全路径（小写）
    //
    // 走 AUMID 这一条是必须的：被 ApplicationFrameHost 托管的 UWP（设置/计算器…）
    // 只按 exe 聚合会全部塌缩成一个条目（实测复现过）。
    let mut groups: HashMap<String, AppEntry> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for w in raw {
        let Some(path) = process_path(w.pid) else {
            continue;
        };
        let hwnd = HWND(w.hwnd as *mut core::ffi::c_void);

        // 只接受「含 `!`」的 AUMID —— 那才是打包应用。
        // （`explorer.exe` / `msedge.exe` 的 AUMID 是 `Microsoft.Windows.Explorer` /
        //  `MSEdge`，不含 `!`，必须继续走 exe 路径那条，否则会和预置项对不上。）
        let win_aumid = window_aumid(hwnd);
        let proc_aumid = process_aumid(w.pid);
        let aumid = win_aumid
            .clone()
            .filter(|a| is_packaged(a))
            .or_else(|| proc_aumid.clone().filter(|a| is_packaged(a)));
        let packaged = aumid.is_some();

        if debug_on() {
            let t: String = w.title.chars().take(40).collect();
            log_info!(
                "[枚举] pid={:<6} 类={:<28} '{}'\n        窗口AUMID   = {}\n        进程AUMID   = {}\n        判定         = {}",
                w.pid,
                class_of(hwnd),
                t,
                win_aumid.as_deref().unwrap_or("(无)"),
                proc_aumid.as_deref().unwrap_or("(无)"),
                if packaged {
                    "打包应用 → 用 AUMID 当身份"
                } else {
                    "Win32 → 用 exe 路径当身份"
                }
            );
        }

        let (id, name, kind, target) = if packaged {
            let a = aumid.clone().unwrap();
            // 打包应用的展示名：AUMID 的 AppId 段（`!` 之后）比
            // 「Application Frame Host」有意义得多；真正的友好名要走
            // PackageManager，成本高，这里先用窗口标题兜底
            let fallback = a
                .split_once('!')
                .map(|(_, app)| app.to_string())
                .unwrap_or_else(|| a.clone());
            let name = if w.title.is_empty() {
                fallback
            } else {
                w.title.clone()
            };
            let target = apps_folder_path(&a);
            let id = id_for_target(&target);
            (id, name, AppKind::Uwp, target)
        } else if is_uwp_host(&path) && aumid.is_none() {
            // 兜底：取不到 AUMID 的 UWP 宿主。
            //
            // 这里的 id **专门用窗口标题**、不能按 target 算：target 只能是宿主
            // applicationframehost.exe（本来也启动不了什么），按 target 算会让
            // 「设置」「计算器」「照片」全部塌缩成一条 —— 早期实测踩过这个坑。
            (
                format!("uwp:{}", w.title.to_lowercase()),
                w.title.clone(),
                AppKind::Uwp,
                path.clone(),
            )
        } else {
            let name = file_description(&path).unwrap_or_else(|| {
                std::path::Path::new(&path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.clone())
            });
            let id = id_for_target(&path);
            (id, name, AppKind::Win32, path.clone())
        };

        let entry = groups.entry(id.clone()).or_insert_with(|| {
            order.push(id.clone());
            AppEntry {
                id: id.clone(),
                display_name: name,
                kind,
                target,
                is_elevated: is_process_elevated(w.pid),
                has_foreground: false,
                running: true,
                windows: Vec::new(),
                separator: false,
                is_folder: false,
                children: Vec::new(),
            }
        });

        let minimized = unsafe { IsIconic(hwnd).as_bool() };

        // ⚠️ 最小化的窗口**仍然可能**是 `GetForegroundWindow()` 的返回值，
        // 但用户看到的是「它没在前台」。不排除的话点击行为会走错分支：
        // 前端 `onActivate` 是「hasForeground → 最小化，否则 → 激活」，
        // 于是一个已经最小化的程序被点后仍然去「最小化」，**还原不回来**。
        // 实测数据：最小化后的记事本 前台=true、最小化=true。
        if hwnd == foreground && !minimized {
            entry.has_foreground = true;
        }
        entry.windows.push(WindowRef {
            hwnd: hwnd.0 as u64,
            title: w.title,
            minimized,
        });
    }

    order
        .into_iter()
        .filter_map(|k| groups.remove(&k))
        .collect()
}

/// 一个 `target` 在本机是否真的可用。
///
/// 用途：预置常用应用时过滤候选。候选表是通用的，但每台机器装了什么不一样 ——
/// 不过滤就会留下一批点不开的空图标。
///
/// 两类 target 都走 Shell 命名空间：Win32 是文件路径，打包应用是
/// `shell:AppsFolder\<AUMID>`（**不是**文件路径，`Path::exists()` 对它恒为 false）。
/// 所以统一用 `SHCreateItemFromParsingName` 探测，它对两者的语义都是「能不能解析出 Shell 项」。
pub fn target_exists(target: &str) -> bool {
    use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName};
    let w = wide(target);
    let r: windows::core::Result<IShellItem> =
        unsafe { SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None) };
    r.is_ok()
}

/// 把「用户放在 Dock 上的图标列表」与「当前正在运行的应用」合并成 Dock 要显示的内容。
///
/// # 唯一的规则：**位置完全由用户决定**
///
/// Dock 上显示哪些图标、按什么顺序，**只看 `dock_apps` 这个列表** ——
/// 它是用户通过拖放 / 「添加应用…」/ 右键加进来的，顺序就是用户排的顺序
/// （出厂只是给了几个初值，见 `store::DEFAULT_PINS`）。
///
/// **运行状态不参与决定位置，也不决定是否出现**：一个应用没在跑，它的图标仍然
/// 待在原处，只是没有运行指示点（白点）。运行只影响白点和「点击是启动还是切换」。
///
/// ## 为什么删掉了「未固定的运行中应用排在后」
///
/// 早期版本有两段：`[固定项…][未固定的运行中应用…]`（后者按首次出现顺序）。
/// 那与本模型直接冲突，会带来两个后果：
///
///   1. **你没添加过的程序一跑就自己冒出来、退出又消失** —— 位置不是用户决定的；
///   2. 同一个程序在「运行中」和「未运行」两种状态下**可能落在不同位置**。
///
/// 所以那一段连同 `discovered` 字段一起删了。顺带也消掉了它带来的一堆麻烦
/// （Z 序导致的乱跳、首次出现顺序的持久化、指纹要跟着改等）。
///
/// 保留的一点：名称取**列表里的**，不取运行态的 —— 运行态的展示名来自窗口标题，
/// 会随你打开的文件变（实测：列表里的「记事本」会变成
/// 「Microsoft.PowerShell_profile.ps1 - Notepad」）。
pub fn enumerate_dock_apps(dock_apps: &[PinnedApp]) -> Vec<AppEntry> {
    let running = enumerate_apps();
    dock_apps.iter().map(|p| entry_for(p, &running)).collect()
}

/// 把一个用户条目（应用 / 分割线 / 文件夹）翻成前端要的 `AppEntry`。
///
/// 三种条目的规则：
///  - **分割线**：原样输出，不参与运行匹配；
///  - **文件夹**：递归翻 children；`running` = 里面有任何一个在运行
///    （用来点亮文件夹的运行点 —— 一眼能看出"里面有东西开着"）；
///  - **应用**：在运行集合里找得到就取运行态（带窗口句柄，才能激活/最小化），
///    找不到就给占位项（点击去启动）。名字一律用**列表里的**，不用窗口标题。
///
/// 为什么用 `find` 而不是把运行项从集合里"消费掉"：同一个应用可以既在顶层
/// 又在某个文件夹里，消费掉的话文件夹里那份会显示成"没在运行"。
fn entry_for(p: &PinnedApp, running: &[AppEntry]) -> AppEntry {
    if p.separator {
        return AppEntry {
            id: p.id.clone(),
            display_name: String::new(),
            kind: AppKind::Unknown,
            target: String::new(),
            is_elevated: false,
            has_foreground: false,
            running: false,
            windows: Vec::new(),
            separator: true,
            is_folder: false,
            children: Vec::new(),
        };
    }

    if p.is_folder {
        let children: Vec<AppEntry> = p.children.iter().map(|c| entry_for(c, running)).collect();
        return AppEntry {
            id: p.id.clone(),
            display_name: p.display_name.clone(),
            kind: AppKind::Unknown,
            target: String::new(),
            is_elevated: false,
            has_foreground: false,
            // ❗文件夹**自己不点运行点**（用户明确要求）：白点的含义是"这个图标的东西开着"，
            // 而文件夹不是应用。里面谁在跑，就由**里面那一格**自己点（见 panel 的 folder-dot）。
            // 顺带也符合"白点 = 有窗口"这条既有语义：文件夹没有窗口。
            running: false,
            windows: Vec::new(),
            separator: false,
            is_folder: true,
            children,
        };
    }

    match running.iter().find(|a| a.id == p.id) {
        // 在运行：取运行态，这样才有窗口句柄可用于激活/最小化
        Some(e) => {
            let mut e = e.clone();
            e.display_name = p.display_name.clone();
            e
        }
        // 没在运行：给一个占位项，点击时去启动它
        None => AppEntry {
            id: p.id.clone(),
            display_name: p.display_name.clone(),
            kind: AppKind::Win32,
            target: p.target.clone(),
            is_elevated: false,
            has_foreground: false,
            running: false,
            windows: Vec::new(),
            separator: false,
            is_folder: false,
            children: Vec::new(),
        },
    }
}

// ---------------------------------------------------------------- 窗口操作

/// 激活某个窗口。
///
/// Windows 有前台锁定：后台进程直接 `SetForegroundWindow` 常被**静默忽略**。
/// 顺序按可靠性排列，前一步失败就退到下一步。
pub fn activate(hwnd: HWND) -> Result<(), String> {
    if hwnd.0.is_null() {
        return Err("无效窗口句柄".into());
    }
    unsafe {
        // 已最小化先还原
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }

        let fg = GetForegroundWindow();
        if fg == hwnd {
            return Ok(());
        }

        let fg_tid = if fg.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(fg, None)
        };
        let our_tid = windows::Win32::System::Threading::GetCurrentThreadId();
        let attached = fg_tid != 0 && fg_tid != our_tid;
        if attached {
            let _ = windows::Win32::System::Threading::AttachThreadInput(fg_tid, our_tid, true);
        }

        let _ = BringWindowToTop(hwnd);
        let ok = SetForegroundWindow(hwnd).as_bool();

        if attached {
            let _ = windows::Win32::System::Threading::AttachThreadInput(fg_tid, our_tid, false);
        }

        if SetForegroundWindow(hwnd).as_bool() || ok {
            return Ok(());
        }

        // 兜底：闪一下任务栏，至少让用户知道是哪个窗口
        let mut info = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd,
            dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
            uCount: 3,
            dwTimeout: 0,
        };
        let _ = FlashWindowEx(&mut info);
        Err("前台锁定：已还原窗口并闪烁任务栏".into())
    }
}

pub fn minimize(hwnd: HWND) -> Result<(), String> {
    if hwnd.0.is_null() {
        return Err("无效窗口句柄".into());
    }
    unsafe {
        if ShowWindow(hwnd, SW_MINIMIZE).as_bool() {
            Ok(())
        } else {
            Err("最小化失败".into())
        }
    }
}

pub fn close_window(hwnd: HWND) -> Result<(), String> {
    if hwnd.0.is_null() {
        return Err("无效窗口句柄".into());
    }
    // 对提权窗口，UIPI 会拦下 PostMessage —— 这里返回可识别的错误
    unsafe {
        PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0))
            .map_err(|e| format!("发送 WM_CLOSE 失败（提权窗口会被 UIPI 拦截）: {e}"))
    }
}

// ---------------------------------------------------------------- 启动

/// 启动一个可执行文件。`run_as_admin = true` 时走 UAC 提权（`lpVerb = "runas"`）。
///
/// `target` 既可以是 exe 全路径，也可以是 Shell 命名空间项
/// （`shell:AppsFolder\<AUMID>`、`shell:MyComputerFolder`…）——
/// `ShellExecuteEx` 的 `lpFile` 本来就吃这些形式，所以「打开此电脑」和
/// 「启动 Edge」在这里是同一条代码路径。
pub fn launch(target: &str, run_as_admin: bool) -> Result<(), String> {
    use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};

    let file = wide(target);
    let verb = wide(if run_as_admin { "runas" } else { "open" });

    unsafe {
        let mut sei = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            nShow: SW_SHOWNORMAL.0 as i32,
            ..Default::default()
        };

        if let Err(e) = ShellExecuteExW(&mut sei) {
            // 用户在 UAC 弹窗点了「否」—— 这不是错误，静默忽略
            const ERROR_CANCELLED: u32 = 1223;
            let code = GetLastError().0;
            if code == ERROR_CANCELLED {
                return Ok(());
            }
            return Err(format!("ShellExecuteEx 失败: {e} (code={code})"));
        }
        if !sei.hProcess.is_invalid() {
            let _ = CloseHandle(sei.hProcess);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

    /// `.url`（Internet 快捷方式）的三条判据：认得出、读得出 IconFile、空壳要说清原因。
    ///
    /// 用**临时文件**造样本，不依赖用户桌面上有没有 Steam 游戏 ——
    /// 依赖真实文件的那种测试在别人机器上必然失败。
    #[test]
    fn url_shortcut_is_recognized_and_its_icon_source_resolved() {
        let dir = std::env::temp_dir().join("dock-url-test");
        let _ = std::fs::create_dir_all(&dir);
        let url_file = dir.join("Some Game.url");
        // `%WINDIR%\system32\notepad.exe` 一定存在，用来验证 %VAR% 会被展开
        let ico = format!(r"{}\system32\notepad.exe", std::env::var("WINDIR").unwrap_or_default());
        std::fs::write(
            &url_file,
            format!(
                "[{{000214A0-0000-0000-C000-000000000046}}]\r\nProp3=19,0\r\n[InternetShortcut]\r\n\
                 URL=steam://rungameid/431960\r\nIconIndex=0\r\nIconFile=%WINDIR%\\system32\\notepad.exe\r\n"
            ),
        )
        .unwrap();
        let p = url_file.display().to_string();

        assert!(is_url_shortcut(&p), "`.url` 应该被认出来");
        assert!(
            !is_url_scheme(&p),
            "`.url` 是**文件**，不该被当成裸协议 URL（两者的菜单行为不同）"
        );
        assert_eq!(
            url_shortcut_value(&p, "URL").as_deref(),
            Some("steam://rungameid/431960")
        );
        assert_eq!(
            url_icon_source(&p).as_deref(),
            Some(ico.as_str()),
            "IconFile 里的 %WINDIR% 要展开，且展开后必须真的存在"
        );

        // 裸协议 URL：能启动，但没有图标可谈，也没有"文件所在位置"
        assert!(is_url_scheme("steam://rungameid/431960"));
        assert!(!is_url_shortcut("steam://rungameid/431960"));
        assert_eq!(url_icon_source("steam://rungameid/431960"), None);
        assert!(is_url_scheme("https://example.com"));

        // 空壳 `.url`（没有 URL=）必须被拒绝，而且理由要具体 ——
        // 加进去只会点了没反应，用户需要知道为什么
        let empty = dir.join("Empty.url");
        std::fs::write(&empty, "[InternetShortcut]\r\nIconFile=whatever\r\n").unwrap();
        let why = crate::drop::resolve_target(&empty).unwrap_err();
        assert!(why.contains("没有 URL"), "实得：{why}");

        // 真正带 URL 的 `.url`：target 保留**快捷方式路径本身**（不是里面的 URL）
        assert_eq!(crate::drop::resolve_target(&url_file).unwrap(), p);

        let _ = std::fs::remove_file(&url_file);
        let _ = std::fs::remove_file(&empty);
    }

    /// `file_description` 的缓存（BL-3）：同一个 exe 读两次必须一致，
    /// 而且**读不到也要缓存**（`None` 是常态：很多小工具没有版本信息，
    /// 不缓存 `None` 的话每轮枚举都白读一次版本资源）。
    ///
    /// ⚠️ 用 `explorer.exe` 而不是 `notepad.exe` 当样本：后者在 Win11 上已经变成
    /// **商店应用的 stub**，版本信息不保证存在 —— 那样这条单测在别人机器上会红。
    /// `explorer.exe` 在所有 Windows 上都有完整版本信息。
    #[test]
    fn file_description_cache_is_consistent() {
        let win = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let exe = format!(r"{win}\explorer.exe");
        let a = file_description(&exe);
        let b = file_description(&exe);
        assert_eq!(a, b, "同一个 exe 两次结果必须一致（缓存命中也要返回同样的值）");
        assert!(a.is_some(), "explorer.exe 应该有 FileDescription");

        // 大小写不同要命中同一条（键是小写化的）
        let c = file_description(&exe.to_uppercase());
        assert_eq!(a, c, "键要小写化，否则同一路径两种写法会各读一次");

        // 不存在的文件：两次都是 None，且不能 panic
        let ghost = r"C:\__dock_no_such_file__.exe";
        assert_eq!(file_description(ghost), None);
        assert_eq!(file_description(ghost), None, "None 也要缓存，第二次不能再读盘");
    }

    /// `SHCreateItemFromParsingName` 要求调用线程初始化过 COM。
    /// 正常运行时由 `main()` 做，单测里得自己做一次
    /// （重复初始化返回 S_FALSE / RPC_E_CHANGED_MODE 都无所谓）。
    fn com() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
    }

    /// 「此电脑 / 回收站」能放进 Dock 的前提是**三条路都通**：
    /// 名字（系统语言）、图标（真取得到而且不是一块不透明底板）、打开（ShellExecute）。
    ///
    /// 前两条在这里断言；第三条（打开）没法在单测里点一下，靠 `ShellExecuteEx`
    /// 对 `shell:` 路径的标准行为，并已手工验证。
    #[test]
    fn system_locations_resolve_name_and_icon() {
        com();
        for (target, fallback) in SYSTEM_LOCATIONS {
            let name = shell_display_name(target).unwrap_or_default();
            assert!(
                !name.is_empty(),
                "{target} 取不到系统显示名（界面上会退成兜底文案「{fallback}」）"
            );
            assert!(
                is_shell_location(target),
                "{target} 应该被认成系统位置而不是打包应用"
            );

            let icon = crate::icons::extract_icon(target, 128)
                .unwrap_or_else(|| panic!("{target} 取不到图标（前端会退回首字母占位）"));
            let s = crate::icons::alpha_stats(&icon);
            assert!(
                icon.width >= 32 && icon.height >= 32,
                "{target} 图标只有 {}x{}",
                icon.width,
                icon.height
            );
            // 不透明占比过高说明取回来的不是图标而是整块底板（本项目踩过这个坑）
            assert!(
                s.transparent_ratio > 0.2,
                "{target} 图标透明像素只占 {:.0}%，形态可疑",
                s.transparent_ratio * 100.0
            );
            log_info!(
                "  {target} → 「{name}」 图标 {}x{} 透明 {:.0}%",
                icon.width,
                icon.height,
                s.transparent_ratio * 100.0
            );
        }
    }

    /// 文件夹**自己不点运行点**，但里面在跑的格子要能看出来。
    ///
    /// 这两条是一起被提出来的（用户："文件夹下面不该有白点，而文件夹里面的软件在运行时
    /// 应该有白点"），所以一起钉住：`running` 在文件夹上是 false、
    /// 在孩子上是各自真实状态。
    #[test]
    fn folder_never_reports_running_but_children_do() {
        com();
        let running = enumerate_apps();
        // 挑一个**当前确定在跑**的应用和它对应的 pinned 条目（用真机的运行集合，
        // 不构造假数据 —— 身份匹配规则本身也要一起验证）
        let Some(live) = running.first() else {
            log_info!("  （当前没有可用的运行中应用，跳过）");
            return;
        };
        let child = PinnedApp {
            id: live.id.clone(),
            display_name: live.display_name.clone(),
            target: live.target.clone(),
            separator: false,
            is_folder: false,
            children: Vec::new(),
        };
        let folder = PinnedApp {
            id: "folder-x".into(),
            display_name: "文件夹".into(),
            target: String::new(),
            separator: false,
            is_folder: true,
            children: vec![child],
        };
        let out = entry_for(&folder, &running);
        assert!(out.is_folder);
        assert!(!out.running, "文件夹自己不该有运行点");
        assert_eq!(out.children.len(), 1);
        assert!(out.children[0].running, "里面那个在跑，格子里要有运行点");
        log_info!("  文件夹 running={} / 里面 '{}' running={}", out.running, out.children[0].display_name, out.children[0].running);
    }

    #[test]
    fn system_location_is_not_confused_with_packaged_app() {        com();
        assert!(is_shell_location("shell:MyComputerFolder"));
        assert!(is_shell_location("shell:RecycleBinFolder"));
        // 打包应用的 target 也以 shell: 开头，但它是**应用**不是位置
        assert!(!is_shell_location(
            r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"
        ));
        assert!(!is_shell_location(r"C:\Windows\explorer.exe"));

        // id 规则：位置 → 全小写；打包应用 → uwp: 前缀。两者不能撞。
        assert_eq!(
            id_for_target("shell:MyComputerFolder"),
            "shell:mycomputerfolder"
        );
        assert!(id_for_target(r"shell:AppsFolder\X!App").starts_with("uwp:"));
        assert_ne!(
            id_for_target("shell:MyComputerFolder"),
            id_for_target(r"shell:AppsFolder\MyComputerFolder!App")
        );
    }
}
