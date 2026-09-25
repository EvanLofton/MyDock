//! 「添加应用…」文件选择器。
//!
//! 为什么需要它：右键菜单只能固定**正在运行**的应用。
//! 但用户想放上 Dock 的，往往正是那些**还没启动**的常用应用 ——
//! 没有这个入口，「日常启动」这条验收就只成立一半。
//!
//! 用 `IFileOpenDialog`（Vista+ 的通用文件对话框），而不是已过时的 `GetOpenFileName`。
//!
//! 选中的东西交给 `drop::resolve_target` —— 和**拖放走同一条路**：
//! `.exe` 直接用、`.lnk` 解析真实目标、`.url`（Steam 游戏快捷方式）保留路径。
//! 两个入口各写一套解析，迟早会出现"拖进去能用、选进去不能用"。

#![allow(dead_code)]

use std::ffi::c_void;

use tauri::AppHandle;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
};

use crate::model::PinnedApp;

/// 弹出文件选择器，返回用户选中的 exe 全路径（取消返回 `None`）。
pub fn pick_executable() -> Result<Option<String>, String> {
    unsafe {
        // `spawn_blocking` 的线程默认没初始化 COM，必须自己来
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL)
            .map_err(|e| format!("创建文件对话框失败: {e}"))?;

        // 第一项就把三种都列出来：`.url` 不是可执行文件，只写 (*.exe) 的话
        // 用户根本不会想到「所有文件」里能找到 Steam 的桌面快捷方式。
        let n1 = crate::win_layer::wide("应用与快捷方式 (*.exe;*.lnk;*.url)");
        let s1 = crate::win_layer::wide("*.exe;*.lnk;*.url");
        let n2 = crate::win_layer::wide("可执行文件 (*.exe)");
        let s2 = crate::win_layer::wide("*.exe");
        let n3 = crate::win_layer::wide("所有文件");
        let s3 = crate::win_layer::wide("*.*");
        let filters = [
            COMDLG_FILTERSPEC {
                pszName: windows::core::PCWSTR(n1.as_ptr()),
                pszSpec: windows::core::PCWSTR(s1.as_ptr()),
            },
            COMDLG_FILTERSPEC {
                pszName: windows::core::PCWSTR(n2.as_ptr()),
                pszSpec: windows::core::PCWSTR(s2.as_ptr()),
            },
            COMDLG_FILTERSPEC {
                pszName: windows::core::PCWSTR(n3.as_ptr()),
                pszSpec: windows::core::PCWSTR(s3.as_ptr()),
            },
        ];
        let _ = dialog.SetFileTypes(&filters);

        if let Ok(mut opts) = dialog.GetOptions() {
            opts |= FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM;
            let _ = dialog.SetOptions(opts);
        }
        let _ = dialog.SetTitle(windows::core::w!("选择要添加到 Dock 的应用或快捷方式"));

        if dialog.Show(None).is_err() {
            return Ok(None); // 用户取消
        }

        let item = dialog
            .GetResult()
            .map_err(|e| format!("取选择结果失败: {e}"))?;
        let p = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(|e| format!("取文件路径失败: {e}"))?;
        let s = p.to_string().ok();
        // GetDisplayName 用 CoTaskMemAlloc 分配，必须自己释放
        CoTaskMemFree(Some(p.0 as *const c_void));
        Ok(s)
    }
}

/// 选一个 exe / 快捷方式并固定到 Dock。
pub fn pick_and_pin(app: &AppHandle) -> Result<(), String> {
    let Some(path) = pick_executable()? else {
        return Ok(()); // 取消
    };
    // 和拖放**同一条**解析路径：`.lnk` 解析真实目标、`.url` 保留快捷方式路径、
    // 不支持的类型给出明确原因（而不是静默什么都不做）。
    let target = match crate::drop::resolve_target(std::path::Path::new(&path)) {
        Ok(t) => t,
        Err(why) => {
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            crate::drop::toast(app, &format!("{name}：{why}"));
            return Err(why);
        }
    };
    let display_name = crate::apps::display_name_for(&target);
    log_info!("[添加应用] {display_name}  <- {target}");
    crate::store::pin(
        app,
        PinnedApp {
            id: crate::apps::id_for_target(&target),
            display_name,
            target,
            separator: false,
            is_folder: false,
            children: Vec::new(),
        },
    )?;
    crate::drop::refresh_apps(app);
    Ok(())
}
