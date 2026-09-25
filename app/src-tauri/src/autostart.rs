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
