//! 设置窗口（后台页面）。
//!
//! # 为什么是**独立窗口**，而不是 Dock 上的一个面板
//!
//! 1. Dock 窗口只有 **72 逻辑像素高**，装不下表单；
//! 2. 更根本的是：Dock 窗口是 `WS_EX_NOACTIVATE` 的**工具窗口，不能获得焦点** ——
//!    这是「点击 Dock 不夺走当前应用焦点」这条验收标准的基础（见 `win_layer.rs`）。
//!    而设置界面要能打字、能拖滑块、能点色板，**必须是普通可聚焦窗口**。
//!
//! 所以设置窗口和 Dock 完全分离：自己的窗口标签 `settings`、自己的页面
//! `settings.html`、自己的样式表。它**不参与** Dock 的焦点防护，也不需要参与 ——
//! 用户点它就是想把焦点给它。
//!
//! 关闭后窗口销毁，下次打开重新创建（页面状态从 Rust 侧重新读，不留脏状态）。

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// 设置窗口的标签。Rust 与前端都靠它找窗口，不要散落字面量。
pub const LABEL: &str = "settings";

/// 打开设置窗口；已经开着就前置并聚焦，不重复创建。
pub fn open(app: &AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return Ok(());
    }

    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("settings.html".into()))
        .title("Dock 设置")
        // 页面是浅色的，标题栏也要浅色 —— 否则系统深色标题栏压在白色页面上很像坏了。
        // （标题栏的深浅由 DWM 决定，CSS 管不到，只能在建窗时声明。）
        .theme(Some(tauri::Theme::Light))
        // 页面有四个分栏，内容比一屏高，滚动是正常的。
        .inner_size(800.0, 760.0)
        .min_inner_size(620.0, 480.0)
        // 普通窗口：有标题栏、可缩放、可聚焦、进任务栏
        .resizable(true)
        .decorations(true)
        .always_on_top(false)
        .skip_taskbar(false)
        .center()
        .build()
        .map_err(|e| format!("创建设置窗口失败: {e}"))?;

    log_info!("[设置] 窗口已打开");
    Ok(())
}
