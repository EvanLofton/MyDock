//! 毛玻璃。
//!
//! # 为什么只有一条路线
//!
//! 设计文档原本写了两条路线：
//!
//! * 路线 A —— 官方 DWM 系统背景 `DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW`
//! * 路线 B —— 传统亚克力 `SetWindowCompositionAttribute` + `ACCENT_ENABLE_ACRYLICBLURBEHIND`
//!
//! P0 阶段在**裸 Win32 窗口**上测，路线 A 可用（能采到 76px 的模糊）。但在**真实的
//! Tauri 窗口**上它渲染出来的是一层完全平坦的材质 —— 不采样背后的内容（逐像素剖面
//! 是一条直线，与桌面的对比度为 0）。原因是 WebView2 的子窗口盖住了客户区，
//! DWM 的系统背景只对窗口自身的合成表面生效。
//!
//! 路线 B 不受影响，因为它是由 DWM 在合成整个窗口时做的模糊，跟客户区里有什么无关。
//! 于是路线 A 的代码已经删除 —— 留着一段已知在目标窗口上无效的代码只会误导后来人。
//! 实测数据见 `docs/p1-window-layer-findings.md` §6.6。
//!
//! # 圆角为什么必须交给 DWM
//!
//! 亚克力材质铺满**整个窗口矩形**。想让四角变成圆的，直觉做法是 `SetWindowRgn`
//! 把窗口裁成圆角矩形，但 P0 做过 A/B 对照：加了 `SetWindowRgn` 和没加，截图像素
//! **完全一致** —— DWM 的亚克力无视窗口区域。唯一能让材质跟着形状走的是
//! `DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND`，代价是半径由系统决定（约 8px）。

use std::ffi::c_void;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::*;

/// `DWMWA_USE_IMMERSIVE_DARK_MODE` 没有出现在 `windows` crate 的
/// `DWMWINDOWATTRIBUTE` 常量表里（它长期是"未公开"属性），所以按数值硬写。
const DWMWA_USE_IMMERSIVE_DARK_MODE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(20);

/// `DWMWA_COLOR_NONE`：让 DWM 不画窗口边框。
const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;

// ---------------------------------------------------------------- 亚克力（路线 B）

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    /// `0xAABBGGRR` —— 注意低字节是 R，不是 A。
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WindowCompositionAttributeData {
    attrib: u32,
    pv_data: *mut c_void,
    cb_data: usize,
}

const WCA_ACCENT_POLICY: u32 = 0x13;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

type SwcaFn =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> windows::core::BOOL;

/// `SetWindowCompositionAttribute` 是 user32.dll 的导出（序号 2391），但**不在 Windows
/// SDK 的 user32.lib 导入库里** —— 静态链接必然 LNK2019，只能运行时取地址。
fn swca() -> Option<SwcaFn> {
    use windows::core::s;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
    unsafe {
        let module = LoadLibraryA(s!("user32.dll")).ok()?;
        let addr = GetProcAddress(module, s!("SetWindowCompositionAttribute"))?;
        Some(std::mem::transmute::<_, SwcaFn>(addr))
    }
}

/// 给整窗铺一层亚克力（模糊 + 着色）。
///
/// `rgba` 的 alpha 决定"透出多少背后的内容"，实测标定：
///
/// | alpha | 与桌面的对比度 | 观感 |
/// |-------|---------------|------|
/// | 220   | 25            | 几乎看不见模糊 |
/// | 150   | 85            | 平衡（默认） |
/// | 70    | 153           | 几乎全透 |
///
/// 返回一行诊断字符串，由调用方打印；失败不会 panic，只是窗口没有毛玻璃。
pub fn apply_acrylic(hwnd: HWND, rgba: (u8, u8, u8, u8)) -> String {
    let Some(f) = swca() else {
        return "swca:NOT_FOUND".into();
    };
    let (r, g, b, mut a) = rgba;
    if a == 0 {
        a = 1; // acrylic 不接受 alpha = 0（会整层消失）
    }
    let mut policy = AccentPolicy {
        accent_state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
        accent_flags: 0, // acrylic 用 0；ACCENT_ENABLE_BLURBEHIND 才用 2
        gradient_color: (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24),
        animation_id: 0,
    };
    let mut data = WindowCompositionAttributeData {
        attrib: WCA_ACCENT_POLICY,
        pv_data: &mut policy as *mut _ as *mut c_void,
        cb_data: std::mem::size_of::<AccentPolicy>(),
    };
    let res = unsafe { f(hwnd, &mut data) };
    format!("swca:{}", res.0)
}

// ---------------------------------------------------------------- 窗口材质

/// 设置与毛玻璃配套的窗口材质：DWM 圆角、去掉系统边框、深色。
///
/// 必须在窗口创建之后调用，且**每次重建窗口（比如改 `reserve_taskbar` 后重设尺寸）
/// 不需要重复调用** —— 这些属性跟着 HWND 走，不会因为 `SetWindowPos` 丢失。
pub fn apply_window_material(hwnd: HWND) {
    unsafe {
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const c_void,
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );

        let none = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &none as *const _ as *const c_void,
            std::mem::size_of::<u32>() as u32,
        );

        let dark = windows::core::BOOL::from(true);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const c_void,
            std::mem::size_of::<windows::core::BOOL>() as u32,
        );
    }
}
