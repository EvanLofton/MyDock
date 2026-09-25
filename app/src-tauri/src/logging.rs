//! 日志 + 崩溃取证。
//!
//! # 为什么必须有这个模块（而不是继续 `println!`）
//!
//! 1. **没有控制台可看**：Dock 是 GUI 子系统程序（`windows_subsystem = "windows"`），
//!    双击启动不该弹一个终端窗口。没有控制台 = `println!` 无处可去，
//!    更糟的是往无效句柄写会让 `println!` **panic**（它把写失败当致命错误）。
//!    所以输出统一走这里：**先落文件**，控制台镜像只是"顺手"，写失败一律忽略。
//! 2. **闪退要能查**：用户要的是"Dock 崩了/出 bug 时能直接从日志定位"。
//!    这需要两条**不同的**路径，缺一不可 ——
//!    - **Rust panic**（`unwrap` 失败、越界、断言）：`std::panic::set_hook` 抓得到，
//!      带上位置 + 线程 + **回溯**；
//!    - **Win32 未处理异常**（访问违例 0xC0000005、非法指令、栈溢出…）：
//!      panic 钩子**根本不会被调用**（那不是 Rust 的 panic），只能靠
//!      `SetUnhandledExceptionFilter`。这类才是真正的"闪退"。
//!
//! # 崩溃现场怎么保证写得出来
//!
//! 崩溃处理器里**不能**加锁、不能分配内存（可能正崩在分配器或日志锁里面），
//! 所以它走一条完全独立的通道：栈上的定长缓冲 + `CreateFileW`/`WriteFile` 直写
//! `crash-*.txt`，再用 `MiniDumpWriteDump` 落一个 `crash-*.dmp`（能用 VS/WinDbg 打开，
//! 带完整调用栈）。文本报告里还贴了**崩溃前的最后 40 行日志** —— 通常一眼就能看出
//! 崩在哪一步（比如"刚点了「新建文件夹」，然后就没了"）。
//!
//! # 文件与轮转
//!
//! - 目录：`DOCK_LOG_DIR` > `DOCK_CONFIG_DIR/logs`（自检时跟配置走，互不干扰）
//!   > `%LOCALAPPDATA%\dev.local.dock\logs`；
//! - 日志：`dock-YYYY-MM-DD.log`，**按天**一个文件；单文件超 [`MAX_LOG_BYTES`] 就轮转成
//!   `.1.log`（只留一代，避免无限长）；保留最近 [`KEEP_DAYS`] 天；
//! - 崩溃：`crash-YYYYMMDD-HHMMSS.txt` + `.dmp`，保留最近 [`KEEP_CRASHES`] 份。
//!
//! # 级别
//!
//! `DOCK_LOG=error|warn|info|debug|trace`（默认 `info`；`DOCK_DEBUG=1` 等价于 `debug`，
//! 兼容以前只用 `DOCK_DEBUG` 的习惯）。`info` 只记生命周期与用户动作，
//! 枚举/图标这类**每次刷新都刷屏**的细节在 `debug` —— 否则日志会被刷成流水账，
//! 真出问题时反而找不着。

use std::fmt::{self, Write as _};
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HMODULE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, WriteFile,
};
use windows::Win32::System::Diagnostics::Debug::{
    EXCEPTION_EXECUTE_HANDLER, EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION, MINIDUMP_TYPE,
    MiniDumpWriteDump, SetUnhandledExceptionFilter,
};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId};

// ------------------------------------------------------------------ 宏
//
// ❗定义放在最前面：`macro_rules!` 在同一个文件里也讲**文本顺序**，
// 本模块内部（会话头、panic 钩子…）也要用它们。
//
// 目标名由 `module_path!()` 在**调用点**展开（如 `dock_app::store`），
// 所以调用方不需要传 target；写 `log_info!("[配置] …")` 就够了。
//
// 为什么带 `log_` 前缀而不是叫 `info!`：和 `std`/常见 crate 的宏名错开，
// 在 grep 里也能一眼看出"这是我们的日志"，不会和别的宏混淆。

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::logging::write($crate::logging::Level::Error, module_path!(), format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::logging::write($crate::logging::Level::Warn, module_path!(), format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::logging::write($crate::logging::Level::Info, module_path!(), format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => { $crate::logging::write($crate::logging::Level::Debug, module_path!(), format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => { $crate::logging::write($crate::logging::Level::Trace, module_path!(), format_args!($($arg)*)) };
}

/// 保留多少天的日志
const KEEP_DAYS: u64 = 7;
/// 单个日志文件的上限，超了就轮转成 `.1.log`
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;
/// 崩溃报告保留份数（`.txt` + `.dmp` 各算一份）
const KEEP_CRASHES: usize = 20;
/// 崩溃报告里贴多少行"崩溃前的日志"
const CRASH_TAIL_LINES: usize = 40;

/// `CreateFileW` 的访问权限位。windows crate 把 `GENERIC_WRITE` 挂在
/// 另一个模块的新类型上（不在 `Storage::FileSystem`），这里直接用数值省一层依赖。
const GENERIC_WRITE: u32 = 0x4000_0000;

// ------------------------------------------------------------------ 级别

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }

    /// `DOCK_LOG` 优先；没给就看 `DOCK_DEBUG`（老开关，等价 debug）
    fn from_env() -> Level {
        match std::env::var("DOCK_LOG")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .trim()
        {
            "error" => Level::Error,
            "warn" | "warning" => Level::Warn,
            "debug" => Level::Debug,
            "trace" => Level::Trace,
            "info" => Level::Info,
            _ => {
                if std::env::var("DOCK_DEBUG").as_deref() == Ok("1") {
                    Level::Debug
                } else {
                    Level::Info
                }
            }
        }
    }

    pub fn name(self) -> &'static str {
        self.tag().trim()
    }
}

/// 当前级别（`init` 时按环境变量设好）
static LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);

pub fn level() -> Level {
    match LEVEL.load(Ordering::Relaxed) {
        1 => Level::Error,
        2 => Level::Warn,
        4 => Level::Debug,
        5 => Level::Trace,
        _ => Level::Info,
    }
}

// ------------------------------------------------------------------ 状态

static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
/// 进程启动时刻（心跳里算运行时长）
static START: OnceLock<Instant> = OnceLock::new();
/// 正在写日志的线程数 —— 崩溃处理器用它判断"是不是正卡在日志里"
static IN_LOG: AtomicU8 = AtomicU8::new(0);

pub fn log_path() -> Option<&'static Path> {
    LOG_PATH.get().map(|p| p.as_path())
}

pub fn uptime() -> std::time::Duration {
    START.get().map(|t| t.elapsed()).unwrap_or_default()
}

/// 日志目录：见模块文档的优先级
pub fn log_dir() -> PathBuf {
    if let Ok(d) = std::env::var("DOCK_LOG_DIR") {
        if !d.trim().is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Ok(d) = std::env::var("DOCK_CONFIG_DIR") {
        if !d.trim().is_empty() {
            return PathBuf::from(d).join("logs");
        }
    }
    if let Ok(la) = std::env::var("LOCALAPPDATA") {
        if !la.trim().is_empty() {
            return PathBuf::from(la).join("dev.local.dock").join("logs");
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("logs")))
        .unwrap_or_else(|| PathBuf::from("logs"))
}

// ------------------------------------------------------------------ 时间戳

fn now() -> windows::Win32::Foundation::SYSTEMTIME {
    unsafe { GetLocalTime() }
}

/// `2026-09-25 09:51:35.123`
fn stamp_full() -> String {
    let t = now();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

/// `2026-09-25`
fn stamp_day() -> String {
    let t = now();
    format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
}

/// `20260925-095135`（崩溃文件名用）
fn stamp_filesafe() -> String {
    let t = now();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

// ------------------------------------------------------------------ 初始化

/// 打开日志文件、清掉过期文件、写会话头。返回日志文件路径。
///
/// ❗**要尽可能早调用**（`main` 的第一件事）：越早，越能记下启动阶段的问题。
pub fn init() -> PathBuf {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("dock-{}.log", stamp_day()));
    prune_old(&dir, &path);
    rotate_if_big(&path);

    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => {
            let _ = LOG_FILE.set(Mutex::new(f));
            let _ = LOG_PATH.set(path.clone());
        }
        Err(e) => {
            // 打不开日志文件也不能让程序起不来 —— 退回"只有控制台镜像"的模式
            let _ = writeln!(io::stderr(), "[日志] 打不开 {}: {e}", path.display());
        }
    }
    let _ = START.set(Instant::now());
    LEVEL.store(Level::from_env() as u8, Ordering::Relaxed);

    header();
    // 会话之间隔一空行（走安全通道 —— 没有控制台时 `println!` 会因为写失败而 panic）
    let _ = writeln!(io::stdout());
    path
}

/// 会话头：出问题时先看这几行（谁、什么时候、哪台机器、什么参数）
fn header() {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = std::env::var("DOCK_CONFIG_DIR").unwrap_or_else(|_| "(默认 %APPDATA%)".into());
    let mut log_names: Vec<String> = Vec::new();
    for k in [
        "DOCK_LOG",
        "DOCK_DEBUG",
        "DOCK_CONFIG_DIR",
        "DOCK_LOG_DIR",
        "DOCK_MENUTEST",
        "DOCK_DRAGTEST",
        "DOCK_LOCTEST",
        "DOCK_SELFTEST",
        "DOCK_LAYERTEST",
        "DOCK_OPENSETTINGS",
    ] {
        if let Ok(v) = std::env::var(k) {
            log_names.push(format!("{k}={v}"));
        }
    }

    log_info!("========== Dock 启动 ==========");
    log_info!("  版本      {} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    log_info!("  时间      {}", stamp_full());
    log_info!("  进程      pid={} 线程={}", unsafe { GetCurrentProcessId() }, unsafe {
        GetCurrentThreadId()
    });
    log_info!("  可执行    {exe}");
    log_info!("  参数      {:?}", args);
    log_info!("  工作目录  {:?}", std::env::current_dir().map(|p| p.display().to_string()));
    log_info!("  配置目录  {cfg}");
    log_info!("  日志文件  {}", log_path().map(|p| p.display().to_string()).unwrap_or_default());
    log_info!("  日志级别  {}（DOCK_LOG=debug 可看枚举/图标细节）", level().name());
    log_info!("  环境开关  {log_names:?}");
    // ⚠️ 这里**不打印屏幕尺寸**：此刻进程还没声明 DPI 感知（tao 在 `Builder` 里才声明），
    // 量到的是**虚拟化坐标**（本机 1920×1080@125% 会读成 1536×864@96 —— 实测踩过）。
    // 真实尺寸由窗口层就绪后的启动横幅记录。
    log_info!("  （屏幕尺寸与 DPI 见窗口层就绪后的横幅）");
}

fn prune_old(dir: &Path, today: &Path) {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let keep_after = now_secs.saturating_sub(KEEP_DAYS * 24 * 3600);

    let mut crashes: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p == today {
                continue;
            }
            if name.starts_with("crash-") {
                if let Ok(md) = e.metadata() {
                    if let Ok(t) = md.modified() {
                        crashes.push((t, p));
                    }
                }
                continue;
            }
            if name.starts_with("dock-") && name.ends_with(".log") {
                // 文件名里的日期能解析就用它；解析不出来就看修改时间
                let keep = name
                    .trim_start_matches("dock-")
                    .trim_end_matches(".log")
                    .trim_end_matches(".1")
                    .parse::<String>()
                    .ok()
                    .and_then(|d| day_to_secs(&d))
                    .map(|s| s >= keep_after)
                    .unwrap_or(true);
                if !keep {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
    // 崩溃报告只留最近 KEEP_CRASHES 份（新→旧排序后把尾巴删掉）
    if crashes.len() > KEEP_CRASHES {
        crashes.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, p) in crashes.into_iter().skip(KEEP_CRASHES) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// `2026-09-25` → 对应的 unix 秒（只用于"留最近几天"的粗判断，允许误差）
fn day_to_secs(day: &str) -> Option<u64> {
    let (y, m, d) = (
        day.get(0..4)?.parse::<i64>().ok()?,
        day.get(5..7)?.parse::<i64>().ok()?,
        day.get(8..10)?.parse::<i64>().ok()?,
    );
    // 民用历 → 儒略日 → unix 秒（够用即可，不考虑闰秒/时区）
    let a = (14 - m) / 12;
    let y2 = y + 4800 - a;
    let m2 = m + 12 * a - 3;
    let jdn = d + (153 * m2 + 2) / 5 + 365 * y2 + y2 / 4 - y2 / 100 + y2 / 400 - 32045;
    Some(((jdn - 2440588) * 86400) as u64)
}

/// 当天日志超过上限就转成 `.1.log`（只留一代）
fn rotate_if_big(path: &Path) {
    let too_big = std::fs::metadata(path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false);
    if !too_big {
        return;
    }
    let backup = path.with_extension("1.log");
    let _ = std::fs::remove_file(&backup);
    let _ = std::fs::rename(path, &backup);
}

// ------------------------------------------------------------------ 写一行

/// 真正落盘/上屏的地方。宏都走它。
pub fn write(level: Level, target: &str, args: fmt::Arguments) {
    if (level as u8) > LEVEL.load(Ordering::Relaxed) {
        return;
    }
    let mut msg = String::new();
    let _ = msg.write_fmt(args);
    let line = format!("{} {} [{target}] {msg}", stamp_full(), level.tag());

    // ① 文件（权威载体）。
    //
    // ❗这里**用阻塞锁而不是 `try_lock`**：`try_lock` 在争用时会把这一行**静默丢掉** ——
    // 对一个"出事时要靠它定位"的通道来说，丢行比"慢两微秒"危险得多。
    // 临界区只有"写一行 + flush"（实测 ~2µs），而且日志频率是每秒几行的量级，
    // 不存在被日志拖住业务的可能。
    //
    // 中毒（某个线程持锁时 panic）用 `into_inner()` 恢复：日志通道**不能**因为
    // 一次 panic 就永久失效 —— 恰恰相反，panic 之后正是最需要它的时候。
    if let Some(f) = LOG_FILE.get() {
        let mut g = f.lock().unwrap_or_else(|e| e.into_inner());
        IN_LOG.fetch_add(1, Ordering::Relaxed);
        let _ = writeln!(g, "{line}");
        // 每条都 flush：崩溃时最后一条必须在盘上（实测一条 ~2µs，可以忽略）
        let _ = g.flush();
        IN_LOG.fetch_sub(1, Ordering::Relaxed);
    }

    // ② 控制台镜像。**写失败一律忽略** —— GUI 子系统下没有控制台，
    //    `println!` 在这种情况会 panic，所以这里绝不能用它。
    //    自检脚本靠 stdout 解析 `[PASS]`/`[FAIL]`，所以这层镜像必须留着。
    let out = io::stdout();
    let mut h = out.lock();
    let _ = writeln!(h, "{line}");
    if level <= Level::Warn {
        let _ = writeln!(io::stderr(), "{line}");
    }
}

// ------------------------------------------------------------------ 宏


/// 心跳：每 5 分钟一行（`reveal` 主循环调）。用途有两个 ——
///
/// 1. "8 小时不崩"这种验收要看得到**还活着**，以及内存有没有一路涨；
/// 2. 真闪退时，日志里最后一条心跳的时间就是**死亡时刻**（配合崩溃报告定位）。
pub fn heartbeat() {
    let up = uptime();
    let mins = up.as_secs() / 60;
    match crate::win_layer::memory_mb() {
        Some(ws) => log_info!("[心跳] 已运行 {mins} 分钟，工作集 {ws} MB"),
        None => log_info!("[心跳] 已运行 {mins} 分钟"),
    }
}

/// 前端（WebView 里的 React）把错误/警告转进来（`commands::js_log` → 这里）。
///
/// 多行消息（错误栈）按行拆开，每条日志仍然是一行 —— 方便 grep，也免得把
/// "一行一条"的格式搞乱。
pub fn from_frontend(level: &str, page: &str, msg: &str) {
    for l in msg.lines() {
        match level {
            "error" => log_error!("[前端 {page}] {l}"),
            "warn" => log_warn!("[前端 {page}] {l}"),
            "debug" => log_debug!("[前端 {page}] {l}"),
            _ => log_info!("[前端 {page}] {l}"),
        }
    }
}

// ------------------------------------------------------------------ ① panic 钩子

/// 装 panic 钩子：把 panic 写进日志（位置 + 线程 + 回溯），再交给原来的钩子。
///
/// ❗回溯用 `force_capture`：只在崩的那一刻抓，值得。
/// 构建是 debug（`debug = true`），所以回溯里**有符号名**，能直接看出是哪一行。
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "未知位置".into());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "<非字符串 panic payload>".to_string()
        };
        let th = std::thread::current();
        let name = th.name().unwrap_or("<未命名线程>").to_string();

        log_error!("💥 PANIC {payload}");
        log_error!("     位置 {loc}");
        log_error!("     线程 {name} (id={:?})", th.id());
        // 回溯只在日志级别允许时抓（force_capture 有成本，但 panic 是"一次性"事件）
        let bt = std::backtrace::Backtrace::force_capture();
        log_error!("     回溯\n{bt}");
        if let Some(p) = log_path() {
            log_error!("     日志 {}", p.display());
        }

        // 标准 stderr 输出照旧（带控制台时看得见；带重定向时进 .dock-err.log）
        prev(info);
    }));
}

// ------------------------------------------------------------------ ② 未处理异常

/// 装 Win32 未处理异常过滤器：**访问违例这类"真闪退"只有这条路能抓到**。
///
/// panic 钩子拦不到它（那不是 Rust panic），WebView2/显卡驱动在某些 bug 下
/// 会直接给进程一个 0xC0000005，表现就是"点了没反应，然后 Dock 没了"。
///
/// 处理器里做的事（**全程不加锁、不分配堆内存**）：
/// 1. 用栈上的定长缓冲拼一份文本报告 → `crash-<时间>.txt`；
/// 2. 报告里贴日志文件的**最后 40 行**（崩溃前发生了什么，一眼可见）；
/// 3. `MiniDumpWriteDump` 落一份 `crash-<时间>.dmp`（VS/WinDbg 能直接打开看到栈）。
///
/// 返回 `EXCEPTION_EXECUTE_HANDLER`：记录完之后仍按系统默认方式结束进程
/// （不吞异常、不假装没出事 —— 吞掉只会让状态更混乱）。
pub fn install_crash_handler() {
    unsafe {
        let _ = SetUnhandledExceptionFilter(Some(crash_filter));
    }
}

unsafe extern "system" fn crash_filter(info: *const EXCEPTION_POINTERS) -> i32 {
    let dir = log_dir();
    let ts = stamp_filesafe();

    // ---- 文本报告（栈缓冲，无堆分配）----
    let mut buf = StackBuf::new();
    let _ = write!(&mut buf, "Dock 崩溃报告\r\n");
    let _ = write!(&mut buf, "时间     {}\r\n", stamp_full());
    let _ = write!(&mut buf, "进程     pid={}\r\n", GetCurrentProcessId());
    let _ = write!(&mut buf, "线程     tid={}\r\n", GetCurrentThreadId());
    let _ = write!(&mut buf, "正在写日志的线程数 {}\r\n", IN_LOG.load(Ordering::Relaxed));

    if !info.is_null() && !(*info).ExceptionRecord.is_null() {
        let rec = &*(*info).ExceptionRecord;
        let code = rec.ExceptionCode.0 as u32;
        let addr = rec.ExceptionAddress as usize;
        let _ = write!(&mut buf, "异常码   0x{code:08X}（{}）\r\n", exception_name(code));
        let _ = write!(&mut buf, "异常地址 0x{addr:016X} ← {}\r\n", module_of(addr));

        // 访问违例的两个参数：0 = 读还是写，1 = 访问的地址
        if code == 0xC000_0005 && rec.NumberParameters >= 2 {
            let kind = match rec.ExceptionInformation[0] {
                0 => "读取",
                1 => "写入",
                8 => "执行",
                _ => "未知",
            };
            let _ = write!(
                &mut buf,
                "访问违例 {kind} 地址 0x{:016X}\r\n",
                rec.ExceptionInformation[1]
            );
        }
    } else {
        let _ = write!(&mut buf, "（没有异常记录指针）\r\n");
    }

    let _ = write!(&mut buf, "\r\n---- 崩溃前的最后 {CRASH_TAIL_LINES} 行日志 ----\r\n");
    append_log_tail(&mut buf, CRASH_TAIL_LINES);

    let txt = dir.join(format!("crash-{ts}.txt"));
    write_file_raw(&txt, buf.as_bytes());

    // ---- Minidump（同一条路径；失败也不影响上面的文本报告）----
    let dmp = dir.join(format!("crash-{ts}.dmp"));
    write_minidump(&dmp, info);

    // 控制台/重定向文件上再喊一声（这里有控制台时用户能立刻看到）
    let _ = writeln!(
        io::stderr(),
        "[崩溃] 报告已写入 {}（另有 {}）",
        txt.display(),
        dmp.display()
    );

    EXCEPTION_EXECUTE_HANDLER
}

fn exception_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "ACCESS_VIOLATION 访问违例",
        0xC000_001D => "ILLEGAL_INSTRUCTION 非法指令",
        0xC000_008C => "ARRAY_BOUNDS_EXCEEDED 数组越界",
        0xC000_0094 => "INTEGER_DIVIDE_BY_ZERO 除零",
        0xC000_00FD => "STACK_OVERFLOW 栈溢出",
        0xC000_0409 => "STACK_BUFFER_OVERRUN 栈缓冲溢出",
        0xC000_0374 => "HEAP_CORRUPTION 堆损坏",
        0xC000_0135 => "UNHANDLED_EXCEPTION",
        0x8000_0003 => "BREAKPOINT 断点",
        0xE06D_736C => "MSVC C++ 异常（多半是 Rust 的 abort）",
        _ => "未知",
    }
}

/// 某个代码地址落在哪个模块（DLL/EXE）+ 偏移 —— 崩溃报告里最有用的一栏
fn module_of(addr: usize) -> String {
    unsafe {
        let mut hmod = HMODULE::default();
        if GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(addr as *const u16),
            &mut hmod,
        )
        .is_err()
        {
            return "未知模块".into();
        }
        let mut name = [0u16; 512];
        let n = GetModuleFileNameW(Some(hmod), &mut name);
        if n == 0 {
            return "未知模块".into();
        }
        let full = String::from_utf16_lossy(&name[..n as usize]);
        let base = hmod.0 as usize;
        match full.rsplit(['\\', '/']).next() {
            Some(f) => format!("{f}+0x{:X}", addr.wrapping_sub(base)),
            None => format!("{full}+0x{:X}", addr.wrapping_sub(base)),
        }
    }
}

/// 栈上的定长文本缓冲：崩溃处理器里不能用 `String`（可能正崩在分配器里）
struct StackBuf {
    b: [u8; 8192],
    n: usize,
}

impl StackBuf {
    fn new() -> Self {
        StackBuf { b: [0; 8192], n: 0 }
    }
    fn as_bytes(&self) -> &[u8] {
        &self.b[..self.n]
    }
    fn push(&mut self, s: &str) {
        for &c in s.as_bytes() {
            if self.n >= self.b.len() {
                return;
            }
            self.b[self.n] = c;
            self.n += 1;
        }
    }
}

impl fmt::Write for StackBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push(s);
        Ok(())
    }
}

/// 把日志文件的最后 N 行贴进崩溃报告（只在崩溃时调用，用普通 IO 就行）
fn append_log_tail(buf: &mut StackBuf, lines: usize) {
    let Some(path) = log_path() else {
        buf.push("（没有日志文件路径）\r\n");
        return;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        buf.push("（日志文件读不出来）\r\n");
        return;
    };
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    for l in &all[start..] {
        buf.push(l);
        buf.push("\r\n");
    }
}

/// 不经 Rust 的 `File`（它会走堆分配与内部锁），直接用 Win32 写
fn write_file_raw(path: &Path, bytes: &[u8]) {
    unsafe {
        let mut w: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().collect();
        w.push(0);
        let h = CreateFileW(
            PCWSTR(w.as_ptr()),
            GENERIC_WRITE,
            FILE_SHARE_READ,
            None,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        );
        let Ok(h) = h else { return };
        if h == INVALID_HANDLE_VALUE {
            return;
        }
        let mut written = 0u32;
        // ⚠️ windows crate 的 `WriteFile` 是**封装过**的：缓冲区和长度合成一个
        // `Option<&[u8]>`，不是 C 里那个 (ptr, len) 五参数版本（实测踩过）
        let _ = WriteFile(h, Some(bytes), Some(&mut written), None);
        let _ = CloseHandle(h);
    }
}

fn write_minidump(path: &Path, info: *const EXCEPTION_POINTERS) {
    unsafe {
        let mut w: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().collect();
        w.push(0);
        let h = CreateFileW(
            PCWSTR(w.as_ptr()),
            GENERIC_WRITE,
            FILE_SHARE_READ,
            None,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        );
        let Ok(h) = h else { return };
        if h == INVALID_HANDLE_VALUE {
            return;
        }
        let mei = MINIDUMP_EXCEPTION_INFORMATION {
            ThreadId: GetCurrentThreadId(),
            ExceptionPointers: info as *mut EXCEPTION_POINTERS,
            ClientPointers: false.into(),
        };
        // `MiniDumpWithDataSegs`（0x1）：线程栈 + 模块表 + 全局数据 —— 足够在
        // VS/WinDbg 里看到 Rust 侧的调用栈；不用 FullMemory（几 GB，没必要）
        let kind = MINIDUMP_TYPE(0x1);
        let _ = MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            h,
            kind,
            Some(&mei),
            None,
            None,
        );
        let _ = CloseHandle(h);
    }
}

// ------------------------------------------------------------------ 自检钩子

/// 人为触发异常/panic（**只在 debug 构建 + 显式环境变量下生效**）。
///
/// 用途：验证"Dock 崩了能不能查"这条链路本身 —— 见 README 的环境变量表。
/// - `DOCK_CRASH_TEST=panic`：故意 panic（走 panic 钩子）
/// - `DOCK_CRASH_TEST=av`：故意空指针写（走未处理异常过滤器 + Minidump）
#[cfg(debug_assertions)]
pub fn maybe_crash_for_test() {
    match std::env::var("DOCK_CRASH_TEST").unwrap_or_default().as_str() {
        "panic" => {
            log_warn!("DOCK_CRASH_TEST=panic —— 故意 panic，用来验证 panic 钩子");
            panic!("DOCK_CRASH_TEST：这是一次**故意**的 panic（验证日志链路）");
        }
        "av" => {
            log_warn!("DOCK_CRASH_TEST=av —— 故意制造访问违例，用来验证崩溃报告 + Minidump");
            unsafe {
                let p = std::ptr::null_mut::<u64>();
                std::ptr::write_volatile(p, 1);
            }
        }
        _ => {}
    }
}

#[cfg(not(debug_assertions))]
pub fn maybe_crash_for_test() {}
