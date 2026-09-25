//! 开机自启。
//!
//! 直接写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，
//! 不用 `tauri-plugin-autostart`（本机 cargo 缓存里没有，离线装不上）。
//! 用 HKCU 而不是 HKLM：不需要管理员权限，且只影响当前用户。

#![allow(dead_code)]

use windows::core::PCWSTR;
use windows::Win32::System::Registry::*;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Dock";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_run_key(write: bool) -> Option<HKEY> {
    let sub = wide(RUN_KEY);
    let mut key = HKEY::default();
    let access = if write {
        KEY_SET_VALUE | KEY_QUERY_VALUE
    } else {
        KEY_QUERY_VALUE
    };
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), None, access, &mut key) };
    if rc.0 == 0 { Some(key) } else { None }
}

/// 当前是否已登记开机自启，并返回登记的路径
pub fn current_entry() -> Option<String> {
    let key = open_run_key(false)?;
    let name = wide(VALUE_NAME);
    let mut buf = [0u8; 1024];
    let mut len = buf.len() as u32;
    let mut ty = REG_VALUE_TYPE::default();
    let rc = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        )
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if rc.0 != 0 {
        return None;
    }
    let s = String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(buf.as_ptr() as *const u16, (len as usize) / 2)
    });
    Some(s.trim_end_matches('\0').to_string())
}

pub fn is_enabled() -> bool {
    current_entry().is_some()
}

pub fn enable(exe_path: &str) -> Result<(), String> {
    let key = open_run_key(true).ok_or("无法打开 HKCU Run 注册表项")?;
    let name = wide(VALUE_NAME);
    // 路径可能含空格，按惯例加引号
    let val = wide(&format!("\"{exe_path}\""));
    let bytes = unsafe {
        std::slice::from_raw_parts(val.as_ptr() as *const u8, val.len() * 2)
    };
    let rc = unsafe {
        RegSetValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            REG_SZ,
            Some(bytes),
        )
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if rc.0 == 0 {
        Ok(())
    } else {
        Err(format!("写入注册表失败 (code={})", rc.0))
    }
}

pub fn disable() -> Result<(), String> {
    let Some(key) = open_run_key(true) else {
        return Ok(());
    };
    let name = wide(VALUE_NAME);
    let rc = unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) };
    unsafe {
        let _ = RegCloseKey(key);
    }
    // 值本来就不存在也算成功
    if rc.0 == 0 || rc.0 == 2 {
        Ok(())
    } else {
        Err(format!("删除注册表值失败 (code={})", rc.0))
    }
}

/// 当前进程的可执行文件全路径
pub fn current_exe() -> Option<String> {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

/// 去掉注册表值两边的引号（`enable` 按惯例是带引号写的）。
fn unquote(v: &str) -> &str {
    v.trim().trim_matches('"')
}

/// **启动对账**的纯判断（不碰注册表，便于单测）。
///
/// 返回 `None` = 一致、什么都不用做；`Some(true)` = 补登记；`Some(false)` = 删掉登记。
///
/// # 为什么需要它（2026-09 用户机器实测）
///
/// `config.json` 里的 `launch_at_login` 是**用户的意图**，`HKCU\...\Run\Dock` 是**事实**。
/// 两者会被"程序之外的动作"弄脱节：**卸载旧版本会删掉那个 Run 项**，而配置里的开关还是
/// `true` —— 于是重装之后设置页/托盘都显示"开机自启：开"，**实际根本不会自启**。
/// 根因是程序只在"开关被改动时"才写注册表（`commands::set_preferences`），
/// 启动路径上没人核对过。
///
/// 顺带也修掉"登记的路径不是当前 exe"这种脱节（换过安装目录、或曾经被开发构建登记过）。
pub fn reconcile_action(want: bool, registered: Option<&str>, exe: Option<&str>) -> Option<bool> {
    match (want, registered) {
        // 关着、也没登记 —— 绝大多数启动走这条，一个注册表写操作都没有
        (false, None) => None,
        // 用户关了、但还登记着 → 删掉（否则下次登录还会起来）
        (false, Some(_)) => Some(false),
        // 想自启、但没登记 → 补上
        (true, None) => Some(true),
        // 登记着，但**路径不是当前 exe** → 重写成当前路径
        (true, Some(v)) => match exe {
            Some(e) if unquote(v) != e => Some(true),
            // 自身路径都取不到时**不动**：宁可不动，也别把一个取不到路径的登记写进去
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::reconcile_action;

    const EXE: &str = r"D:\MyDock\dock-app.exe";
    const OLD: &str = r"C:\Program1\Projects\Dock\app\src-tauri\target\release\dock-app.exe";

    #[test]
    fn nothing_to_do_when_the_switch_is_off_and_nothing_is_registered() {
        // 最常见的启动：关着、没登记 —— 必须是"什么都不做"（不要每次启动都写一遍注册表）
        assert_eq!(reconcile_action(false, None, Some(EXE)), None);
    }

    #[test]
    fn re_registers_after_a_reinstall_wiped_the_run_entry() {
        // 用户机器上真实发生的那一次：配置说"要自启"，卸载旧版把 Run 项删了
        assert_eq!(reconcile_action(true, None, Some(EXE)), Some(true));
    }

    #[test]
    fn removes_a_stale_entry_when_the_switch_is_off() {
        let reg = format!("\"{EXE}\"");
        assert_eq!(reconcile_action(false, Some(reg.as_str()), Some(EXE)), Some(false));
    }

    #[test]
    fn rewrites_when_the_registered_path_is_not_this_exe() {
        // 换过安装目录 / 曾被开发构建登记过
        let quoted_old = format!("\"{OLD}\"");
        assert_eq!(
            reconcile_action(true, Some(quoted_old.as_str()), Some(EXE)),
            Some(true)
        );
        // 没引号的登记（别的工具写的）也要认得
        assert_eq!(reconcile_action(true, Some(OLD), Some(EXE)), Some(true));
    }

    #[test]
    fn leaves_a_correct_entry_alone() {
        let ok = format!("\"{EXE}\"");
        assert_eq!(reconcile_action(true, Some(ok.as_str()), Some(EXE)), None);
        // 带首尾空白的登记也算一致（读注册表时本来就 trim 过）
        let padded = format!("  \"{EXE}\"  ");
        assert_eq!(reconcile_action(true, Some(padded.as_str()), Some(EXE)), None);
    }

    #[test]
    fn does_nothing_when_own_path_is_unavailable() {
        // 取不到自身路径：宁可不动 —— 别把登记改成空字符串
        let quoted_old = format!("\"{OLD}\"");
        assert_eq!(
            reconcile_action(true, Some(quoted_old.as_str()), None),
            None
        );
    }
}
