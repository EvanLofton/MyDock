//! R15 自检：**同一进程内**点击 Dock 是否夺走焦点。
//!
//! ## ❗关键教训（曾导致我误判「环境不支持合成点击」）
//!
//! 测试用的辅助窗口**必须由跑消息循环的线程创建**。
//! 早期版本在后台线程里 `CreateWindowExW`，而那个线程没有消息循环 ——
//! 点击产生的是**投递**消息，进了队列却无人 `DispatchMessage`，
//! 于是 `WM_LBUTTONDOWN` 永远是 0，看起来就像「SendInput 没生效 / 环境不支持」。
//!
//! 正确切分：
//! - **主线程**：创建辅助窗口（主线程跑着 tao 的事件循环，会派发消息）
//! - **后台线程**：驱动测试（要 `sleep`，不能占用主线程）

use std::sync::atomic::Ordering;
use std::time::Duration;

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{HBRUSH, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::win_layer::{
    BLOCK_MOUSEACTIVATE, MOUSEACTIVATE_COUNT, describe_ex_style,
    top_level_window_at, window_rect,
};

static PLAIN_PROC_HITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static PLAIN_CLICKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

unsafe extern "system" fn plain_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    PLAIN_PROC_HITS.fetch_add(1, Ordering::SeqCst);
    match msg {
        WM_LBUTTONDOWN => {
            PLAIN_CLICKS.fetch_add(1, Ordering::SeqCst);
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let _ = ValidateRect(Some(hwnd), None);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hinst() -> HINSTANCE {
    HINSTANCE(unsafe { GetModuleHandleW(None).unwrap_or_default() }.0)
}

fn register(class: &str) -> Vec<u16> {
    let c = wide(class);
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(plain_proc),
            hInstance: hinst(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH::default(),
            lpszClassName: PCWSTR(c.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
    }
    c
}

fn describe(hwnd: HWND) -> String {
    format!("0x{:X} '{}'", hwnd.0 as usize, crate::win_layer::title_of(hwnd))
}

fn force_foreground(target: HWND) {
    unsafe {
        let fg = GetForegroundWindow();
        let fg_tid = if fg.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(fg, None)
        };
        let our_tid = GetCurrentThreadId();
        let attached = fg_tid != 0 && fg_tid != our_tid;
        if attached {
            let _ = AttachThreadInput(fg_tid, our_tid, true);
        }
        let _ = ShowWindow(target, SW_SHOW);
        let _ = BringWindowToTop(target);
        let _ = SetForegroundWindow(target);
        if attached {
            let _ = AttachThreadInput(fg_tid, our_tid, false);
        }
    }
}

/// 绝对坐标移动 + 按下 + 抬起，一次性批量注入
fn click_at(x: i32, y: i32) -> u32 {
    unsafe {
        let _ = SetCursorPos(x, y);
        std::thread::sleep(Duration::from_millis(80));
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2);
        let ax = (((x - vx) as i64 * 65535) / (vw as i64 - 1)) as i32;
        let ay = (((y - vy) as i64 * 65535) / (vh as i64 - 1)) as i32;
        let mk = |flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let inputs = [
            mk(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                ax,
                ay,
            ),
            mk(MOUSEEVENTF_LEFTDOWN, 0, 0),
            mk(MOUSEEVENTF_LEFTUP, 0, 0),
        ];
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32)
    }
}

fn center(hwnd: HWND) -> (i32, i32) {
    let r = window_rect(hwnd);
    ((r.left + r.right) / 2, (r.top + r.bottom) / 2)
}

// ---------------------------------------------------------------- 拖拽可行性实验

/// **BL-4 的前置实验**：把图标往上拖出 Dock 窗口之后，WebView2 还收不收 pointer 事件？
///
/// 为什么必须先做：Dock 窗口只有 **72 逻辑像素高**。如果要做「拖出 Dock = 删除图标」，
/// 就必须在指针离开窗口后继续拿到坐标 —— 否则拖到一半事件就断了，功能根本没法做。
/// 备选方案（拖拽期间临时把窗口加高）代价很大：透明的窗口区域会**吞掉桌面点击**
/// （窗口透明 ≠ 点击穿透，README「已知限制」记过这个取舍）。
///
/// 做法：**运行时用 `eval` 注入**探针（不改产品代码），然后在真实窗口上合成一次
/// 鼠标按下 + 向上拖 200 逻辑像素 + 抬起，把页面收到的每一个 pointer 坐标回传。
///
/// 判据：如果回传的 `clientY` 出现**负值**（窗口上方），说明指针已离开窗口
/// 而事件仍在送达 → `setPointerCapture` 在这类窗口里可用 ✅。
pub fn drive_drag_probe(app: &tauri::AppHandle) {
    use tauri::Manager;

    log_info!("\n============= 拖拽可行性实验（BL-4 前置）=============");

    let Some(win) = app.get_webview_window("dock") else {
        log_info!("  找不到 Dock 窗口");
        return;
    };
    let Ok(raw) = win.hwnd() else {
        log_info!("  取不到 HWND");
        return;
    };
    let dock = HWND(raw.0 as *mut core::ffi::c_void);
    let rect = window_rect(dock);
    let scale = crate::win_layer::dpi_of(dock) as f64 / 96.0;
    log_info!(
        "  Dock 窗口: ({},{}) {}x{} 物理  缩放 {scale:.2}",
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top
    );

    // 注入探针：不改产品代码，纯运行时挂监听
    let _ = crate::commands::take_test_report();
    let probe = r#"(function () {
  const row = document.querySelector('.dock-row') || document.body;
  const log = [];
  const rep = (m) => window.__TAURI_INTERNALS__.invoke('selftest_report', { msg: String(m) });
  row.addEventListener('pointerdown', function onDown(e) {
    try { row.setPointerCapture(e.pointerId); } catch (err) { log.push('capture-fail'); }
    log.push('down ' + Math.round(e.clientX) + ',' + Math.round(e.clientY));
    const mv = (ev) => log.push('mv ' + Math.round(ev.clientX) + ',' + Math.round(ev.clientY));
    const up = (ev) => {
      log.push('up ' + Math.round(ev.clientX) + ',' + Math.round(ev.clientY));
      row.removeEventListener('pointermove', mv);
      row.removeEventListener('pointerup', up);
      rep('ok ' + log.join(' | '));
    };
    const cancel = (ev) => { log.push('cancel'); rep('cancel ' + log.join(' | ')); };
    row.addEventListener('pointermove', mv);
    row.addEventListener('pointerup', up);
    row.addEventListener('pointercancel', cancel);
  }, { once: true });
  rep('armed');
})();"#;
    if let Err(e) = win.eval(probe) {
        log_info!("  注入探针失败: {e}");
        return;
    }
    // 等 "armed"
    let mut armed = false;
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(100));
        if crate::commands::take_test_report() == "armed" {
            armed = true;
            break;
        }
    }
    if !armed {
        log_info!("  探针未就绪 ❌");
        return;
    }
    log_info!("  探针已就绪，开始合成拖拽…");

    // 从 Dock 中间一个图标位置按下，向上拖 200 逻辑像素（远超 72 高的窗口）
    let start_x = (rect.left + rect.right) / 2;
    let start_y = rect.bottom - (30.0 * scale) as i32; // 图标行附近
    let up_px = (200.0 * scale) as i32;
    unsafe {
        let _ = SetCursorPos(start_x, start_y);
    }
    std::thread::sleep(Duration::from_millis(150));

    let n = drag_from_to(start_x, start_y, start_x, start_y - up_px, 12);
    log_info!("  合成拖拽: ({start_x},{start_y}) -> ({start_x},{}) 注入 {n} 段", start_y - up_px);

    let mut report = String::new();
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        let r = crate::commands::take_test_report();
        if !r.is_empty() {
            report = r;
            break;
        }
    }
    log_info!("  页面回执: {}", if report.is_empty() { "<超时>" } else { &report });

    // 判读：clientY 是否有负值（= 指针到了窗口上方，事件仍在送达）
    let window_h_logical = ((rect.bottom - rect.top) as f64 / scale).round() as i32;
    let min_y = report
        .split("mv ")
        .skip(1)
        .filter_map(|s| s.split(',').nth(1))
        .filter_map(|s| s.split(|c: char| !c.is_ascii_digit() && c != '-').next())
        .filter_map(|s| s.parse::<i32>().ok())
        .min();
    match min_y {
        Some(y) if y < 0 => log_info!(
            "  -> 结论：**事件在窗口外继续送达** ✅（最小 clientY={y}，窗口高 {window_h_logical} 逻辑像素）"
        ),
        Some(y) => log_info!(
            "  -> 结论：事件**没有**离开窗口 ❌（最小 clientY={y}，窗口高 {window_h_logical}）—— 拖出删除需要换方案"
        ),
        None => log_info!("  -> 结论：没拿到坐标，判读失败 ⚠️"),
    }
    log_info!("====================================================\n");
}

// ---------------------------------------------------------------- Dock 内拖拽

/// Dock 内拖拽的端到端自检：**拖拽排序** 与 **拖出删除**。
///
/// 为什么能自动测：`drive_drag_probe` 已经证明合成鼠标事件能驱动页面的 pointer 流程，
/// 所以这里可以直接「按下 → 移动 → 松开」并断言**落盘结果**（`config.json` 里的顺序）。
/// 判据用持久化的结果而不是界面截图 —— 界面观感仍需人工看一眼。
///
/// 顺带验证需求「拖完松开不能触发启动」：全程盯着被拖的那个应用**有没有被拉起来**。
///
/// 测试结束会把顺序和列表**完整还原**。
pub fn drive_drag_test(app: &tauri::AppHandle) {
    use tauri::Manager;

    if !require_temp_config("Dock 内拖拽自检") {
        return;
    }

    log_info!("\n============= Dock 内拖拽自检 =============");

    let list = || -> Vec<crate::model::PinnedApp> {
        app.state::<crate::store::PrefsState>()
            .0
            .lock()
            .unwrap()
            .pinned
            .clone()
    };
    let original = list();
    if original.len() < 4 {
        log_info!("  固定项少于 4 个（{}），跳过拖拽用例", original.len());
        return;
    }
    let mut pass = 0;
    let mut fail = 0;
    let mut check = |ok: bool, what: &str, detail: String| {
        if ok {
            pass += 1;
            log_info!("  [PASS] {what}  {detail}");
        } else {
            fail += 1;
            log_info!("  [FAIL] {what}  {detail}");
        }
    };

    let Some(win) = app.get_webview_window("dock") else {
        log_info!("  找不到 Dock 窗口");
        return;
    };
    let Ok(raw) = win.hwnd() else { return };
    let dock = HWND(raw.0 as *mut core::ffi::c_void);
    let rect = window_rect(dock);
    let scale = crate::win_layer::dpi_of(dock) as f64 / 96.0;

    // 向页面要每个图标的中心（client 坐标 = 窗口内逻辑像素）
    let centers = match icon_centers(&win, &rect, scale) {
        Some(c) if c.len() == original.len() => c,
        Some(c) => {
            log_info!("  图标数({})与固定项数({})不符，跳过", c.len(), original.len());
            let _ = c;
            return;
        }
        None => {
            log_info!("  取图标位置失败，跳过");
            return;
        }
    };
    log_info!("  取到 {} 个图标位置", centers.len());

    // ---- 用例 1：把某个图标拖到别的槽位 ----
    //
    // ❗拖拽源必须选一个**当前没在运行**的图标 —— 「松手有没有把它拉起来」才是有效断言。
    // 拿文件资源管理器试是测不出来的：explorer.exe 一直在跑，断言恒为真（第一版就栽在这）。
    let running_ids: Vec<String> = crate::apps::enumerate_apps()
        .iter()
        .map(|a| a.id.clone())
        .collect();
    let pick = original.iter().position(|p| !running_ids.contains(&p.id));
    let from = pick.unwrap_or(0);
    let to = if from + 3 < original.len() { from + 3 } else { 0 };
    let (x0, y0) = centers[from];
    let (x1, y1) = centers[to];
    // 竖直不移动，只横向拖 —— 这样不会误入「拖出去」区域
    let n = drag_from_to(x0, y0, x1, y1, 10);
    std::thread::sleep(Duration::from_millis(700));

    let after_drag = list();
    let mut expect = original.clone();
    let moved = expect.remove(from);
    expect.insert(to, moved);
    let order_ok = after_drag.iter().map(|p| &p.id).eq(expect.iter().map(|p| &p.id));
    check(
        order_ok && n > 0,
        "拖拽排序：图标被移到目标槽位并落盘",
        format!(
            "把 '{}' 从 {from} 拖到 {to}；注入 {n} 段；期望 {to} 位='{}'，实得='{}'",
            original[from].display_name,
            expect[to].display_name,
            after_drag.get(to).map(|p| p.display_name.clone()).unwrap_or_default()
        ),
    );

    // 需求「拖完松开不能触发启动」
    let dragged_id = original[from].id.clone();
    if pick.is_some() {
        let launched = crate::apps::enumerate_apps().iter().any(|a| a.id == dragged_id);
        check(
            !launched,
            "拖完松开**没有**触发启动",
            format!("被拖的 '{}'（拖前未运行）现在也没运行 = {}", original[from].display_name, !launched),
        );
    } else {
        log_info!("  （所有图标都在运行，无法验证「松手不启动」——explorer 之类恒在运行）");
    }

    // ---- 用例：拖到面板外时必须给出**文字**提示；拖回面板内松手不应删除 ----
    {
        let cur = list();
        let idx = cur.len() - 1;
        let (hx, hy) = centers[idx];
        // 要越过 DELETE_MARGIN(26) 才算「拖出去了」，所以要按**起点**算行程：
        // 图标中心的 clientY 大约 42，只往上拖 60 逻辑像素只到 -18，根本进不了删除区
        // （第一版就是这么写的，于是断言拿到的是 <none>）。
        let start_client_y = ((hy - rect.top) as f64 / scale).round() as i32;
        let up = (((start_client_y + 26 + 30) as f64) * scale) as i32;
        let mut n = mouse_abs(hx, hy);
        std::thread::sleep(Duration::from_millis(120));
        n += mouse_down();
        std::thread::sleep(Duration::from_millis(80));
        for k in 1..=6 {
            n += mouse_abs(hx, hy - up * k / 6);
            std::thread::sleep(Duration::from_millis(40));
        }
        // 指针此刻在面板上方 —— 这时页面**必须**已经显示出提示文字
        let hint = read_hint(&win);
        // 再拖回面板内松手：既避免误删，又顺带验证「回到里面就不该删」
        n += mouse_abs(hx, hy);
        std::thread::sleep(Duration::from_millis(80));
        n += mouse_up();
        std::thread::sleep(Duration::from_millis(600));

        check(
            hint.contains("松开即移除") && hint.contains(&cur[idx].display_name),
            "拖出面板时有文字提示，且写明移除的是哪一个",
            format!("实得提示='{hint}'（注入 {n} 段）"),
        );
        let back_ids: Vec<String> = list().iter().map(|p| p.id.clone()).collect();
        let cur_ids: Vec<String> = cur.iter().map(|p| p.id.clone()).collect();
        check(
            back_ids == cur_ids,
            "拖出去又拖回面板内松手：**不**删除也不改顺序",
            format!("{} 项，列表一致 = {}", back_ids.len(), back_ids == cur_ids),
        );
    }

    // ---- 用例 2：把最后一个图标拖到面板上方 → 松手移除 ----
    let cur = list();
    let last_idx = cur.len() - 1;
    let removed_name = cur[last_idx].display_name.clone();
    let (lx, ly) = centers[last_idx];
    // 竖直向上拖到面板上方 120 逻辑像素处（远超 DELETE_MARGIN=26）
    let up = (120.0 * scale) as i32;
    let n2 = drag_from_to(lx, ly, lx, ly - up, 10);
    std::thread::sleep(Duration::from_millis(700));

    let after_remove = list();
    // 按 **id** 判"移除了没有"，不能按显示名：用户完全可能有多个同名条目
    // （实测：桌面上四个「新建文件夹」，按名字判会永远报红）。
    let removed_id = cur[last_idx].id.clone();
    check(
        after_remove.len() == cur.len() - 1
            && !after_remove.iter().any(|p| p.id == removed_id),
        "拖出 Dock 松手 = 从 Dock 移除",
        format!(
            "注入 {n2} 段；'{}' 移除后剩 {} 项",
            removed_name,
            after_remove.len()
        ),
    );

    // ---- 用例 3：顺序集合校验（错误顺序必须被拒绝且不写盘）----
    let before_bad = list();
    let mut bad: Vec<String> = before_bad.iter().map(|p| p.id.clone()).collect();
    bad.pop(); // 故意少一个
    let rejected = crate::store::reorder(app, &bad).is_err();
    let unchanged = list().len() == before_bad.len();
    check(
        rejected && unchanged,
        "非法顺序被拒绝，配置不变",
        format!("少一项时报错={rejected} 列表未变={unchanged}"),
    );

    // ---- 还原 ----
    //
    // ⚠️ 顺序必须是「先补回被删的，再整体重排」。
    // 反过来会失败：reorder 要求 id 集合完全一致，而被删的那项还没补回来（
    // 第一版就写反了，于是还原后它被 append 到末尾，位置不对）。
    for p in &original {
        if !list().iter().any(|x| x.id == p.id) {
            let _ = crate::store::pin(app, p.clone());
        }
    }
    let ids: Vec<String> = original.iter().map(|p| p.id.clone()).collect();
    let restore_order = crate::store::reorder(app, &ids);
    let final_ids: Vec<String> = list().iter().map(|p| p.id.clone()).collect();
    check(
        restore_order.is_ok() && final_ids == ids,
        "测试结束后顺序与列表已还原",
        format!(
            "{} 项，顺序一致 = {}",
            final_ids.len(),
            final_ids == ids
        ),
    );

    log_info!("  小结：{pass} 项通过，{fail} 项失败");
    log_info!("==========================================\n");
}

/// 在指定 webview 里执行一段表达式并把结果回传成字符串。
///
/// 用 `selftest_report` 这条既有通道回传 —— 和 `drive_ipc_test` 一样，
/// 不依赖 window title 之类的间接通道。
fn eval_in(win: &tauri::WebviewWindow, expr: &str) -> String {
    let _ = crate::commands::take_test_report();
    let js = format!(
        r#"(function () {{
  try {{
    const v = ({expr});
    window.__TAURI_INTERNALS__.invoke('selftest_report', {{ msg: 'v:' + String(v) }});
  }} catch (e) {{
    window.__TAURI_INTERNALS__.invoke('selftest_report', {{ msg: 'v:<err ' + e + '>' }});
  }}
}})();"#
    );
    if win.eval(&js).is_err() {
        return "<eval 失败>".into();
    }
    for _ in 0..25 {
        std::thread::sleep(Duration::from_millis(100));
        let r = crate::commands::take_test_report();
        if !r.is_empty() {
            return r.strip_prefix("v:").unwrap_or(&r).to_string();
        }
    }
    "<超时>".into()
}

/// 读当前是否显示了拖拽提示文字（`.dock-hint`）。
fn read_hint(win: &tauri::WebviewWindow) -> String {
    let _ = crate::commands::take_test_report();
    let js = r#"(function () {
  const el = document.querySelector('.dock-hint');
  window.__TAURI_INTERNALS__.invoke('selftest_report', {
    msg: 'hint:' + (el ? el.textContent : '<none>')
  });
})();"#;
    if win.eval(js).is_err() {
        return "<eval 失败>".into();
    }
    for _ in 0..25 {
        std::thread::sleep(Duration::from_millis(100));
        let r = crate::commands::take_test_report();
        if !r.is_empty() {
            return r.strip_prefix("hint:").unwrap_or(&r).to_string();
        }
    }
    "<超时>".into()
}

/// 向页面要各图标的中心点，换算成**屏幕物理坐标**。
fn icon_centers(
    win: &tauri::WebviewWindow,
    rect: &windows::Win32::Foundation::RECT,
    scale: f64,
) -> Option<Vec<(i32, i32)>> {
    let _ = crate::commands::take_test_report();
    let js = r#"(function () {
  const els = [...document.querySelectorAll('.dock-icon')];
  const cs = els.map(function (el) {
    const r = el.getBoundingClientRect();
    return Math.round(r.left + r.width / 2) + ':' + Math.round(r.top + r.height / 2);
  });
  window.__TAURI_INTERNALS__.invoke('selftest_report', { msg: 'centers ' + cs.join(' ') });
})();"#;
    win.eval(js).ok()?;
    let mut report = String::new();
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        let r = crate::commands::take_test_report();
        if !r.is_empty() {
            report = r;
            break;
        }
    }
    let body = report.strip_prefix("centers ")?;
    let mut out = Vec::new();
    for tok in body.split_whitespace() {
        let (x, y) = tok.split_once(':')?;
        let cx: f64 = x.parse().ok()?;
        let cy: f64 = y.parse().ok()?;
        out.push((
            rect.left + (cx * scale).round() as i32,
            rect.top + (cy * scale).round() as i32,
        ));
    }
    Some(out)
}

// ---- 鼠标注入的三个基本动作（拆开是为了能在拖拽中途做检查）----

fn mouse_abs(x: i32, y: i32) -> u32 {
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2);
        let ax = (((x - vx) as i64 * 65535) / (vw as i64 - 1)) as i32;
        let ay = (((y - vy) as i64 * 65535) / (vh as i64 - 1)) as i32;
        mouse_input(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, ax, ay)
    }
}

fn mouse_down() -> u32 {
    mouse_input(MOUSEEVENTF_LEFTDOWN, 0, 0)
}

fn mouse_up() -> u32 {
    mouse_input(MOUSEEVENTF_LEFTUP, 0, 0)
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32) -> u32 {
    // ⚠️ `SendInput` 的第二个参数是**单个 INPUT 结构的大小**，不是数组长度。
    // 传错（比如传 1）会让它整个失败并返回 0 —— 表现就是「注入 0 段」。
    let cb = std::mem::size_of::<INPUT>() as i32;
    let inp = [INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }];
    unsafe { SendInput(&inp, cb) }
}

/// 按下 → 分 `steps` 段移动到终点 → 抬起。返回注入的事件段数。
fn drag_from_to(x0: i32, y0: i32, x1: i32, y1: i32, steps: i32) -> u32 {
    let mut n = mouse_abs(x0, y0);
    std::thread::sleep(Duration::from_millis(120));
    n += mouse_down();
    std::thread::sleep(Duration::from_millis(80));
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let x = x0 + ((x1 - x0) as f64 * t) as i32;
        let y = y0 + ((y1 - y0) as f64 * t) as i32;
        n += mouse_abs(x, y);
        std::thread::sleep(Duration::from_millis(30));
    }
    n += mouse_up();
    n
}

/// 必须由**主线程**调用（见文件头说明）
pub struct TestWindows {
    pub settings: HWND,
    /// 带 `WS_EX_NOACTIVATE` 的假 Dock（应与真 Dock 行为一致）
    pub fake: HWND,
    /// **无任何保护**的假 Dock —— 真正的阳性对照，用来证明测试能测出「夺焦点」
    pub fake_open: HWND,
}

pub fn create_windows() -> Option<TestWindows> {
    unsafe {
        let cs = register("DockSelfTestSettings");
        let settings = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(cs.as_ptr()),
            windows::core::w!("SETTINGS-HOLDER"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            120,
            120,
            420,
            300,
            None,
            None,
            Some(hinst()),
            None,
        )
        .ok()?;

        let cf = register("DockSelfTestFakeDock");
        let fake = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR(cf.as_ptr()),
            windows::core::w!("FAKE-DOCK"),
            WS_POPUP | WS_VISIBLE,
            200,
            500,
            500,
            90,
            None,
            None,
            Some(hinst()),
            None,
        )
        .ok()?;

        // 无保护的对照窗：**故意不加** WS_EX_NOACTIVATE
        let co = register("DockSelfTestOpenDock");
        let fake_open = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(co.as_ptr()),
            windows::core::w!("OPEN-DOCK"),
            WS_POPUP | WS_VISIBLE,
            1200,
            500,
            500,
            90,
            None,
            None,
            Some(hinst()),
            None,
        )
        .ok()?;

        Some(TestWindows {
            settings,
            fake,
            fake_open,
        })
    }
}

/// 找一个**已在运行的、属于其它进程**的稳定顶层窗口当焦点持有者。
///
/// 早期版本用「spawn 一个 helper 进程」的办法，但那个 helper 生命周期不稳
/// （实测焦点会莫名跳到 Explorer），得出的结论不可信。
/// 改用已存在的系统窗口，稳定得多。
fn external_holder() -> Option<HWND> {
    // 按稳定性排序：资源管理器文件窗口 → 桌面窗口
    for cls in ["CabinetWClass", "Progman"] {
        let c = wide(cls);
        let h = unsafe { FindWindowW(PCWSTR(c.as_ptr()), None) }.unwrap_or_default();
        if h.0.is_null() {
            continue;
        }
        // 确认它确实是别的进程的
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(h, Some(&mut pid));
        }
        if pid != 0 && pid != std::process::id() {
            return Some(h);
        }
    }
    None
}

/// 在后台线程驱动测试
pub fn drive(dock: HWND, w: &TestWindows) {
    log_info!("\n============= R15 自检：点击是否夺焦点 =============");
    log_info!(
        "真 Dock      : {}  exstyle={}",
        describe(dock),
        describe_ex_style(unsafe { GetWindowLongPtrW(dock, GWL_EXSTYLE) as u32 })
    );
    log_info!("同进程焦点窗 : {}", describe(w.settings));
    log_info!("正对照假 Dock: {}", describe(w.fake));

    let ext = external_holder();
    match ext {
        Some(h) => log_info!("跨进程焦点窗 : {}  (独立进程)", describe(h)),
        None => log_info!("跨进程焦点窗 : 未提供（设 DOCK_HOLDER_EXE 指向 probe.exe 可启用）"),
    }

    let mut orig = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut orig);
    }

    // (标签, 目标窗口, 焦点持有者, 是否拦截 WM_MOUSEACTIVATE, 期望是否夺焦点)
    let mut phases: Vec<(&str, HWND, HWND, bool, bool)> = vec![
        (
            "[P0] 同进程 无保护假Dock（阳性对照，期望夺焦点）",
            w.fake_open,
            w.settings,
            true,
            true,
        ),
        (
            "[A]  同进程 真 Dock（已装钩子）",
            dock,
            w.settings,
            true,
            false,
        ),
        (
            "[B]  同进程 真 Dock（未装钩子）",
            dock,
            w.settings,
            false,
            false,
        ),
    ];
    if let Some(h) = ext {
        phases.push((
            "[C]  跨进程 真 Dock（真实工况，期望不夺）",
            dock,
            h,
            true,
            false,
        ));
        phases.push((
            "[D]  跨进程 无保护假Dock（阳性对照，期望夺焦点）",
            w.fake_open,
            h,
            true,
            true,
        ));
        phases.push((
            "[E]  跨进程 带NOACTIVATE假Dock（期望不夺）",
            w.fake,
            h,
            true,
            false,
        ));
    }

    let mut summary: Vec<String> = Vec::new();

    for (label, target, holder, block, expect_steal) in phases {
        BLOCK_MOUSEACTIVATE.store(block, Ordering::SeqCst);
        crate::win_layer::reset_counters();
        PLAIN_PROC_HITS.store(0, Ordering::SeqCst);
        PLAIN_CLICKS.store(0, Ordering::SeqCst);

        force_foreground(holder);
        std::thread::sleep(Duration::from_millis(420));
        let before = unsafe { GetForegroundWindow() };
        let focus_ready = before == holder;

        // ⚠️ 真 Dock **不能点中心**。
        //
        // 面板中心正好落在那一排图标上，点下去会**真的把那个应用激活起来**，
        // 于是 `after` 变成那个应用、`stolen` 为真，测试就报出「焦点被夺」的
        // **假失败**。实测：计算器在 Dock 上时，[A]/[C] 两相的 `after` 一直是
        // '计算器' —— 那不是 Dock 夺焦，是 Dock 正确地把计算器叫到了前台。
        //
        // 改点面板的**左侧内边距**，那一段保证没有图标：
        // 面板宽 = 「放大到最大时的行宽」+ 2×pad，而图标行是居中的，
        // 所以左右各至少留出 pad（12 逻辑像素 ≈ 125% 下 15 物理像素）的空白。
        // 点 dock.left + 5 物理像素既在面板内、又在图标行左侧。
        let (cx, cy) = if target == dock {
            let r = window_rect(dock);
            (r.left + 5, (r.top + r.bottom) / 2)
        } else {
            center(target)
        };
        let under = top_level_window_at(cx, cy);
        let on_target = under == target;

        let injected = click_at(cx, cy);
        std::thread::sleep(Duration::from_millis(520));

        let after = unsafe { GetForegroundWindow() };
        let stolen = after != holder;
        let ma = MOUSEACTIVATE_COUNT.load(Ordering::SeqCst);
        let plain_clicks = PLAIN_CLICKS.load(Ordering::SeqCst);

        // 只有纯 Win32 目标的「收到了 WM_LBUTTONDOWN」才能证明点击确实送达；
        // 真 Dock 的点击会被 WebView2 子窗口吃掉，顶层收不到任何鼠标消息，
        // 所以对真 Dock 不能用这个判据（用了会得出错误的 INVALID）。
        let click_proven = if target == w.fake || target == w.fake_open {
            plain_clicks > 0
        } else {
            // 真 Dock：至少确认点击坐标落在它上面
            on_target
        };

        let verdict = if !focus_ready {
            "INVALID（焦点未就位）"
        } else if injected < 3 {
            "INVALID（SendInput 未注入）"
        } else if !click_proven {
            "INVALID（无法确认点击送达）"
        } else if stolen == expect_steal {
            if stolen {
                "焦点被夺（符合预期）"
            } else {
                "焦点**未**被夺 ✅（符合预期）"
            }
        } else if stolen {
            "焦点被夺 ⚠️ **不符合预期**"
        } else {
            "焦点未被夺 ⚠️ **不符合预期**"
        };

        log_info!("\n{label}");
        log_info!("    before = {}   after = {}", describe(before), describe(after));
        log_info!("    点击点顶层窗口 = {}  (注入 {}/3)", describe(under), injected);
        log_info!("    目标收到：WM_MOUSEACTIVATE={ma}  WM_LBUTTONDOWN={plain_clicks}");
        log_info!("    -> {verdict}");
        summary.push(format!("{label} -> {verdict}"));
    }

    unsafe {
        let _ = SetCursorPos(orig.x, orig.y);
    }
    BLOCK_MOUSEACTIVATE.store(true, Ordering::SeqCst);

    log_info!("\n------------------------- 结论 -------------------------");
    for s in &summary {
        log_info!("  {s}");
    }
    // 把「哪些相本来就是红的」写清楚。否则这两行红字会训练人忽略整套输出，
    // 真正的回归就混在里面看不见了。
    log_info!("");
    log_info!("  注：[A]/[B] 报红是**已知限制**，不是回归 ——");
    log_info!("      同进程点击 Dock 时，WM_MOUSEACTIVATE 会落到无法子类化的深层");
    log_info!("      Chromium 子窗口（comctl32 子类化是线程局部的，硬限制），");
    log_info!("      任何钩子都拦不到。实际防护是「Dock 不创建任何会获得焦点的自有窗口」。");
    log_info!("      **验收判据是 [C] 跨进程相**（真实工况：你从别的应用点 Dock）。");
    log_info!("      详见 README「已知限制」表。");
    log_info!("========================================================\n");
}

/// 应用操作自检：**激活 / 最小化 / 还原**。
///
/// 这是验收标准「能替代任务栏完成日常启动与切换常用应用」的核心行为，
/// 前几轮一直因为「合成点击不可用」而被推给人工 —— 那个归因是错的（见 §6.14）。
///
/// 做法：spawn 一个 `dock-app holdwin` 子进程当操作对象（另一个进程的普通窗口），
/// 然后直接调用 UI 点击时用的同一套 `apps::` 函数并断言结果。
pub fn drive_app_ops(app: &tauri::AppHandle, settings: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsIconic};

    log_info!("\n============= 应用操作自检：激活 / 最小化 / 还原 =============");

    let Ok(exe) = std::env::current_exe() else {
        log_info!("取不到自身路径，跳过");
        return;
    };
    let mut child = match std::process::Command::new(exe).arg("holdwin").spawn() {
        Ok(c) => c,
        Err(e) => {
            log_info!("无法启动操作对象进程: {e}");
            return;
        }
    };

    let mut target = HWND::default();
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        let h = unsafe { FindWindowW(windows::core::w!("DockOpponentWindow"), None) }
            .unwrap_or_default();
        if !h.0.is_null() {
            target = h;
            break;
        }
    }
    if target.0.is_null() {
        log_info!("操作对象窗口未出现，跳过");
        let _ = child.kill();
        return;
    }
    log_info!("操作对象（另一进程）: {}", describe(target));

    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut check = |name: &str, ok: bool, detail: String| {
        log_info!("  [{}] {name}  {detail}", if ok { "PASS" } else { "FAIL" });
        if ok {
            pass += 1
        } else {
            fail += 1
        }
    };

    force_foreground(settings);
    std::thread::sleep(Duration::from_millis(400));
    let before = unsafe { GetForegroundWindow() };
    check(
        "前置：焦点在别处",
        before == settings,
        format!("foreground={}", describe(before)),
    );

    let r = crate::apps::activate(target);
    std::thread::sleep(Duration::from_millis(450));
    let fg = unsafe { GetForegroundWindow() };
    check(
        "activate() 把目标带到前台",
        fg == target && r.is_ok(),
        format!("foreground={}  result={r:?}", describe(fg)),
    );

    let r = crate::apps::minimize(target);
    std::thread::sleep(Duration::from_millis(450));
    let iconic = unsafe { IsIconic(target).as_bool() };
    check(
        "minimize() 让目标最小化",
        iconic && r.is_ok(),
        format!("IsIconic={iconic}  result={r:?}"),
    );

    let r = crate::apps::activate(target);
    std::thread::sleep(Duration::from_millis(500));
    let iconic2 = unsafe { IsIconic(target).as_bool() };
    let fg2 = unsafe { GetForegroundWindow() };
    check(
        "activate() 能把最小化窗口还原并带到前台",
        !iconic2 && fg2 == target && r.is_ok(),
        format!(
            "IsIconic={iconic2}  foreground={}  result={r:?}",
            describe(fg2)
        ),
    );

    // ⑤ 启动：验证 apps::launch 真的能拉起一个进程，且启动后能被枚举到。
    //
    //    判据用「运行中的应用数是否增加」，而不是「target 是否等于传入路径」——
    //    实测踩到：`System32\notepad.exe` 是**应用执行别名（stub）**，
    //    它会重定向到商店版记事本，真实进程路径在 `WindowsApps\...` 下，
    //    按传入路径比对必然匹配不上（那不是 bug，是 Windows 的重定向）。
    if let Ok(path) = std::env::var("DOCK_LAUNCH_TEST") {
        let count_running = || {
            crate::apps::enumerate_apps()
                .iter()
                .filter(|a| a.running)
                .count()
        };
        let before = count_running();
        let r = crate::apps::launch(&path, false);
        std::thread::sleep(Duration::from_millis(3500));
        let after = count_running();
        check(
            "launch() 后运行中的应用数增加",
            after > before && r.is_ok(),
            format!("运行中应用数 {before} -> {after}  result={r:?}"),
        );
    }

    // ⑥ 配置**写入**路径往返：固定 → 落盘 → 取消固定 → 落盘。
    //    之前只验证过「读配置能驱动行为」，从没验证过**写**；而固定项正是靠写落盘的。
    if let Ok(dir) = std::env::var("DOCK_CONFIG_DIR") {
        let cfg = std::path::Path::new(&dir).join("config.json");
        let probe_id = "__selftest_pin__";

        let _ = crate::store::unpin(app, probe_id); // 清掉可能的残留
        let pin_ok = crate::store::pin(
            app,
            crate::model::PinnedApp {
                id: probe_id.to_string(),
                display_name: "自检项".into(),
                target: r"C:\Windows\notepad.exe".into(),
                separator: false,
                is_folder: false,
                children: Vec::new(),
            },
        )
        .is_ok();
        let persisted = std::fs::read_to_string(&cfg)
            .map(|s| s.contains(probe_id))
            .unwrap_or(false);

        let unpin_ok = crate::store::unpin(app, probe_id).is_ok();
        let removed = std::fs::read_to_string(&cfg)
            .map(|s| !s.contains(probe_id))
            .unwrap_or(false);

        check(
            "固定 / 取消固定能持久化到磁盘",
            pin_ok && persisted && unpin_ok && removed,
            format!("写盘={persisted} 清除={removed} pin_ok={pin_ok} unpin_ok={unpin_ok}"),
        );
    }

    unsafe {
        let _ = PostMessageW(Some(target), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let _ = child.wait();

    log_info!("\n  小结：{pass} 项通过，{fail} 项失败");
    log_info!("==========================================================\n");
}

/// IPC 链路自检：**JS → invoke → Rust 命令 → Win32**。
///
/// 这是「点击图标」这条路上最后一段没验证过的环节 ——
/// 前面的 §6.14 验证了焦点、上面的 `drive_app_ops` 验证了 `apps::` 原语，
/// 但「前端能不能真的调到命令」还没验过。
///
/// 做法：用 `webview.eval` 在前端注入调用，前端走**和真实点击同一个** `activateApp`，
/// 再把结果通过一条回执命令写回 Rust（不依赖 window title 之类的间接通道）。
pub fn drive_ipc_test(app: &tauri::AppHandle, settings: HWND) {
    use tauri::Manager;
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

    log_info!("\n============= IPC 链路自检：JS → invoke → Rust → Win32 =============");

    let Ok(exe) = std::env::current_exe() else {
        log_info!("取不到自身路径，跳过");
        return;
    };
    let Ok(mut child) = std::process::Command::new(exe).arg("holdwin").spawn() else {
        log_info!("无法启动操作对象进程，跳过");
        return;
    };

    let mut target = HWND::default();
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        let h = unsafe { FindWindowW(windows::core::w!("DockOpponentWindow"), None) }
            .unwrap_or_default();
        if !h.0.is_null() {
            target = h;
            break;
        }
    }
    if target.0.is_null() {
        log_info!("操作对象窗口未出现，跳过");
        let _ = child.kill();
        return;
    }
    log_info!("操作对象（另一进程）: {}", describe(target));

    // 让目标不在前台，这样「激活成功」才有区分度
    force_foreground(settings);
    std::thread::sleep(Duration::from_millis(400));

    let _ = crate::commands::take_test_report();
    let Some(win) = app.get_webview_window("dock") else {
        log_info!("找不到 dock 窗口，跳过");
        let _ = child.kill();
        return;
    };

    let js = format!(
        "window.__dockIpcTest && window.__dockIpcTest({});",
        target.0 as usize
    );
    if let Err(e) = win.eval(&js) {
        log_info!("eval 注入失败: {e}");
        let _ = child.kill();
        return;
    }

    // 轮询回执
    let mut report = String::new();
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        let r = crate::commands::take_test_report();
        if !r.is_empty() {
            report = r;
            break;
        }
    }

    let fg = unsafe { GetForegroundWindow() };
    let foreground_ok = fg == target;
    let report_ok = report == "activate_app:ok";

    log_info!("  回执            : {}", if report.is_empty() { "<超时未收到>" } else { &report });
    log_info!("  目标是否到前台  : {}  (foreground={})", foreground_ok, describe(fg));
    log_info!(
        "  -> {}",
        if report_ok && foreground_ok {
            "链路完整 ✅（前端成功调用命令且窗口行为正确）"
        } else if report_ok {
            "命令调用成功，但窗口未见激活 ⚠️"
        } else {
            "链路有问题 ❌"
        }
    );

    unsafe {
        let _ = PostMessageW(Some(target), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let _ = child.wait();
    log_info!("==================================================================\n");
}

// ---------------------------------------------------------------- 设置窗口

/// 设置窗口自检。
///
/// **为什么需要它**：设置窗口只能靠托盘菜单打开，自动化测试点不到托盘；而这一段
/// （多页面前端 + 第二个 webview + 两个新命令）如果不验证，等于把一个没人跑过的
/// 功能交给用户。做法与 `drive_ipc_test` 一致：`eval` 注入 JS，结果经
/// `selftest_report` 回传。
///
/// 验证四件事：
///  1. 设置窗口能创建，且是**普通窗口**（有标题、进任务栏、可缩放）
///  2. 页面真的渲染了（数 DOM 里的 `<section>` 数量 —— 只有 React 挂载成功才非 0）
///  3. 页面的 IPC 能到 Rust（`get_dock_info` / `get_preferences` 返回预期字段）
///  4. 改背景色能落盘并**即时生效**（改完再读回来应当是改后的值）
///
/// 最后会把颜色**还原**成原来的值，不污染用户配置。
pub fn drive_settings_test(app: &tauri::AppHandle) {
    use tauri::Manager;

    log_info!("\n============= 设置窗口自检 =============");

    if let Err(e) = crate::settings_window::open(app) {
        log_info!("  打开设置窗口失败: {e}");
        return;
    }

    // 等窗口出现
    let mut found = None;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(w) = app.get_webview_window(crate::settings_window::LABEL) {
            found = Some(w);
            break;
        }
    }
    let Some(win) = found else {
        log_info!("  设置窗口没出现 ❌");
        return;
    };

    // 等页面挂载（生产构建的前端是本地资源，很快，但给足余量）
    std::thread::sleep(Duration::from_millis(1500));

    // 窗口属性：证明它是普通窗口而不是 Dock 那种工具窗口
    if let Ok(raw) = win.hwnd() {
        let h = HWND(raw.0 as *mut core::ffi::c_void);
        let ex = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32;
        let r = crate::win_layer::window_rect(h);
        log_info!(
            "  窗口             : hwnd=0x{:X}  {}x{}  标题='{}'",
            raw.0 as usize,
            r.right - r.left,
            r.bottom - r.top,
            win.title().unwrap_or_default()
        );
        log_info!(
            "  扩展样式         : {}",
            crate::win_layer::describe_ex_style(ex)
        );
        log_info!(
            "  -> 普通窗口      : {}",
            if ex & WS_EX_NOACTIVATE.0 == 0 && ex & WS_EX_TOOLWINDOW.0 == 0 {
                "是 ✅（可聚焦、进任务栏）"
            } else {
                "否 ❌（带了 NOACTIVATE / TOOLWINDOW，用户没法打字）"
            }
        );
    }

    // 页面内驱动：数 DOM、读信息、改颜色再读回、还原
    let _ = crate::commands::take_test_report();
    let js = r#"(async () => {
  const I = window.__TAURI_INTERNALS__;
  const rep = (m) => I.invoke("selftest_report", { msg: String(m) });
  try {
    if (!I || !I.invoke) return void rep("no-bridge");
    const sections = document.querySelectorAll("section").length;
    const textLen = (document.body.innerText || "").length;
    const info = await I.invoke("get_dock_info");
    const prefs = await I.invoke("get_preferences");
    const before = JSON.stringify(prefs.glassRgb) + "@" + prefs.glassAlpha;
    const test = Object.assign({}, prefs, { glassRgb: [200, 60, 60], glassAlpha: 200 });
    await I.invoke("set_preferences", { prefs: test });
    const after = await I.invoke("get_preferences");
    const changed = JSON.stringify(after.glassRgb) + "@" + after.glassAlpha;
    await I.invoke("set_preferences", { prefs: prefs });   // 还原
    rep("ok sections=" + sections + " textLen=" + textLen
        + " ver=" + info.version + " pinned=" + info.pinnedCount
        + " running=" + info.runningCount + " scale=" + info.scale
        + " logical=" + info.dockLogical.join("x")
        + " | color " + before + " -> " + changed);
  } catch (e) {
    rep("err " + e);
  }
})();"#;

    if let Err(e) = win.eval(js) {
        log_info!("  eval 注入失败: {e}");
        return;
    }

    let mut report = String::new();
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(100));
        let r = crate::commands::take_test_report();
        if !r.is_empty() {
            report = r;
            break;
        }
    }

    log_info!(
        "  页面回执         : {}",
        if report.is_empty() { "<超时未收到>" } else { &report }
    );

    // 断言用「渲染出了若干 section」而不是写死数量 —— 写死数量的话，
    // 以后往设置页加一节（比如这次的「Dock 位置」）就会误报失败。
    let sections: usize = report
        .split("sections=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let ok = report.starts_with("ok ")
        && sections >= 2
        && report.contains("-> [200,60,60]@200");
    log_info!(
        "  -> {}",
        if ok {
            "页面已渲染、IPC 通、背景色可即时改 ✅"
        } else {
            "有问题 ❌（逐项看上面的回执）"
        }
    );

    // ---- 「距底部距离」是否**立刻**生效（不重启）----
    //
    // 判据用 Dock 窗口的实际 y：从 Rust 侧直接读，不靠页面自报。
    // 注意还原时要恢复成**原来的值**，不是写死 0 —— 否则会把用户设的底距清掉。
    let orig_offset = app
        .state::<crate::store::PrefsState>()
        .0
        .lock()
        .unwrap()
        .bottom_offset;
    if let Some(dock) = app.get_webview_window("dock").and_then(|w| w.hwnd().ok()) {
        let dh = HWND(dock.0 as *mut core::ffi::c_void);
        let scale = crate::win_layer::dpi_of(dh) as f64 / 96.0;
        let before = crate::win_layer::window_rect(dh).top;
        // 在原值基础上抬高 60，这样不管用户当前设的是多少都成立
        let target = orig_offset + 60;
        let js = format!(
            r#"(async () => {{
  const I = window.__TAURI_INTERNALS__;
  const p = await I.invoke('get_preferences');
  p.bottomOffset = {target};
  await I.invoke('set_preferences', {{ prefs: p }});
  I.invoke('selftest_report', {{ msg: 'offset-set' }});
}})();"#
        );
        let _ = crate::commands::take_test_report();
        let _ = win.eval(&js);
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            if !crate::commands::take_test_report().is_empty() {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500)); // 等 reveal 线程挪窗口
        let after = crate::win_layer::window_rect(dh).top;
        let expect = (60.0 * scale).round() as i32;
        let moved = before - after;
        log_info!(
            "  位置即时生效     : 距底部 {orig_offset} → {target} 逻辑像素，窗口上移 {moved} 物理像素（期望 {expect}）  {}",
            if (moved - expect).abs() <= 2 { "✅" } else { "❌" }
        );

        // 还原成用户原来的值
        let js2 = format!(
            r#"(async () => {{
  const I = window.__TAURI_INTERNALS__;
  const p = await I.invoke('get_preferences');
  p.bottomOffset = {orig_offset};
  await I.invoke('set_preferences', {{ prefs: p }});
}})();"#
        );
        let _ = win.eval(&js2);
        std::thread::sleep(Duration::from_millis(400));
    }

    // ---- 图标边长是否**立刻**生效 ----
    //
    // 判据用 Dock 页面里第一个图标的实际 style.width —— 精确、可判读，
    // 比"窗口宽了一点"这种模糊断言强得多。
    if let Some(dock) = app.get_webview_window("dock") {
        let orig_icon = app
            .state::<crate::store::PrefsState>()
            .0
            .lock()
            .unwrap()
            .icon_size;
        let target: u32 = if orig_icon == 56 { 48 } else { 56 };
        let js = format!(
            r#"(async () => {{
  const I = window.__TAURI_INTERNALS__;
  const p = await I.invoke('get_preferences');
  p.iconSize = {target};
  await I.invoke('set_preferences', {{ prefs: p }});
  I.invoke('selftest_report', {{ msg: 'icon-set' }});
}})();"#
        );
        let _ = crate::commands::take_test_report();
        let _ = win.eval(&js);
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            if !crate::commands::take_test_report().is_empty() {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(700));
        let got = eval_in(&dock, "document.querySelector('.dock-icon')?.style.width || '<无图标>'");
        log_info!(
            "  图标边长即时生效 : 设为 {target} → Dock 页面里第一个图标 width='{got}'  {}",
            if got.trim() == format!("{target}px") {
                "✅"
            } else {
                "❌"
            }
        );
        // 还原
        let js2 = format!(
            r#"(async () => {{
  const I = window.__TAURI_INTERNALS__;
  const p = await I.invoke('get_preferences');
  p.iconSize = {orig_icon};
  await I.invoke('set_preferences', {{ prefs: p }});
}})();"#
        );
        let _ = win.eval(&js2);
        std::thread::sleep(Duration::from_millis(500));
    }

    // ---- 面板高度是否立刻生效，而且**底边不动**（往上长，不是往下长）----
    //
    // 「往上长」是刻意的：Dock 贴底，变高应该朝屏幕中间长，否则会跑到屏幕外。
    // 判据同时看两点：窗口高度增加了应有的量，**且底边坐标没变**。
    if let Some(dock) = app.get_webview_window("dock").and_then(|w| w.hwnd().ok()) {
        let dh = HWND(dock.0 as *mut core::ffi::c_void);
        let scale = crate::win_layer::dpi_of(dh) as f64 / 96.0;
        let before = crate::win_layer::window_rect(dh);
        let orig_h = app
            .state::<crate::store::PrefsState>()
            .0
            .lock()
            .unwrap()
            .panel_height;
        let target = orig_h + 28;
        let js = format!(
            r#"(async () => {{
  const I = window.__TAURI_INTERNALS__;
  const p = await I.invoke('get_preferences');
  p.panelHeight = {target};
  await I.invoke('set_preferences', {{ prefs: p }});
  I.invoke('selftest_report', {{ msg: 'h-set' }});
}})();"#
        );
        let _ = crate::commands::take_test_report();
        let _ = win.eval(&js);
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            if !crate::commands::take_test_report().is_empty() {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(800));
        let after = crate::win_layer::window_rect(dh);
        let grew = (after.bottom - after.top) - (before.bottom - before.top);
        let expect = (28.0 * scale).round() as i32;
        let bottom_same = after.bottom == before.bottom;
        log_info!(
            "  面板高度即时生效 : {orig_h} → {target} 逻辑像素，窗口高度 +{grew}（期望 {expect}），底边{}  {}",
            if bottom_same { "不动" } else { "移动了" },
            if (grew - expect).abs() <= 2 && bottom_same {
                "✅"
            } else {
                "❌"
            }
        );

        let js2 = format!(
            r#"(async () => {{
  const I = window.__TAURI_INTERNALS__;
  const p = await I.invoke('get_preferences');
  p.panelHeight = {orig_h};
  await I.invoke('set_preferences', {{ prefs: p }});
}})();"#
        );
        let _ = win.eval(&js2);
        std::thread::sleep(Duration::from_millis(500));
    }

    let _ = win.close();
    std::thread::sleep(Duration::from_millis(400));
    log_info!("======================================\n");
}

// ---------------------------------------------------------------- 拖放添加

/// 拖放添加自检。
///
/// **能自动验证**：路径解析（exe / 文件夹 / 未知扩展名 / `.lnk`）、去重、
/// 加进列表的位置（末尾）、以及「已在运行 → 会亮白点」这条判定。
///
/// **不能自动验证**：操作系统是否真的把拖放投递到我们的窗口 —— 那需要真的用鼠标
/// 从桌面拖一次（合成一个 OLE 拖动循环不现实，而且可能在用户桌面上误开文件）。
/// 这一条只能人工确认，与 UAC 弹窗、右键菜单视觉同类。
///
/// 测试会**把添加的东西全部还原**，不留痕迹。
pub fn drive_drop_test(app: &tauri::AppHandle) {
    use std::path::PathBuf;
    use tauri::Manager;

    log_info!("\n============= 拖放添加自检 =============");

    let pinned_ids = || -> Vec<String> {
        app.state::<crate::store::PrefsState>()
            .0
            .lock()
            .unwrap()
            .pinned
            .iter()
            .map(|p| p.id.clone())
            .collect()
    };
    let before = pinned_ids();
    let mut added: Vec<String> = Vec::new();
    let mut pass = 0;
    let mut fail = 0;
    let mut check = |ok: bool, what: &str, detail: String| {
        if ok {
            pass += 1;
            log_info!("  [PASS] {what}  {detail}");
        } else {
            fail += 1;
            log_info!("  [FAIL] {what}  {detail}");
        }
    };

    // ---- 1. 普通 exe：应当被接受，并**追加到固定列表末尾** ----
    let exe = PathBuf::from(r"C:\Windows\System32\cmd.exe");
    let exe_id = crate::apps::id_for_target(&exe.display().to_string());
    if before.contains(&exe_id) {
        log_info!("  （cmd.exe 已在固定列表里，跳过「新增」用例）");
    } else {
        match crate::drop::handle_one(app, &exe) {
            crate::drop::Outcome::Added { name, running } => {
                added.push(exe_id.clone());
                let now = pinned_ids();
                check(
                    now.last() == Some(&exe_id) && now.len() == before.len() + 1,
                    "普通 exe 被接受且排在末尾",
                    format!("name='{name}' running={running} 位置={}/{}", now.len(), now.len()),
                );
            }
            other => {
                let d = describe_outcome(&other);
                check(false, "普通 exe 被接受且排在末尾", d);
            }
        }
    }

    // ---- 2. 同一个再拖一次：应当去重，固定列表不变 ----
    let n_before_dup = pinned_ids().len();
    match crate::drop::handle_one(app, &exe) {
        crate::drop::Outcome::Already { name } => check(
            pinned_ids().len() == n_before_dup,
            "重复拖同一个：被识别为「已在」且不重复添加",
            format!("name='{name}' 数量仍为 {}", n_before_dup),
        ),
        other => {
            let d = describe_outcome(&other);
            check(false, "重复拖同一个：被识别为「已在」", d)
        }
    }

    // ---- 3. 文件夹：应当被拒绝 ----
    let dir = PathBuf::from(r"C:\Windows");
    match crate::drop::handle_one(app, &dir) {
        crate::drop::Outcome::Rejected { why, .. } => check(
            why.contains("文件夹"),
            "文件夹被拒绝且说明了原因",
            format!("why='{why}'"),
        ),
        other => {
            let d = describe_outcome(&other);
            check(false, "文件夹被拒绝", d)
        }
    }

    // ---- 4. 非程序文件：应当被拒绝 ----
    let txt = std::env::temp_dir().join("dock-drop-test.txt");
    let _ = std::fs::write(&txt, b"not a program");
    match crate::drop::handle_one(app, &txt) {
        crate::drop::Outcome::Rejected { why, name } => check(
            why.contains("只接受") && name.ends_with(".txt"),
            "未知扩展名被拒绝，且提示用文件名（不是全路径）",
            format!("name='{name}' why='{why}'"),
        ),
        other => {
            let d = describe_outcome(&other);
            check(false, "未知扩展名被拒绝", d)
        }
    }
    let _ = std::fs::remove_file(&txt);

    // ---- 5. .lnk：解析出真实目标 ----
    match find_lnk() {
        Some(lnk) => match crate::drop::resolve_target(&lnk) {
            Ok(t) => check(
                t.to_lowercase().ends_with(".exe") || t.starts_with("shell:"),
                "快捷方式解析出可启动目标",
                format!("{} -> {t}", lnk.display()),
            ),
            Err(e) => check(false, "快捷方式解析出可启动目标", format!("{e}")),
        },
        None => log_info!("  （没找到 .lnk 样本，跳过快捷方式用例）"),
    }

    // ---- 6. 「正在运行」判定：拿一个正在跑、且没固定的 Win32 程序试 ----
    let running_win32 = crate::apps::enumerate_apps().into_iter().find(|a| {
        a.target.to_lowercase().ends_with(".exe")
            && std::path::Path::new(&a.target).exists()
            && !before.contains(&a.id)
    });
    match running_win32 {
        Some(a) => {
            let p = PathBuf::from(&a.target);
            match crate::drop::handle_one(app, &p) {
                crate::drop::Outcome::Added { running, name } => {
                    added.push(a.id.clone());
                    check(
                        running,
                        "已在运行的程序：添加时会识别出「运行中」",
                        format!("name='{name}' 期望 running=true，实得 {running}"),
                    );
                }
                other => {
                    let d = describe_outcome(&other);
                    check(false, "已在运行的程序被识别为运行中", d)
                }
            }
        }
        None => log_info!("  （没有「在跑且未固定」的 Win32 程序，跳过运行判定用例）"),
    }

    // ---- 还原：把本次测试添加的全部取消固定 ----
    for id in &added {
        let _ = crate::store::unpin(app, id);
    }
    let after = pinned_ids();
    check(
        after == before,
        "测试结束后已还原配置",
        format!("固定项 {} -> {} -> {}", before.len(), before.len() + added.len(), after.len()),
    );

    log_info!("  小结：{pass} 项通过，{fail} 项失败");
    log_info!("  ⚠️ 未覆盖：OS 是否真的把拖放投递到窗口 —— 需要人工从桌面拖一次");
    log_info!("======================================\n");
}

fn describe_outcome(o: &crate::drop::Outcome) -> String {
    match o {
        crate::drop::Outcome::Added { name, running } => {
            format!("意外地被接受: {name} running={running}")
        }
        crate::drop::Outcome::Already { name } => format!("意外地判定为已在: {name}"),
        crate::drop::Outcome::Rejected { why, .. } => format!("意外地被拒绝: {why}"),    }
}

/// 找一个真实的 `.lnk` 当样本（开始菜单里一定有）。
fn find_lnk() -> Option<std::path::PathBuf> {
    let roots = [
        r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs",
        r"C:\Users\Public\Desktop",
    ];
    for root in roots {
        if let Some(p) = walk_for_lnk(std::path::Path::new(root), 0) {
            return Some(p);
        }
    }
    None
}

fn walk_for_lnk(dir: &std::path::Path, depth: usize) -> Option<std::path::PathBuf> {
    if depth > 3 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
        } else if p.extension().map(|x| x.eq_ignore_ascii_case("lnk")) == Some(true) {
            return Some(p);
        }
    }
    for d in subdirs {
        if let Some(p) = walk_for_lnk(&d, depth + 1) {
            return Some(p);
        }
    }
    None
}

pub fn destroy_windows(w: &TestWindows) {    unsafe {
        let _ = DestroyWindow(w.fake_open);
        let _ = DestroyWindow(w.fake);
        let _ = DestroyWindow(w.settings);
    }
}

// ---------------------------------------------------------------- 右键菜单 / 分割线自检

/// 右键点击（真实合成输入，走完整的「页面 onContextMenu → invoke → Rust」链路）
fn right_click_at(x: i32, y: i32) -> u32 {
    let n = mouse_abs(x, y);
    std::thread::sleep(Duration::from_millis(90));
    let n = n + mouse_input(MOUSEEVENTF_RIGHTDOWN, 0, 0);
    std::thread::sleep(Duration::from_millis(60));
    n + mouse_input(MOUSEEVENTF_RIGHTUP, 0, 0)
}

/// 敲一下某个键（按下 + 抬起）
fn key_press(vk: VIRTUAL_KEY) -> u32 {
    let mk = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let cb = std::mem::size_of::<INPUT>() as i32;
    unsafe {
        let n = SendInput(&[mk(KEYBD_EVENT_FLAGS(0))], cb);
        std::thread::sleep(Duration::from_millis(60));
        n + SendInput(&[mk(KEYEVENTF_KEYUP)], cb)
    }
}

/// 第 `idx` 个菜单项在屏幕上的中心点（**问页面要**，不按行高去猜）
fn menu_item_point(
    menu_win: &tauri::WebviewWindow,
    mrect: &RECT,
    scale: f64,
    idx: usize,
) -> Option<(i32, i32)> {
    let expr = format!(
        r#"(function () {{
  const e = document.querySelectorAll('.menu-item')[{idx}];
  if (!e) return '<none>';
  const b = e.getBoundingClientRect();
  return Math.round(b.left + b.width / 2) + ':' + Math.round(b.top + b.height / 2);
}})()"#
    );
    let v = eval_in(menu_win, &expr);
    let (x, y) = v.split_once(':')?;
    let cx: f64 = x.parse().ok()?;
    let cy: f64 = y.parse().ok()?;
    Some((
        mrect.left + (cx * scale).round() as i32,
        mrect.top + (cy * scale).round() as i32,
    ))
}

/// 等 Dock 页面把面板尺寸报上来。
///
/// ❗为什么必须等：`set_dock_size` 是**页面挂载之后**才调的，在那之前窗口一直是
/// 建窗时的初值（520×panelHeight 逻辑）。不等就量，会得出「加了分割线反而变窄了 337」
/// 这种荒唐结论（第一版就是这么栽的 —— 量的其实是「页面还没上报」这个状态）。
///
/// 判据：DOM 里的图标数等于配置数，**并且**窗口宽度连续 `STABLE` 次不变。
fn wait_panel_settled(
    win: &tauri::WebviewWindow,
    dock: HWND,
    want: usize,
    timeout: Duration,
) -> (bool, i32) {
    const STABLE: i32 = 3;
    let t0 = std::time::Instant::now();
    let mut same = 0;
    let mut last_w = -1;
    loop {
        std::thread::sleep(Duration::from_millis(400));
        let n = eval_in(win, "document.querySelectorAll('.dock-icon').length");
        let w = {
            let r = window_rect(dock);
            r.right - r.left
        };
        let ok_count = n.trim() == want.to_string();
        if ok_count && w == last_w {
            same += 1;
            if same >= STABLE {
                return (true, w);
            }
        } else {
            same = 0;
        }
        last_w = w;
        if t0.elapsed() > timeout {
            log_info!(
                "  [等待超时] DOM 图标数={n}（期望 {want}）窗口宽={w}  用时 {:?}",
                t0.elapsed()
            );
            return (false, w);
        }
    }
}

/// 右键第 `idx` 个图标。
///
/// 先移过去、**再重新量一次**：鱼眼放大在光标进入时会重排整行（`getBoundingClientRect`
/// 反映的是变换后的位置），用移入之前的坐标去点会点到邻居身上。
fn right_click_icon(
    win: &tauri::WebviewWindow,
    dock: HWND,
    scale: f64,
    idx: usize,
) -> Option<(i32, i32)> {
    let c1 = icon_centers(win, &window_rect(dock), scale)?;
    let (x1, y1) = *c1.get(idx)?;
    mouse_abs(x1, y1);
    std::thread::sleep(Duration::from_millis(300));
    let c2 = icon_centers(win, &window_rect(dock), scale)?;
    let (x2, y2) = *c2.get(idx)?;
    right_click_at(x2, y2);
    Some((x2, y2))
}

/// 会改动 Dock 列表的自检（拖拽 / 菜单）在动手前必须先确认配置被重定向到了临时目录。
///
/// ❗**为什么必须要有这道闸**（本轮踩过，真实配置被写脏了一次）：
/// 这两组用例会**真的往配置里加/删条目**（加分割线、拖走图标），只在正常跑完时还原。
/// 一旦中途被打断 —— 超时被杀、构建覆盖了可执行文件、Ctrl-C ——
/// 用户的 Dock 上就留下测试痕迹。本轮就是真实配置里多出一条分割线，
/// 而且**事后很难从日志看出是哪一次留下的**。
///
/// 所以：宁可拒绝跑，也不要动用户正在用的配置。
fn require_temp_config(what: &str) -> bool {
    match std::env::var("DOCK_CONFIG_DIR") {
        Ok(d) if !d.trim().is_empty() => true,
        _ => {
            log_info!("\n===== {what} 已跳过（安全闸）=====");
            log_info!("  这会**真的改动 Dock 列表**，且只在正常跑完时还原。");
            log_info!("  为防止中途被打断时弄脏正在使用的配置，必须先指向一个临时目录：");
            log_info!(r#"    $env:DOCK_CONFIG_DIR = "$PWD\.dockcfg-test""#);
            log_info!("  （顺手把真实配置复制进去，这样测试用的是同一批应用）");
            log_info!("==================================\n");
            false
        }
    }
}

/// 量「图标之间的间距到底是多少」（开关 `DOCK_ICONBOX=1`）。
///
/// 分两层量，因为"看起来空"有两个完全不同的来源：
///   1. **布局间距** —— 从页面读每个按钮的 `getBoundingClientRect()`，算相邻间隙；
///   2. **图形留白** —— 图标画布是固定方的，但图形在画布里留多少白各不相同。
///      视觉间距 = 布局间距 + 左边图形的右留白 + 右边图形的左留白。
///
/// 有了这两组数字，"某几个图标之间显得空"就能定位到是布局要改还是图形要放大，
/// 而不是靠感觉调。
pub fn dump_icon_boxes(app: &tauri::AppHandle) {
    use tauri::Manager;

    log_info!("\n==================== 图标间距与图形留白 ====================");

    let pinned = app
        .state::<crate::store::PrefsState>()
        .0
        .lock()
        .unwrap()
        .pinned
        .clone();

    // ---- 1. 布局：相邻按钮之间的物理间隙 ----
    if let Some(win) = app.get_webview_window("dock") {
        if let (Ok(raw), Ok(mrect)) = (
            win.hwnd(),
            win.hwnd().map(|h| {
                crate::win_layer::window_rect(HWND(h.0 as *mut std::ffi::c_void))
            }),
        ) {
            let scale = crate::win_layer::dpi_of(HWND(raw.0 as *mut std::ffi::c_void)) as f64 / 96.0;
            let js = r#"JSON.stringify([...document.querySelectorAll('.dock-icon')].map(function (el) {
  const r = el.getBoundingClientRect();
  return [Math.round(r.left * 100) / 100, Math.round(r.right * 100) / 100];
}))"#;
            let raw_json = eval_in(&win, js);
            if let Ok(arr) = serde_json::from_str::<Vec<[f64; 2]>>(&raw_json) {
                log_info!("  布局（逻辑像素；面板原点 {:.0},{:.0} 缩放 {scale:.2}）", mrect.left as f64, mrect.top as f64);
                for i in 0..arr.len() {
                    let name = pinned
                        .get(i)
                        .map(|p| {
                            if p.separator {
                                "｜分割线".to_string()
                            } else {
                                p.display_name.clone()
                            }
                        })
                        .unwrap_or_default();
                    let gap = if i + 1 < arr.len() {
                        format!("{:.1}", arr[i + 1][0] - arr[i][1])
                    } else {
                        "-".into()
                    };
                    log_info!(
                        "    [{i}] {:<16} 宽 {:.1}  与下一项的间隙 {gap}",
                        name,
                        arr[i][1] - arr[i][0]
                    );
                }
            } else {
                log_info!("  （页面没就绪，读不到按钮矩形：{raw_json}）");
            }
        }
    }

    // ---- 2. 图形留白：alpha 包围盒占画布的多少 ----
    //
    // 「视觉间距」按 iconSize 折算：图形每侧留白 × iconSize / 画布边长。
    let icon_size = app
        .state::<crate::store::PrefsState>()
        .0
        .lock()
        .unwrap()
        .icon_size as f64;
    log_info!("\n  图形留白（画布 128px，按图标 {icon_size:.0} 逻辑像素折算）:");
    for p in &pinned {
        if p.separator {
            continue;
        }
        match crate::icons::extract_icon(&p.target, 128) {
            Some(d) => match crate::icons::alpha_bbox(&d, 8) {
                Some((x0, y0, x1, y1)) => {
                    let k = icon_size / d.width as f64;
                    log_info!(
                        "    {:<16} 图形 {:>3}×{:<3}px（占 {:>3.0}%×{:<3.0}%）  左空 {:.1}  右空 {:.1}  上下 {:>4.1}/{:<4.1} 逻辑像素",
                        p.display_name,
                        x1 - x0 + 1,
                        y1 - y0 + 1,
                        (x1 - x0 + 1) as f64 / d.width as f64 * 100.0,
                        (y1 - y0 + 1) as f64 / d.height as f64 * 100.0,
                        x0 as f64 * k,
                        (d.width - 1 - x1) as f64 * k,
                        y0 as f64 * k,
                        (d.height - 1 - y1) as f64 * k,
                    );
                }
                None => log_info!("    {:<16} 整张全透明（取图标失败？）", p.display_name),
            },
            None => log_info!("    {:<16} **取不到图标**（前端会退回首字母占位）", p.display_name),
        }
    }

    // ---- 3. 渲染后：页面里每张图标**实际画出来的图形**有多宽 ----
    //
    // 这一层才是用户眼睛看到的东西（前两层分别是布局间距和图标的原始留白，
    // 中间还隔着前端的"光学归一化"）。做法：把 `<img>` 的 data URL 解回来，
    // 在页面里扫 alpha 包围盒，再把"视觉间距 = 布局间距 + 左右留白"算出来。
    if let Some(win) = app.get_webview_window("dock") {
        let _ = crate::commands::take_test_report();
        let js = r#"(function () {
  const rep = (m) => window.__TAURI_INTERNALS__.invoke('selftest_report', { msg: 'boxes ' + m });
  (async function () {
    const els = [...document.querySelectorAll('.dock-icon')];
    const out = [];
    for (const el of els) {
      const img = el.querySelector('img');
      const box = el.getBoundingClientRect();
      if (!img) { out.push({ box: [box.left, box.right], g: null }); continue; }
      try {
        const blob = await (await fetch(img.src)).blob();
        const bmp = await createImageBitmap(blob);
        const c = new OffscreenCanvas(bmp.width, bmp.height);
        const cx = c.getContext('2d');
        cx.drawImage(bmp, 0, 0);
        const d = cx.getImageData(0, 0, bmp.width, bmp.height).data;
        let x0 = bmp.width, x1 = -1, y0 = bmp.height, y1 = -1;
        for (let y = 0; y < bmp.height; y++) {
          for (let x = 0; x < bmp.width; x++) {
            if (d[(y * bmp.width + x) * 4 + 3] > 16) {
              if (x < x0) x0 = x; if (x > x1) x1 = x;
              if (y < y0) y0 = y; if (y > y1) y1 = y;
            }
          }
        }
        out.push({ box: [box.left, box.right], g: [x0, x1, y0, y1, bmp.width, bmp.height] });
      } catch (e) { out.push({ box: [box.left, box.right], g: null }); }
    }
    rep(JSON.stringify(out));
  })();
})();"#;
        if win.eval(js).is_ok() {
            let mut report = String::new();
            for _ in 0..60 {
                std::thread::sleep(Duration::from_millis(100));
                let r = crate::commands::take_test_report();
                if !r.is_empty() {
                    report = r;
                    break;
                }
            }
            let body = report.strip_prefix("boxes ").unwrap_or("");
            match serde_json::from_str::<serde_json::Value>(body) {
                Ok(serde_json::Value::Array(arr)) => {
                    log_info!("\n  渲染后的**视觉间距**（页面里量图形实际占宽，逻辑像素）:");
                    // 先把每项的 [框宽, 左留白, 右留白] 算出来
                    let mut m: Vec<(f64, f64, f64)> = Vec::new();
                    for item in &arr {
                        let bx = item["box"].as_array().cloned().unwrap_or_default();
                        let w = bx.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0)
                            - bx.get(0).and_then(|v| v.as_f64()).unwrap_or(0.0);
                        match item["g"].as_array() {
                            Some(g) => {
                                let (x0, x1, iw) = (
                                    g[0].as_f64().unwrap_or(0.0),
                                    g[1].as_f64().unwrap_or(0.0),
                                    g[4].as_f64().unwrap_or(1.0),
                                );
                                let k = w / iw;
                                m.push((w, x0 * k, (iw - 1.0 - x1) * k));
                            }
                            None => m.push((w, 0.0, 0.0)),
                        }
                    }
                    for i in 0..arr.len() {
                        let name = pinned
                            .get(i)
                            .map(|p| {
                                if p.separator {
                                    "｜分割线".to_string()
                                } else {
                                    p.display_name.clone()
                                }
                            })
                            .unwrap_or_default();
                        let gap = if i + 1 < m.len() {
                            let x1 = arr[i]["box"].as_array().cloned().unwrap_or_default()
                                [1].as_f64().unwrap_or(0.0);
                            let x0n = arr[i + 1]["box"].as_array().cloned().unwrap_or_default()
                                [0].as_f64().unwrap_or(0.0);
                            format!("{:.1}", (x0n - x1) + m[i].2 + m[i + 1].1)
                        } else {
                            "-".into()
                        };
                        let pct = if m[i].0 > 0.0 {
                            (m[i].0 - m[i].1 - m[i].2) / m[i].0 * 100.0
                        } else {
                            100.0
                        };
                        // 竖直方向也看一下：包围盒贴到画布边缘（y0==0 或 y1==h-1）。
                        //
                        // ⚠️ 「贴边」本身**不等于被裁**：应用图标（Edge/Chrome…）本来就
                        // 画满整张画布，归一化用 `contain` 的图形也会"正好填满"。
                        // 但这一栏仍有价值：配合上面「图形留白」那一段的**原始**包围盒
                        // （例如回收站 96×118）就能反推倍率 —— 如果这里显示贴边，
                        // 而宽度比例又和原始比例对不上，那才是真被裁了。
                        // （第一版就是那样：给了 1.1 倍纵向余量，回收站顶部被切掉一条。）
                        let clipped = match arr[i]["g"].as_array() {
                            Some(g) => {
                                let (y0, y1, ih) = (
                                    g[2].as_f64().unwrap_or(0.0),
                                    g[3].as_f64().unwrap_or(0.0),
                                    g[5].as_f64().unwrap_or(1.0),
                                );
                                let pct_h = (y1 - y0 + 1.0) / ih * 100.0;
                                if y0 <= 0.0 || y1 >= ih - 1.0 {
                                    format!("  图形占高 {pct_h:>3.0}% 贴边")
                                } else {
                                    format!("  图形占高 {pct_h:>3.0}%")
                                }
                            }
                            None => String::new(),
                        };
                        log_info!(
                            "    {:<16} 图形占宽 {pct:>3.0}%  左空 {:.1} 右空 {:.1}  → **视觉间距 {gap}**{clipped}",
                            name, m[i].1, m[i].2
                        );
                    }
                }
                _ => log_info!("  （渲染后测量失败：{body}）"),
            }
        }
    }
    log_info!("==========================================================\n");
}

/// 系统位置自检（开关 `DOCK_LOCTEST=1`）：**真的**把「此电脑」「回收站」打开一次，
/// 确认出现了新窗口，然后只关掉那个新窗口。
///
/// 为什么非真开一次不可：系统位置能不能用取决于三条路 ——
///   名字（Shell 语言）/ 图标（`IShellItemImageFactory` 认不认 shell 路径）/
///   **打开**（`ShellExecuteEx` 认不认 shell 路径）。
/// 前两条单测就能覆盖（`apps::tests::system_locations_resolve_name_and_icon`），
/// 第三条只有真调一次才知道 —— 而它恰好是用户点下去时唯一看得见的行为。
///
/// ⚠️ 会在屏幕上开两个资源管理器窗口（跑完自动关掉），所以单独开关。
pub fn drive_location_test(app: &tauri::AppHandle) {
    let _ = app;
    log_info!("\n============= 系统位置自检（此电脑 / 回收站）=============");

    let mut pass = 0;
    let mut fail = 0;
    let mut check = |ok: bool, what: &str, detail: String| {
        if ok {
            pass += 1;
            log_info!("  [PASS] {what}  {detail}");
        } else {
            fail += 1;
            log_info!("  [FAIL] {what}  {detail}");
        }
    };

    /// 资源管理器当前所有窗口句柄
    fn explorer_hwnds() -> Vec<(u64, String)> {
        crate::apps::enumerate_apps()
            .into_iter()
            .find(|a| a.id.ends_with("explorer.exe"))
            .map(|a| a.windows.into_iter().map(|w| (w.hwnd, w.title)).collect())
            .unwrap_or_default()
    }

    for (target, fallback) in crate::apps::SYSTEM_LOCATIONS {
        let name = crate::apps::shell_display_name(target).unwrap_or_else(|| (*fallback).into());
        let before = explorer_hwnds();

        let launched = crate::apps::launch(target, false);
        if let Err(e) = &launched {
            check(false, &format!("打开「{name}」"), format!("launch 失败: {e}"));
            continue;
        }

        // 等新窗口出现（资源管理器建窗口要一点时间）
        let mut fresh: Vec<(u64, String)> = Vec::new();
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(300));
            fresh = explorer_hwnds()
                .into_iter()
                .filter(|(h, _)| !before.iter().any(|(b, _)| b == h))
                .collect();
            if !fresh.is_empty() {
                break;
            }
        }

        check(
            !fresh.is_empty(),
            &format!("点「{name}」真的打开了窗口"),
            match fresh.first() {
                Some((h, t)) => format!("新窗口 0x{h:X} '{t}'（目标 {target}）"),
                None => format!("4 秒内没有出现新窗口（launch 返回 Ok，目标 {target}）"),
            },
        );

        // 只关掉刚开出来的那个窗口 —— 绝不能碰用户自己的资源管理器窗口
        for (h, t) in &fresh {
            let r = crate::apps::close_window(HWND(*h as *mut std::ffi::c_void));
            log_info!("  （已关闭测试窗口 0x{h:X} '{t}'：{r:?}）");
        }
        std::thread::sleep(Duration::from_millis(600));
        let after = explorer_hwnds();
        check(
            fresh.iter().all(|(h, _)| !after.iter().any(|(a, _)| a == h)),
            &format!("测试窗口「{name}」已关掉，没留下痕迹"),
            format!(
                "新开 {} 个并已关闭；资源管理器窗口数 {} → {}（原有 {} 个不能动）",
                fresh.len(),
                before.len(),
                after.len(),
                before.len()
            ),
        );
    }

    log_info!("  小结：{pass} 项通过，{fail} 项失败");
    log_info!("======================================================\n");
}

/// 自检里给文件夹用的名字（用固定值，断言"配置里真的叫这个"）
const TEST_FOLDER_NAME: &str = "自检文件夹";

/// 等「新建文件夹」/「重命名」的名字输入框（`prompt_window` 建的那个自绘窗口）出现，
/// 可选地改掉输入框里的内容，然后点「确定」。
///
/// 返回 `Some(提交的名字)`；3 秒内没等到窗口就 `None`。
///
/// 为什么要自检来驱动它：那个框对 Rust 侧是**模态**的（工作线程阻塞在 `recv` 上等结果），
/// 自检不点它，整条「新建文件夹」的路就走不下去（这也是自检第一次跑会卡住的原因）。
/// 顺带把"敲进去的名字有没有生效"一起验了 —— 只验"窗口出现了"是没用的，
/// 名字有没有落盘才是功能本身。
///
/// ❗输入框是 React **受控**组件：`el.value = x` 它收不到（React 只听自己挂的
/// `input` 事件），必须取原型上的原生 setter 赋值、再补一个 `input` 事件；
/// 而且点「确定」要等 React 重渲染完（名字为空时那颗按钮是 disabled 的，
/// 点早了等于没点）。
fn accept_name_prompt(app: &tauri::AppHandle, new_name: Option<&str>) -> Option<String> {
    use tauri::Manager;

    let mut win = None;
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(w) = app.get_webview_window(crate::prompt_window::LABEL) {
            // 窗口是**先创建、渲染完再显示**的（见 `prompt_window.rs` 的显示时序），
            // 所以"可见"就等于"输入框已经能用了"。
            if w.is_visible().unwrap_or(false) {
                win = Some(w);
                break;
            }
        }
    }
    let win = win?;

    // 没给新名字 → 报告框里预填的是什么（用来确认"改名时默认值 = 它现在的名字"），原样提交
    let want = match new_name {
        Some(n) => n.to_string(),
        None => crate::prompt_window::payload()
            .map(|p| p.value)
            .unwrap_or_default(),
    };
    if new_name.is_none() {
        log_info!("  （输入框默认名字 = {want:?}）");
    }

    let js = format!(
        r#"(function () {{
  const el = document.querySelector('.prompt-input');
  if (!el) return;
  const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
  set.call(el, {name});
  el.dispatchEvent(new Event('input', {{ bubbles: true }}));
  setTimeout(function () {{
    const b = document.querySelector('.prompt-btn.primary');
    if (b && !b.disabled) b.click();
  }}, 80);
}})()"#,
        name = serde_json::to_string(&want).unwrap_or_else(|_| "\"\"".into())
    );
    if let Err(e) = win.eval(&js) {
        log_info!("  ⚠️ 驱动名字输入框失败: {e}");
        return None;
    }
    Some(want)
}

/// 第 `idx` 个**文件夹格子**在屏幕上的中心点（和 `menu_item_point` 同一套做法）
fn folder_item_point(
    menu_win: &tauri::WebviewWindow,
    mrect: &RECT,
    scale: f64,
    idx: usize,
) -> Option<(i32, i32)> {
    let expr = format!(
        r#"(function () {{
  const e = document.querySelectorAll('.folder-item')[{idx}];
  if (!e) return '<none>';
  const b = e.getBoundingClientRect();
  return Math.round(b.left + b.width / 2) + ':' + Math.round(b.top + b.height / 2);
}})()"#
    );
    let v = eval_in(menu_win, &expr);
    let (x, y) = v.split_once(':')?;
    let cx: f64 = x.parse().ok()?;
    let cy: f64 = y.parse().ok()?;
    Some((
        mrect.left + (cx * scale).round() as i32,
        mrect.top + (cy * scale).round() as i32,
    ))
}

/// 资源管理器当前所有窗口 `(hwnd, 标题)`。
///
/// 用来验证"点文件夹里的图标真的打开了东西" —— 拿系统位置（此电脑/回收站）当被测对象：
/// 它一定会开一个**可断言、可关闭**的资源管理器窗口。
fn explorer_hwnds() -> Vec<(u64, String)> {
    crate::apps::enumerate_apps()
        .into_iter()
        .find(|a| a.id.ends_with("explorer.exe"))
        .map(|a| a.windows.into_iter().map(|w| (w.hwnd, w.title)).collect())
        .unwrap_or_default()
}

/// 右键菜单 + 分割线自检（开关 `DOCK_MENUTEST=1`）。
///
/// 全部用**真实合成输入**（`SendInput`）驱动：右键图标、按 Esc、点菜单项、点别处。
/// 只有「添加分割线」这一步直接调 `store::add_separator`（要拿到返回的 id 做断言），
/// 其余每一项都走完整链路 —— 这样任何一环断了都能被发现。
pub fn drive_menu_test(app: &tauri::AppHandle, outside: HWND) {
    use tauri::Manager;

    if !require_temp_config("右键菜单 / 分割线自检") {
        return;
    }

    log_info!("\n============= 右键菜单 / 分割线自检 =============");

    let list = || -> Vec<crate::model::PinnedApp> {
        app.state::<crate::store::PrefsState>()
            .0
            .lock()
            .unwrap()
            .pinned
            .clone()
    };
    let original = list();
    if original.len() < 3 {
        log_info!("  固定项少于 3 个（{}），跳过", original.len());
        return;
    }
    let mut pass = 0;
    let mut fail = 0;
    let mut check = |ok: bool, what: &str, detail: String| {
        if ok {
            pass += 1;
            log_info!("  [PASS] {what}  {detail}");
        } else {
            fail += 1;
            log_info!("  [FAIL] {what}  {detail}");
        }
    };

    let Some(win) = app.get_webview_window("dock") else {
        log_info!("  找不到 Dock 窗口");
        return;
    };
    // 两个浮层窗口：`menu` 是菜单层（盖在上面），`panel` 是文件夹层（在下面）。
    // ⚠️ 两者共用同一个页面，所以**读 DOM 的时候必须读对窗口** ——
    // 读错了不会报错，只会给出一个看起来像产品问题的数字（本轮踩过）。
    let Some(menu_win) = app.get_webview_window("menu") else {
        log_info!("  找不到菜单窗口（启动时创建失败？）");
        return;
    };
    let Some(folder_win) = app.get_webview_window("panel") else {
        log_info!("  找不到文件夹窗口（启动时创建失败？）");
        return;
    };
    let (Ok(raw_dock), Ok(raw_menu), Ok(raw_folder)) =
        (win.hwnd(), menu_win.hwnd(), folder_win.hwnd())
    else {
        return;
    };
    let dock = HWND(raw_dock.0 as *mut std::ffi::c_void);
    let menu = HWND(raw_menu.0 as *mut std::ffi::c_void);
    let folder = HWND(raw_folder.0 as *mut std::ffi::c_void);
    let scale = crate::win_layer::dpi_of(dock) as f64 / 96.0;
    let wa = crate::win_layer::work_area();

    // ❗先等页面把面板尺寸报上来（否则量到的是建窗初值 520 逻辑像素）
    let (ready, w0) = wait_panel_settled(&win, dock, original.len(), Duration::from_secs(30));
    check(
        ready,
        "页面就绪：窗口宽度已按面板尺寸上报（不是建窗初值）",
        format!("窗口宽 {w0} 物理像素，{} 个图标", original.len()),
    );
    if !ready {
        log_info!("  页面没就绪，后面的用例无法判定，跳过");
        log_info!("  小结：{pass} 项通过，{fail} 项失败\n==============================================\n");
        return;
    }
    let dock_rect = window_rect(dock);

    /// 从页面读出行宽（`.dock-row` 的宽度 = 「各图标宽 + 间距」的精确值）
    fn row_width(win: &tauri::WebviewWindow) -> i64 {
        eval_in(
            win,
            "Math.round(document.querySelector('.dock-row').getBoundingClientRect().width)",
        )
        .trim()
        .parse()
        .unwrap_or(-1)
    }

    /// 从页面读出**相邻两项之间的布局间距**（逻辑像素）。
    ///
    /// 为什么要真读：间距现在**不是常数** —— 图形留白多的图标（回收站 3.6）
    /// 会被扣掉留白，插入分割线时"顶掉"的那个间距取决于它两边是谁。
    /// 写死一个数字的断言会随图标列表变化而失效（这正是本轮踩到的）。
    fn layout_gaps(win: &tauri::WebviewWindow) -> Vec<f64> {
        let v = eval_in(
            win,
            r#"JSON.stringify([...document.querySelectorAll('.dock-icon')].map(function (e) {
  const r = e.getBoundingClientRect();
  return [r.left, r.right];
}))"#,
        );
        match serde_json::from_str::<Vec<[f64; 2]>>(&v) {
            Ok(a) => (0..a.len().saturating_sub(1))
                .map(|i| a[i + 1][0] - a[i][1])
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    // ---------------- 1. 加一条分割线（第 1 项之后） ----------------
    let dock_w_before = dock_rect.right - dock_rect.left;
    let row_w_before = row_width(&win);
    // 记下**将被分割线顶掉**的那个间距（第 0 项与第 1 项之间）
    let replaced_gap = layout_gaps(&win).first().copied().unwrap_or(10.0);
    let anchor_id = original[0].id.clone();
    let sep_id = match crate::store::add_separator(app, Some(&anchor_id)) {
        Ok(id) => id,
        Err(e) => {
            log_info!("  添加分割线失败: {e}");
            return;
        }
    };
    crate::drop::refresh_apps(app);
    let (settled, dock_w_after) =
        wait_panel_settled(&win, dock, original.len() + 1, Duration::from_secs(20));
    check(
        settled,
        "加了分割线之后页面重新上报了尺寸",
        format!("窗口宽 {dock_w_after} 物理像素"),
    );
    let row_w_after = row_width(&win);

    let after_add = list();
    check(
        after_add.len() == original.len() + 1
            && after_add.get(1).map(|p| p.id == sep_id).unwrap_or(false)
            && after_add[1].separator,
        "分割线插在右键的那一项**后面**",
        format!(
            "{} 项 → {} 项；第 1 项 = {}（separator={}）",
            original.len(),
            after_add.len(),
            after_add[1].id,
            after_add[1].separator
        ),
    );

    let persisted = std::env::var("DOCK_CONFIG_DIR")
        .ok()
        .map(|d| std::path::Path::new(&d).join("config.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.contains(&sep_id))
        .unwrap_or(false);
    check(persisted, "分割线已经落盘", format!("配置里含 {sep_id}"));

    // 行宽增量 = 分割线宽 9 + 两侧紧间距 4+4 − **被顶掉的那个间距**。
    // 被顶掉的间距不是常数（见 `layout_gaps` 的说明），所以这里按量到的值算期望，
    // 而不是硬写 7。
    let expected_delta = 9.0 + 4.0 + 4.0 - replaced_gap;
    let delta = (row_w_after - row_w_before) as f64;
    check(
        (delta - expected_delta).abs() <= 1.5,
        "行宽增加 = 分割线宽 9 + 两侧紧间距 4+4 − 被顶掉的间距",
        format!(
            "行宽 {row_w_before} → {row_w_after}（+{delta}，期望 {expected_delta:.1}；\
             被顶掉的间距量得 {replaced_gap:.1}）"
        ),
    );
    // 窗口宽度必须跟着面板走。注意**不能**要求它等于行宽增量×缩放：
    // 面板宽度取的是「放大到最大时的行宽」的最大值，多一个条目会让峰值落在别的
    // 光标位置上，所以两者差几个像素是正常的。这条断言要管的是
    // 「窗口有没有跟着面板一起变」，不是逐像素复算几何。
    let grew = dock_w_after - dock_w_before;
    check(
        grew > 0 && (grew as f64) < 7.0 * scale + 10.0 && settled,
        "窗口宽度跟着面板一起变宽（窗口必须与面板严丝合缝）",
        format!(
            "窗口宽 {dock_w_before} → {dock_w_after}（+{grew} 物理像素，行宽 +7 逻辑）"
        ),
    );

    // 渲染：**刚加的那一条**是竖线，宽度 = SEP_W，没有图标、没有白点。
    //
    // ⚠️ 断言必须**相对**（"第 sep_pos 个是分割线"），不能写"全 Dock 只有一条分割线"——
    // 用户的配置里本来就可能有分割线（本轮就踩到了：用户自己加过一条，
    // 于是 `n == 1` 这种断言在他机器上必挂）。
    let sep_pos = after_add
        .iter()
        .position(|p| p.id == sep_id)
        .unwrap_or(1);
    let sep_count = after_add.iter().filter(|p| p.separator).count();
    //
    // 「这条线在**渲染出来的底色**上到底看不看得见」查不了 —— 那需要读屏幕像素，
    // 而这个自检不开截图。人工量过的数字记在 `docs/backlog.md` BL-5：
    // 底色 #797980(121)、线 178，差 57；那条"换成中性灰就完全看不见"的教训也在那里。
    let dom = eval_in(
        &win,
        &format!(
            r#"JSON.stringify((function () {{
  const all = [...document.querySelectorAll('.dock-icon')];
  const s = all[{sep_pos}];
  const l = s ? s.querySelector('.dock-sep-line') : null;
  const lb = l ? l.getBoundingClientRect() : null;
  return {{
    n: document.querySelectorAll('.dock-sep').length,
    isSep: !!(s && s.classList.contains('dock-sep')),
    w: s ? Math.round(s.getBoundingClientRect().width) : -1,
    line: !!l,
    lineW: lb ? Math.round(lb.width * 100) / 100 : -1,
    lineH: lb ? Math.round(lb.height) : -1,
    lineBg: l ? getComputedStyle(l).backgroundColor : '',
    dot: s ? !!s.querySelector('.dock-dot') : true,
    img: s ? !!s.querySelector('img') : true
  }};
}})())"#
        ),
    );
    let d: serde_json::Value = serde_json::from_str(&dom).unwrap_or(serde_json::Value::Null);
    let line_bg = d["lineBg"].as_str().unwrap_or("");
    check(
        d["n"] == sep_count
            && d["isSep"] == true
            && d["w"] == 9
            && d["line"] == true
            && d["dot"] == false
            && d["img"] == false,
        "刚加的那条渲染成竖线：位置对、宽度 9、无图标无白点",
        format!("第 {sep_pos} 项（共 {sep_count} 条分割线）实得 {dom}"),
    );
    // 线本身必须：1 逻辑像素宽、竖直方向有长度、**颜色是浅色**。
    // 最后一条是回归闸：曾经用过中性灰 rgba(120,120,128,.5)，它与渲染出来的
    // 底色（#797980）几乎一样 —— 元素存在、宽度也对，但肉眼完全看不见。
    check(
        d["lineW"].as_f64() == Some(1.0)
            && d["lineH"].as_i64().unwrap_or(0) >= 10
            && line_bg.starts_with("rgba(255, 255, 255"),
        "线本身：1px 宽、有高度、用浅色（不是会融进底色的中性灰）",
        format!("宽={} 高={} 颜色={line_bg}", d["lineW"], d["lineH"]),
    );

    // ---------------- 2. 右键：菜单窗口真的弹出来了 ----------------
    let fg_before = unsafe { GetForegroundWindow() };
    let Some((cursor_x, _cursor_y)) = right_click_icon(&win, dock, scale, 0) else {
        log_info!("  取图标位置失败，跳过菜单用例");
        restore_separators(app, &original);
        return;
    };
    std::thread::sleep(Duration::from_millis(900));

    let visible = crate::panel_window::is_visible();
    let mrect = window_rect(menu);
    check(
        visible,
        "右键图标 → 独立菜单窗口已显示",
        format!(
            "is_visible={visible}  菜单矩形 ({},{}) {}x{}",
            mrect.left,
            mrect.top,
            mrect.right - mrect.left,
            mrect.bottom - mrect.top
        ),
    );

    check(
        mrect.bottom <= dock_rect.top && mrect.top >= wa.top,
        "菜单贴在 Dock **上方**且在屏内",
        format!(
            "菜单底 {} ≤ Dock 顶 {}；菜单顶 {} ≥ 工作区顶 {}",
            mrect.bottom, dock_rect.top, mrect.top, wa.top
        ),
    );

    // 锚点：菜单水平中心应落在光标附近（锚点就是右键那一刻的光标）
    let center_delta = (mrect.left + mrect.right) / 2 - cursor_x;
    check(
        center_delta.abs() <= (4.0 * scale) as i32,
        "菜单以光标为中心水平锚定",
        format!(
            "菜单中心 {} vs 光标 {cursor_x}（差 {center_delta} 物理像素）",
            (mrect.left + mrect.right) / 2
        ),
    );

    // 焦点没有被菜单抢走（WS_EX_NOACTIVATE 的硬要求）
    let fg_after = unsafe { GetForegroundWindow() };
    let ex = unsafe { GetWindowLongPtrW(menu, GWL_EXSTYLE) as u32 };
    check(
        fg_after == fg_before && fg_after != menu,
        "菜单没有抢走前台焦点",
        format!(
            "前台 {} → {}；菜单扩展样式 = {}",
            describe(fg_before),
            describe(fg_after),
            describe_ex_style(ex)
        ),
    );

    // ---------------- 3. 菜单内容与 Rust 算的几何一致 ----------------
    let items = crate::panel_window::current_menu_items().expect("浮层已显示却没有菜单项");
    let want_items = items.iter().filter(|i| !i.divider).count();
    let want_div = items.iter().filter(|i| i.divider).count();
    let payload_dom = eval_in(
        &menu_win,
        r#"JSON.stringify((function () {
  const m = document.querySelector('.panel');
  return {
    n: document.querySelectorAll('.menu-item').length,
    div: document.querySelectorAll('.menu-divider').length,
    labels: [...document.querySelectorAll('.menu-item')].map(function (e) { return e.textContent; }),
    scroll: m.scrollHeight,
    client: m.clientHeight,
    w: Math.round(m.getBoundingClientRect().width)
  };
})())"#,
    );
    let pd: serde_json::Value =
        serde_json::from_str(&payload_dom).unwrap_or(serde_json::Value::Null);
    check(
        pd["n"] == want_items && pd["div"] == want_div,
        "菜单项数量与 Rust 给的一致",
        format!("页面 {}/{want_items} 项、分隔 {}/{}", pd["n"], pd["div"], want_div),
    );
    // ❗这条是「Rust 算窗口尺寸」与「页面按同一组数字渲染」之间唯一的一致性检查：
    // 一旦两边的高度算法不一致，scrollHeight 就会大于 clientHeight（最后一行被裁掉）
    check(
        pd["scroll"].as_i64().unwrap_or(-1) <= pd["client"].as_i64().unwrap_or(-2),
        "菜单高度没有溢出（Rust 的尺寸算法与页面渲染一致）",
        format!("scrollHeight={} ≤ clientHeight={}", pd["scroll"], pd["client"]),
    );
    let labels: Vec<String> = pd["labels"]
        .as_array()
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    log_info!("  菜单内容: {}", labels.join(" | "));

    // ---------------- 4. 同一个图标再右键一次 = 关掉 ----------------
    let Some((tx2, ty2)) = right_click_icon(&win, dock, scale, 0) else {
        log_info!("  取图标位置失败");
        restore_separators(app, &original);
        return;
    };
    std::thread::sleep(Duration::from_millis(700));
    let toggled_off = !crate::panel_window::is_visible();
    check(
        toggled_off,
        "同一个图标再次右键 → 菜单关闭（右键是开关）",
        format!(
            "点 ({tx2},{ty2}) 后 is_visible={}，当前目标={:?}",
            crate::panel_window::is_visible(),
            crate::panel_window::current_target_id()
        ),
    );

    // ---------------- 5. Esc 关闭 ----------------
    right_click_icon(&win, dock, scale, 0);
    std::thread::sleep(Duration::from_millis(800));
    let opened = crate::panel_window::is_visible();
    let n_esc = key_press(VK_ESCAPE);
    std::thread::sleep(Duration::from_millis(500));
    check(
        opened && !crate::panel_window::is_visible(),
        "Esc 关闭菜单",
        format!("打开={opened} 注入 {n_esc} 段后 is_visible={}", crate::panel_window::is_visible()),
    );

    // ---------------- 6. 点菜单和 Dock 以外的地方 → 关闭 ----------------
    right_click_icon(&win, dock, scale, 0);
    std::thread::sleep(Duration::from_millis(800));
    let opened = crate::panel_window::is_visible();
    let (ox, oy) = center(outside);
    click_at(ox, oy);
    std::thread::sleep(Duration::from_millis(500));
    check(
        opened && !crate::panel_window::is_visible(),
        "点菜单和 Dock 以外的地方 → 菜单关闭",
        format!("打开={opened} 点到 ({ox},{oy}) 后 is_visible={}", crate::panel_window::is_visible()),
    );

    // ---------------- 7. 真的点一个菜单项：动作被执行 ----------------
    let before_pick = list();
    right_click_icon(&win, dock, scale, 0);
    std::thread::sleep(Duration::from_millis(800));
    let mrect = window_rect(menu);
    let add_idx = items
        .iter()
        .filter(|i| !i.divider)
        .position(|i| i.id == crate::menu::id::ADD_SEP);
    match add_idx.and_then(|i| menu_item_point(&menu_win, &mrect, scale, i)) {
        Some((px, py)) => {
            click_at(px, py);
            std::thread::sleep(Duration::from_millis(900));
            let after_pick = list();
            check(
                !crate::panel_window::is_visible(),
                "选了菜单项之后菜单立刻关闭",
                format!("点 ({px},{py}) 后 is_visible={}", crate::panel_window::is_visible()),
            );
            check(
                after_pick.len() == before_pick.len() + 1
                    && after_pick
                        .get(1)
                        .map(|p| p.separator && p.id != sep_id)
                        .unwrap_or(false),
                "点「添加分割线」真的在那一项后面加了一条",
                format!(
                    "{} 项 → {} 项；第 1 项 separator={}",
                    before_pick.len(),
                    after_pick.len(),
                    after_pick.get(1).map(|p| p.separator).unwrap_or(false)
                ),
            );
        }
        None => {
            check(
                false,
                "取菜单项「添加分割线」的位置",
                "页面里找不到这一项".into(),
            );
            crate::panel_window::hide_all();
        }
    }

    // ---------------- 8. 分割线自己的菜单：只有「移除分割线」 ----------------
    let cur = list();
    let sep_pos = cur.iter().position(|p| p.separator);
    if let Some(sp) = sep_pos {
        // 列表变了，窗口宽度也变了 —— 重新取一次图标位置
        if right_click_icon(&win, dock, scale, sp).is_some() {
            std::thread::sleep(Duration::from_millis(800));
            let sep_items = crate::panel_window::current_menu_items().unwrap_or_default();
            let only_remove = sep_items.len() == 1 && sep_items[0].id == crate::menu::id::REMOVE;
            check(
                only_remove,
                "右键分割线：菜单只有「移除分割线」",
                format!(
                    "项数={} 首项={:?}",
                    sep_items.len(),
                    sep_items.first().map(|i| i.label.clone())
                ),
            );

            // 顺便把这一条通过菜单移除掉 —— 验证分割线能被右键移除
            let mrect2 = window_rect(menu);
            if let Some((px, py)) = menu_item_point(&menu_win, &mrect2, scale, 0) {
                click_at(px, py);
                std::thread::sleep(Duration::from_millis(900));
                let after = list();
                check(
                    after.len() == cur.len() - 1 && !after.iter().any(|p| p.id == cur[sp].id),
                    "点「移除分割线」把它从 Dock 上删掉了",
                    format!("{} 项 → {} 项", cur.len(), after.len()),
                );
            }
        }
    } else {
        log_info!("  （没有分割线可测右键菜单）");
    }

    // ---------------- 9. 大文件夹（程序坞文件夹） ----------------
    //
    // 全部走**真实交互**：右键菜单建文件夹 → 点图标展开 → 拖图标进去 →
    // 右键看菜单 → 解散。每一步都要"点下去"才算验证过（Rust 里的函数能跑通
    // 不代表界面接对了，本轮的教训）。
    log_info!("\n  ---- 大文件夹 ----");
    {
        let cur = list();
        let top_ids: Vec<String> = cur.iter().map(|p| p.id.clone()).collect();
        // 找一个顶层条目当文件夹的第一个成员。
        //
        // ❗**优先挑系统位置**（此电脑 / 回收站）：下面要验「面板里右键这个孩子 → 它自己的菜单」
        // 和「点它真的能打开」，而系统位置是唯一"点了必定开一个可断言、可关闭的资源管理器窗口"
        // 的条目。挑普通应用的话，这两个用例会随"用户当时开着什么"而跳过
        // （临时文件夹加进最左侧之后真的跳过了一次 —— 它是个目录，不是系统位置）。
        let Some(first_pos) = cur
            .iter()
            .position(|p| !p.separator && !p.is_folder && crate::apps::is_shell_location(&p.target))
            .or_else(|| {
                cur.iter()
                    .position(|p| !p.separator && !p.is_folder && !p.target.is_empty())
            })
        else {
            log_info!("  （没有可用的顶层条目，跳过大文件夹用例）");
            check(false, "大文件夹用例", "找不到非空的条目".into());
            restore_separators(app, &original);
            return;
        };

        // ---- 9.1 右键 → 新建文件夹 ----
        right_click_icon(&win, dock, scale, first_pos);
        std::thread::sleep(Duration::from_millis(800));
        let mrect = window_rect(menu);
        let make_idx = crate::panel_window::current_menu_items()
            .unwrap_or_default()
            .iter()
            .filter(|i| !i.divider)
            .position(|i| i.id == crate::menu::id::MAKE_FOLDER);
        match make_idx.and_then(|i| menu_item_point(&menu_win, &mrect, scale, i)) {
            Some((px, py)) => {
                click_at(px, py);
                // 点完之后会弹**名字输入框**（`prompt_window.rs` 的自绘窗口）—— 自检要驱动它：
                // 等窗口出现 → 把输入框改成"自检文件夹" → 点「确定」。这一条同时验证了
                // 「弹窗真的会弹」和「敲进去的名字真的落到了配置里」。
                match accept_name_prompt(app, Some(TEST_FOLDER_NAME)) {
                    Some(used) => {
                        std::thread::sleep(Duration::from_millis(900));
                        let after = list();
                        let named = after
                            .iter()
                            .find(|p| p.is_folder)
                            .map(|f| f.display_name.clone())
                            .unwrap_or_default();
                        check(
                            named == used,
                            "「新建文件夹」会弹名字输入框，敲进去的名字真的用上了",
                            format!("输入框用的是 {used:?}，配置里的文件夹叫 {named:?}"),
                        );
                    }
                    None => {
                        check(
                            false,
                            "「新建文件夹」会弹名字输入框",
                            "3 秒内没等到名字输入框窗口（Dock 命名）".into(),
                        );
                        restore_separators(app, &original);
                        return;
                    }
                }
            }
            None => {
                check(false, "菜单里有「新建文件夹」", "找不到这一项".into());
                restore_separators(app, &original);
                return;
            }
        }
        let after_make = list();
        let folder_pos = after_make.iter().position(|p| p.is_folder);
        check(
            folder_pos.is_some()
                && after_make
                    .iter()
                    .find(|p| p.is_folder)
                    .map(|f| f.children.len() == 1 && f.children[0].id == top_ids[first_pos])
                    .unwrap_or(false),
            "右键「新建文件夹」：原图标**原位**变成文件夹，自己成了里面第一个",
            format!(
                "下标 {first_pos} 处现在是 folder={}，里面有 {} 项",
                after_make.get(first_pos).map(|p| p.is_folder).unwrap_or(false),
                after_make
                    .get(first_pos)
                    .map(|p| p.children.len())
                    .unwrap_or(0)
            ),
        );

        // ---- 9.2 点文件夹图标 → 展开浮层 ----
        let fp = folder_pos.unwrap_or(first_pos);
        let folder_id = list()[fp].id.clone();
        // 等 Dock 页面重画（条目数没变，尺寸也没变，所以要靠轮询自己刷新）
        std::thread::sleep(Duration::from_millis(1600));
        if right_click_icon(&win, dock, scale, fp).is_none() {
            log_info!("  （取不到文件夹图标位置）");
        }
        // 上一步是右键（会弹菜单）—— 先关掉，再改成"左键点图标"
        crate::panel_window::hide_all();
        std::thread::sleep(Duration::from_millis(300));
        {
            let c = icon_centers(&win, &window_rect(dock), scale).unwrap_or_default();
            if let Some(&(cx, cy)) = c.get(fp) {
                mouse_abs(cx, cy);
                std::thread::sleep(Duration::from_millis(250));
                click_at(cx, cy);
                std::thread::sleep(Duration::from_millis(900));
            }
        }
        let folder_visible = crate::panel_window::folder_visible();
        let items_in_panel = crate::panel_window::current_folder_items().unwrap_or_default();
        check(
            folder_visible && items_in_panel.len() == 1 && items_in_panel[0].id == top_ids[first_pos],
            "点文件夹图标 → 展开浮层，网格里就是里面那一项",
            format!(
                "folder_visible={folder_visible} 网格 {} 项 {:?}",
                items_in_panel.len(),
                items_in_panel.first().map(|i| i.label.clone())
            ),
        );

        // ---- 悬停名字：文件夹里那一格也要"上方气泡"（不是原生 tooltip）----
        //
        // 用户明确要求过：名字提示要像 macOS 那样**在图标上方**显示，不要原生 tooltip。
        // 文件夹格子里的名字走的是同一条路（复用菜单层窗口的 `Label` 形态），
        // 但**定位基准是文件夹面板自己** —— 拿 Dock 当基准的话气泡会跑到屏幕底部去。
        {
            crate::panel_window::hide_all();
            std::thread::sleep(Duration::from_millis(300));
            // 先把光标挪远（清掉上一次的悬停状态），否则"已经在显示同一个"会直接返回 true
            mouse_abs(wa.left + 40, wa.top + 40);
            std::thread::sleep(Duration::from_millis(300));
            let c = icon_centers(&win, &window_rect(dock), scale).unwrap_or_default();
            if let Some(&(cx, cy)) = c.get(fp) {
                click_at(cx, cy);
                std::thread::sleep(Duration::from_millis(900));
            }
            let prect = window_rect(folder);
            let item = folder_item_point(&folder_win, &prect, scale, 0);
            match item {
                Some((px, py)) => {
                    // 只把光标**移上去**，不点：这是悬停
                    mouse_abs(px, py);
                    std::thread::sleep(Duration::from_millis(700));
                    let lrect = window_rect(menu);
                    let visible = crate::panel_window::label_visible();
                    let above = lrect.bottom <= prect.top;
                    let centered = (lrect.left + lrect.right) / 2 - px;
                    check(
                        visible && above && centered.abs() <= 25,
                        "文件夹里悬停一格 → 名字气泡出现在**面板上方**（不是原生 tooltip）",
                        format!(
                            "label_visible={visible} 气泡 ({},{})-({},{}) 面板顶 {} 水平偏差 {}px",
                            lrect.left, lrect.top, lrect.right, lrect.bottom, prect.top, centered
                        ),
                    );
                }
                None => check(false, "文件夹里悬停一格 → 名字气泡", "取不到格子坐标".into()),
            }
            crate::panel_window::hide_all();
            std::thread::sleep(Duration::from_millis(300));
        }

        // ---- 运行点的规矩（用户明确要求过，别改回去）----
        //   文件夹**自己**不点：它不是应用、也没有窗口；
        //   里面**在跑的那一格**要点，否则展开之后看不出哪个开着。
        {
            let dot_dom = eval_in(
                &win,
                &format!(
                    r#"JSON.stringify((function () {{
  const el = [...document.querySelectorAll('.dock-icon')][{fp}];
  return {{ folderDot: !!(el && el.querySelector('.dock-dot')) }};
}})())"#
                ),
            );
            let fd: serde_json::Value =
                serde_json::from_str(&dot_dom).unwrap_or(serde_json::Value::Null);
            check(
                fd["folderDot"] == false,
                "文件夹图标上**没有**白点（白点是应用的，不是文件夹的）",
                format!("实得 {dot_dom}"),
            );

            let panel_dots = eval_in(
                &folder_win,
                r#"JSON.stringify([...document.querySelectorAll('.folder-item')].map(function (e) {
  return !!e.querySelector('.folder-dot');
}))"#,
            );
            let want: Vec<bool> = items_in_panel.iter().map(|i| i.running).collect();
            let got: Vec<bool> = serde_json::from_str(&panel_dots).unwrap_or_default();
            check(
                got == want,
                "文件夹面板里：**在跑的那一格**有点、没在跑的不点",
                format!("页面 {got:?} vs Rust 给的运行态 {want:?}"),
            );
        }

        // 浮层不是"普通弹窗"：它和 Dock 同材质、贴在 Dock 上方、不抢焦点
        let prect = window_rect(folder);
        let dr = window_rect(dock);
        check(
            prect.bottom <= dr.top && prect.bottom > dr.top - 40,
            "文件夹面板贴在 Dock **上方**（像是从图标里长出来的）",
            format!("面板底 {} ≤ Dock 顶 {}", prect.bottom, dr.top),
        );
        crate::panel_window::hide_all();
        std::thread::sleep(Duration::from_millis(300));

        // ---- 9.3 把另一个图标**拖到文件夹上** → 进文件夹 ----
        //
        // 优先挑一个**当前在跑**的顶层应用当被拖对象：这样下面"在跑的那一格有点"
        // 才是真的在验证（拖一个没在跑的进去，两组都是 false，断言等于没写）。
        let before_drag = list();
        let running_ids: Vec<String> = crate::apps::enumerate_apps()
            .iter()
            .map(|a| a.id.clone())
            .collect();
        let candidates = |only_running: bool| -> Option<usize> {
            before_drag.iter().position(|p| {
                !p.separator
                    && !p.is_folder
                    && !p.target.is_empty()
                    && p.id != before_drag[fp].id
                    && (!only_running || running_ids.contains(&p.id))
            })
        };
        let drag_from = candidates(true).or_else(|| candidates(false));
        // 记住"拖进去的那个到底在不在跑"：`candidates(true)` 命中就说明挑到了在跑的，
        // 下面那条"运行点必须点亮"的断言才有意义（挑不到就只能验一致性，见 2960 附近）。
        let dragged_running = drag_from
            .map(|df| before_drag.get(df).map(|p| running_ids.contains(&p.id)).unwrap_or(false))
            .unwrap_or(false);
        if let Some(df) = drag_from {
            if df != fp {
                let c = icon_centers(&win, &window_rect(dock), scale).unwrap_or_default();
                if let (Some(&(x0, y0)), Some(&(x1, y1))) = (c.get(df), c.get(fp)) {
                    drag_from_to(x0, y0, x1, y1, 10);
                    std::thread::sleep(Duration::from_millis(900));
                }
                let after_drag = list();
                check(
                    after_drag.iter().filter(|p| !p.separator).count() == before_drag.iter().filter(|p| !p.separator).count() - 1
                        && after_drag
                            .iter()
                            .find(|p| p.id == folder_id)
                            .map(|f| f.children.len() == 2)
                            .unwrap_or(false),
                    "把图标拖到文件夹上 → 进了文件夹（顶层少一个，文件夹里多一个）",
                    format!(
                        "文件夹里 {} 项",
                        after_drag
                            .iter()
                            .find(|p| p.id == folder_id)
                            .map(|f| f.children.len())
                            .unwrap_or(0)
                    ),
                );

                // 再展开一次：这时候里面有一个**在跑**的，运行点必须点出来
                std::thread::sleep(Duration::from_millis(1700));
                let fp3 = list().iter().position(|p| p.id == folder_id).unwrap_or(fp);
                {
                    let c = icon_centers(&win, &window_rect(dock), scale).unwrap_or_default();
                    if let Some(&(cx, cy)) = c.get(fp3) {
                        click_at(cx, cy);
                        std::thread::sleep(Duration::from_millis(900));
                    }
                }
                let items2 = crate::panel_window::current_folder_items().unwrap_or_default();
                let want2: Vec<bool> = items2.iter().map(|i| i.running).collect();
                let dots = eval_in(
                    &folder_win,
                    r#"JSON.stringify([...document.querySelectorAll('.folder-item')].map(function (e) {
  return !!e.querySelector('.folder-dot');
}))"#,
                );
                let got2: Vec<bool> = serde_json::from_str(&dots).unwrap_or_default();
                // 「本次含在跑的应用」以前是**硬要求**（`want2.iter().any()`）——
                // 但拖进去的那个应用在不在跑取决于用户当时开着什么：什么都没开时
                // 这条必然报红，而它想验的东西（页面与 Rust 一致）其实是对的。
                // 所以：一致性**永远**断言；"至少有一个点在亮"只在真的拖进了在跑的应用时才要求。
                let has_running = want2.iter().any(|x| *x);
                check(
                    !want2.is_empty() && got2 == want2 && (has_running || !dragged_running),
                    if dragged_running {
                        "展开后：**在跑的那一格**有点、没在跑的不点（本次含在跑的应用）"
                    } else {
                        "展开后：运行点与 Rust 的运行态一致（本次没挑到在跑的应用，只验一致性）"
                    },
                    format!("页面 {got2:?} vs Rust 给的运行态 {want2:?}（拖进去的在跑={dragged_running}）"),
                );

                // ---- 9.4 面板里**右键**一个图标 → 弹的是它自己的菜单 ----
                //
                // 挑一个 target 是**系统位置**的孩子：它一定会开一个
                // 可断言、可关闭的资源管理器窗口（见下面 9.5 的"点一下真的打开了吗"）。
                let sys_idx = items2.iter().position(|i| {
                    crate::apps::is_shell_location(&i.target)
                });
                if let Some(si) = sys_idx {
                    let prect2 = window_rect(folder);
                    if let Some((px, py)) = folder_item_point(&folder_win, &prect2, scale, si) {
                        right_click_at(px, py);
                        std::thread::sleep(Duration::from_millis(900));
                        let child_menu = crate::panel_window::current_menu_items().unwrap_or_default();
                        let labels: Vec<String> =
                            child_menu.iter().map(|i| i.label.clone()).collect();
                        check(
                            labels.iter().any(|l| l == "移出文件夹"),
                            "面板里右键图标 → 弹的是**它自己的菜单**（含「移出文件夹」）",
                            format!("实得 {labels:?}"),
                        );

                        // 被右键的这个孩子是**回收站**（挑就是挑它）→ 菜单里该有「清空回收站」，
                        // 而且是"不空才可点"。
                        //
                        // ⚠️ 这里**绝不点它**：那是永久删除用户回收站里的东西。
                        // 我们只验"菜单里有没有、状态对不对"。
                        {
                            let bin = child_menu
                                .iter()
                                .find(|i| i.id == crate::menu::id::EMPTY_BIN);
                            let n = crate::apps::recycle_bin_items();
                            check(
                                bin.is_some() && bin.map(|b| b.enabled) == Some(n > 0),
                                "回收站的右键菜单有「清空回收站」（空的时候置灰）",
                                format!(
                                    "回收站 {n} 项；菜单项={:?}（**刻意不点它**：会永久删掉里面的东西）",
                                    bin.map(|b| (b.label.clone(), b.enabled))
                                ),
                            );
                        }

                        // ---- 两层叠加：菜单盖在文件夹上，**逐层关闭** ----
                        //
                        // 用户要求：在文件夹里右键，**文件夹不该关**；
                        // 点外面先关菜单，再点一次才关文件夹。
                        check(
                            crate::panel_window::is_visible() && crate::panel_window::folder_visible(),
                            "在文件夹里右键 → 菜单和文件夹**同时开着**（文件夹没被关掉）",
                            format!(
                                "菜单={} 文件夹={}",
                                crate::panel_window::is_visible(),
                                crate::panel_window::folder_visible()
                            ),
                        );

                        // 第一次点外面：只关菜单
                        //
                        // ⚠️ 断言的是**结果**，不是"点一次就够"：合成点击偶尔会被吃掉一次
                        // （实测：同一段代码多数时候一次就关，偶尔第一次无效 ——
                        //  而 Esc 那条路每次都准，说明分层逻辑没问题）。
                        // 所以最多点两次，但要求**仍然是**"菜单关上、文件夹还开着" ——
                        // 真正要防的回归是"一次把两层都关掉"（那就没有分层了）。
                        let (ox, oy) = center(outside);
                        let mut clicks = 0;
                        for _ in 0..2 {
                            click_at(ox, oy);
                            clicks += 1;
                            std::thread::sleep(Duration::from_millis(700));
                            if !crate::panel_window::is_visible() {
                                break;
                            }
                        }
                        check(
                            !crate::panel_window::is_visible() && crate::panel_window::folder_visible(),
                            "点菜单外面 → **只关菜单**，文件夹还开着",
                            format!(
                                "点了 {clicks} 次；菜单={} 文件夹={}",
                                crate::panel_window::is_visible(),
                                crate::panel_window::folder_visible()
                            ),
                        );

                        // 第二次点外面：这时菜单没了，才轮到文件夹
                        let mut clicks2 = 0;
                        for _ in 0..2 {
                            click_at(ox, oy);
                            clicks2 += 1;
                            std::thread::sleep(Duration::from_millis(700));
                            if !crate::panel_window::folder_visible() {
                                break;
                            }
                        }
                        check(
                            !crate::panel_window::is_visible() && !crate::panel_window::folder_visible(),
                            "再点一次外面 → 文件夹才关掉（逐层关闭）",
                            format!(
                                "点了 {clicks2} 次；菜单={} 文件夹={}",
                                crate::panel_window::is_visible(),
                                crate::panel_window::folder_visible()
                            ),
                        );

                        // Esc 也要逐层：先关菜单、再关文件夹
                        {
                            let c = icon_centers(&win, &window_rect(dock), scale).unwrap_or_default();
                            if let Some(&(cx, cy)) = c.get(fp3) {
                                click_at(cx, cy);
                                std::thread::sleep(Duration::from_millis(900));
                            }
                            let prect_e = window_rect(folder);
                            if let Some((px, py)) =
                                folder_item_point(&folder_win, &prect_e, scale, si)
                            {
                                right_click_at(px, py);
                                std::thread::sleep(Duration::from_millis(900));
                            }
                            key_press(VK_ESCAPE);
                            std::thread::sleep(Duration::from_millis(600));
                            let after_esc1 = (
                                crate::panel_window::is_visible(),
                                crate::panel_window::folder_visible(),
                            );
                            key_press(VK_ESCAPE);
                            std::thread::sleep(Duration::from_millis(600));
                            let after_esc2 = (
                                crate::panel_window::is_visible(),
                                crate::panel_window::folder_visible(),
                            );
                            check(
                                after_esc1 == (false, true) && after_esc2 == (false, false),
                                "Esc 也是逐层：先关菜单、再关文件夹",
                                format!("第一次后 {after_esc1:?}，第二次后 {after_esc2:?}"),
                            );
                        }

                        // ---- 9.5 点一下：**真的要能打开** ----
                        //
                        // 这条是给"点文件夹里的图标没反应"那个 bug 立的闸：
                        // 当时 `panel_launch` 把**应用 id** 当成菜单项 id 传下去，
                        // 动作层报"未知的菜单项"，界面上就是点了没反应。
                        crate::panel_window::hide_all();
                        std::thread::sleep(Duration::from_millis(400));
                        // 重新展开（关掉之后浮层没了，得再点一次文件夹图标）
                        {
                            let c = icon_centers(&win, &window_rect(dock), scale)
                                .unwrap_or_default();
                            if let Some(&(cx, cy)) = c.get(fp3) {
                                click_at(cx, cy);
                                std::thread::sleep(Duration::from_millis(900));
                            }
                        }
                        let prect3 = window_rect(folder);
                        let before_win = explorer_hwnds();
                        if let Some((px, py)) = folder_item_point(&folder_win, &prect3, scale, si) {
                            click_at(px, py);
                            std::thread::sleep(Duration::from_millis(1500));
                        }
                        let after_win = explorer_hwnds();
                        let name = items2[si].label.clone();
                        let opened = after_win.len() > before_win.len()
                            || after_win.iter().any(|(_, t)| t.contains(&name));
                        check(
                            opened,
                            "点文件夹里的图标 → 真的打开了（不是点了没反应）",
                            format!(
                                "「{name}」资源管理器窗口 {} → {}",
                                before_win.len(),
                                after_win.len()
                            ),
                        );
                        // 只关掉测试开出来的那个窗口
                        let mut closed = 0;
                        for (h, t) in &after_win {
                            if !before_win.iter().any(|(b, _)| b == h) {
                                let _ = crate::apps::close_window(HWND(
                                    *h as *mut std::ffi::c_void
                                ));
                                closed += 1;
                                log_info!("  （已关闭测试窗口 0x{h:X} '{t}'）");
                            }
                        }
                        let _ = closed;
                        std::thread::sleep(Duration::from_millis(600));

                        // ---- 9.6 「移出文件夹」= 挪到顶层，**不是删掉** ----
                        {
                            let c = icon_centers(&win, &window_rect(dock), scale)
                                .unwrap_or_default();
                            if let Some(&(cx, cy)) = c.get(fp3) {
                                click_at(cx, cy);
                                std::thread::sleep(Duration::from_millis(900));
                            }
                        }
                        let prect4 = window_rect(folder);
                        let items3 = crate::panel_window::current_folder_items().unwrap_or_default();
                        let si3 = items3
                            .iter()
                            .position(|i| i.id == items2[si].id)
                            .unwrap_or(0);
                        if let Some((px, py)) = folder_item_point(&folder_win, &prect4, scale, si3) {
                            right_click_at(px, py);
                            std::thread::sleep(Duration::from_millis(900));
                        }
                        let mv_menu = crate::panel_window::current_menu_items().unwrap_or_default();
                        let mv_idx = mv_menu
                            .iter()
                            .filter(|i| !i.divider)
                            .position(|i| i.id == crate::menu::id::MOVE_OUT);
                        let mrect_mv = window_rect(menu);
                        if let Some((px, py)) =
                            mv_idx.and_then(|i| menu_item_point(&menu_win, &mrect_mv, scale, i))
                        {
                            let before_move = list();
                            let child_id = items2[si].id.clone();
                            click_at(px, py);
                            std::thread::sleep(Duration::from_millis(900));
                            let after_move = list();
                            let n_before = before_move
                                .iter()
                                .find(|p| p.id == folder_id)
                                .map(|f| f.children.len())
                                .unwrap_or(0);
                            let n_after = after_move
                                .iter()
                                .find(|p| p.id == folder_id)
                                .map(|f| f.children.len())
                                .unwrap_or(0);
                            let top_now = after_move.iter().position(|p| p.id == child_id);
                            let folder_at = after_move.iter().position(|p| p.id == folder_id);
                            check(
                                n_after + 1 == n_before
                                    && top_now.is_some()
                                    && top_now == folder_at.map(|f| f + 1),
                                "「移出文件夹」把它挪到**顶层**（文件夹后面），不是删掉",
                                format!(
                                    "文件夹里 {n_before} → {n_after} 项；顶层下标 {top_now:?}（文件夹在下标 {folder_at:?}）"
                                ),
                            );
                        } else {
                            check(false, "点「移出文件夹」", "取不到菜单项位置".into());
                        }
                    }
                } else {
                    log_info!("  （文件夹里没有系统位置，跳过「点开 / 移出文件夹」用例）");
                }
                crate::panel_window::hide_all();
                std::thread::sleep(Duration::from_millis(300));
            }
        } else {
            log_info!("  （没有第二个可拖的顶层应用，跳过拖入文件夹用例）");
        }

        // ---- 9.4 右键文件夹 → 菜单是「解散 / 移出」 ----
        std::thread::sleep(Duration::from_millis(1600));
        let fp2 = list().iter().position(|p| p.id == folder_id).unwrap_or(fp);
        right_click_icon(&win, dock, scale, fp2);
        std::thread::sleep(Duration::from_millis(800));
        let folder_items = crate::panel_window::current_menu_items().unwrap_or_default();
        let labels: Vec<String> = folder_items.iter().map(|i| i.label.clone()).collect();
        let has_dissolve = labels.iter().any(|l| l.starts_with("解散文件夹"));
        let remove_label = labels
            .iter()
            .find(|l| l.starts_with("移出 Dock"))
            .cloned()
            .unwrap_or_default();
        let n_in_folder = list()
            .iter()
            .find(|p| p.id == folder_id)
            .map(|f| f.children.len())
            .unwrap_or(0);
        check(
            has_dissolve && !remove_label.is_empty(),
            "右键文件夹 → 菜单给「解散文件夹」和「移出 Dock」",
            format!("实得 {labels:?}"),
        );
        // 「移出 Dock」对文件夹来说是**连里面一起删**，文案必须写明有几项
        check(
            remove_label.contains(&n_in_folder.to_string()),
            "文件夹的「移出 Dock」写明了会连里面几项一起删（否则是静默的数据损失）",
            format!("文案 {remove_label:?}，里面实际 {n_in_folder} 项"),
        );

        // ---- 9.4b 右键文件夹 →「重命名…」→ 弹输入框 → 名字真的改掉 ----
        //
        // 这一段用的是**同一个**名字输入框（和新建时那个自绘窗口），
        // 只是默认值换成它现在的名字。
        {
            let renamed = "自检改名后";
            let rrect = window_rect(menu);
            let rename_idx = folder_items
                .iter()
                .filter(|i| !i.divider)
                .position(|i| i.id == crate::menu::id::RENAME);
            match rename_idx.and_then(|i| menu_item_point(&menu_win, &rrect, scale, i)) {
                Some((px, py)) => {
                    click_at(px, py);
                    match accept_name_prompt(app, Some(renamed)) {
                        Some(_) => {
                            std::thread::sleep(Duration::from_millis(900));
                            let now = list()
                                .iter()
                                .find(|p| p.id == folder_id)
                                .map(|f| f.display_name.clone())
                                .unwrap_or_default();
                            check(
                                now == renamed,
                                "右键文件夹 →「重命名…」：弹输入框，改完名字真的落盘",
                                format!("期望 {renamed:?}，实得 {now:?}"),
                            );
                        }
                        None => check(
                            false,
                            "右键文件夹 →「重命名…」会弹输入框",
                            "3 秒内没等到输入框窗口".into(),
                        ),
                    }
                }
                None => check(false, "文件夹菜单里有「重命名…」", "找不到这一项".into()),
            }
        }

        // ---- 9.5 解散：里面的东西回到顶层 ----
        //
        // ⚠️ 先把菜单**重新打开**：9.4b 点了「重命名…」，那一下把菜单关掉了。
        // 继续用上一次读到的坐标去点「解散文件夹」会点到菜单**下面**的东西
        // （Dock 图标 / 桌面）—— 那就可能顺手启动一个程序，是会伤到用户的假失败。
        std::thread::sleep(Duration::from_millis(600));
        right_click_icon(&win, dock, scale, fp2);
        std::thread::sleep(Duration::from_millis(800));
        let folder_items = crate::panel_window::current_menu_items().unwrap_or_default();
        let dmrect = window_rect(menu);
        let dissolve_idx = folder_items
            .iter()
            .filter(|i| !i.divider)
            .position(|i| i.id == crate::menu::id::DISSOLVE);
        if let Some((px, py)) = dissolve_idx.and_then(|i| menu_item_point(&menu_win, &dmrect, scale, i)) {
            let before = list();
            let n_children = before
                .iter()
                .find(|p| p.id == folder_id)
                .map(|f| f.children.len())
                .unwrap_or(0);
            click_at(px, py);
            std::thread::sleep(Duration::from_millis(900));
            let after = list();
            // 只断言**测试自己造的那个**文件夹没了。不能写 `!any(is_folder)` ——
            // 用户的列表里本来就可能已经有文件夹（本轮实测：4 个），那样会永远报红。
            check(
                !after.iter().any(|p| p.id == folder_id)
                    && after.len() == before.len() - 1 + n_children,
                "「解散文件夹」：里面的条目回到顶层，文件夹本身消失",
                format!("{} 项 → {} 项（原文件夹里有 {n_children} 项）", before.len(), after.len()),
            );
        }
    }

    // ---------------- 临时文件夹（Dock 最左侧那个） ----------------
    //
    // 它是个**真实目录**（`apps::temp_folder_path`），点一下要在资源管理器里打开 ——
    // 用户往里丢临时文件。这里验三件事：落在最左侧、目录真的建出来了、
    // 右键菜单第一项是「打开」（目录不是程序，不该说「启动」）。
    {
        crate::store::add_temp_folder(app).ok(); // 已经在了就是"挪到最左"，幂等
        crate::drop::refresh_apps(app);
        std::thread::sleep(Duration::from_millis(1200));

        let cur = list();
        let first = cur.first();
        let want_path = crate::apps::temp_folder_path().unwrap_or_default();
        let is_temp = first
            .map(|p| p.target.eq_ignore_ascii_case(&want_path.display().to_string()))
            .unwrap_or(false);
        check(
            is_temp && want_path.is_dir(),
            "「临时文件夹」在 Dock **最左侧**，而且目录真的建出来了",
            format!(
                "最左侧 = {:?}（{}）；目录 {} 存在={}",
                first.map(|p| p.display_name.clone()),
                first.map(|p| p.target.clone()).unwrap_or_default(),
                want_path.display(),
                want_path.is_dir()
            ),
        );

        // 右键它 → 第一项必须是「打开」
        if let Some(ti) = cur.iter().position(|p| p.target == want_path.display().to_string()) {
            right_click_icon(&win, dock, scale, ti);
            std::thread::sleep(Duration::from_millis(800));
            let items = crate::panel_window::current_menu_items().unwrap_or_default();
            let first_label = items.first().map(|i| i.label.clone()).unwrap_or_default();
            let runas_enabled = items
                .iter()
                .find(|i| i.id == crate::menu::id::RUNAS)
                .map(|i| i.enabled)
                .unwrap_or(true);
            check(
                first_label == "打开" && !runas_enabled,
                "右键「临时文件夹」：第一项是「打开」，且不提权（它是个目录）",
                format!("首项={first_label:?}，runas 可点={runas_enabled}"),
            );
            crate::panel_window::hide_all();
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    // ---------------- 回收站菜单：清空回收站 ----------------
    //
    // 放在**还原之后**：这时列表已经回到原样，按 target 找回收站的位置是确定的
    // （放在前面的话它可能正被关在测试自己建的文件夹里，那条用例会被跳过 —— 实测踩到）。
    //
    // ⚠️ 这里**绝不点它**：那是永久删除用户回收站里的东西。
    {
        let cur = list();
        let bin_pos = cur
            .iter()
            .position(|p| crate::apps::is_recycle_bin(&p.target));
        match bin_pos {
            Some(bi) => {
                right_click_icon(&win, dock, scale, bi);
                std::thread::sleep(Duration::from_millis(800));
                let items = crate::panel_window::current_menu_items().unwrap_or_default();
                let entry = items.iter().find(|i| i.id == crate::menu::id::EMPTY_BIN);
                let n = crate::apps::recycle_bin_items();
                check(
                    entry.is_some() && entry.map(|e| e.enabled) == Some(n > 0),
                    "回收站的右键菜单有「清空回收站」（空的时候置灰）",
                    format!(
                        "回收站 {n} 项；菜单项={:?}（**刻意不点它**：会永久删掉里面的东西）",
                        entry.map(|e| (e.label.clone(), e.enabled))
                    ),
                );
                crate::panel_window::hide_all();
                std::thread::sleep(Duration::from_millis(300));
            }
            None => log_info!("  （Dock 上没有回收站，跳过清空回收站用例）"),
        }
    }

    // ---------------- 还原 ----------------
    restore_separators(app, &original);
    let final_ids: Vec<String> = list().iter().map(|p| p.id.clone()).collect();
    let want_ids: Vec<String> = original.iter().map(|p| p.id.clone()).collect();
    check(
        final_ids == want_ids,
        "测试结束后列表已还原",
        format!("{} 项，一致 = {}", final_ids.len(), final_ids == want_ids),
    );

    log_info!("  小结：{pass} 项通过，{fail} 项失败");
    log_info!("==============================================\n");
}

/// 清掉测试期间加的分割线 / 文件夹，并把顺序还原成 `original`。
///
/// 文件夹要**先解散**（而不是删掉）：里面的条目本来是顶层的东西，
/// 删掉文件夹会把它们一起弄丢。
///
/// ⚠️ **只动测试自己造的**：`original` 里就有的分割线 / 文件夹是**用户的东西**，
/// 一个都不能碰。早期版本无条件"把所有文件夹都解散"，遇到用户的配置里本来就有文件夹时
/// 会把它们拆了 —— 里面的条目被甩到顶层、文件夹本身再被重新建出来，
/// 于是"测试结束后列表已还原"必然报红（本轮实测：48 项 vs 原来的 27 项）。
fn restore_separators(app: &tauri::AppHandle, original: &[crate::model::PinnedApp]) {
    use tauri::Manager;
    let list = || -> Vec<crate::model::PinnedApp> {
        app.state::<crate::store::PrefsState>()
            .0
            .lock()
            .unwrap()
            .pinned
            .clone()
    };
    // 用户原本就有的条目 —— 测试不许碰它们（见函数说明）
    let preexisting: Vec<String> = original.iter().map(|p| p.id.clone()).collect();
    for p in list() {
        if preexisting.contains(&p.id) {
            continue;
        }
        if p.separator {
            let _ = crate::store::remove(app, &p.id);
        }
        if p.is_folder {
            let _ = crate::store::dissolve_folder(app, &p.id);
        }
    }
    // 先补齐缺的（reorder 要求 id 集合完全一致），再整体重排
    for p in original {
        if !list().iter().any(|x| x.id == p.id) {
            let _ = crate::store::pin(app, p.clone());
        }
    }
    let ids: Vec<String> = original.iter().map(|p| p.id.clone()).collect();
    let _ = crate::store::reorder(app, &ids);
    crate::drop::refresh_apps(app);
}

// ---------------------------------------------------------------- 保留：激活决策点量测

pub fn probe_mouseactivate(hwnd: HWND) -> isize {
    unsafe {
        let lparam = ((WM_LBUTTONDOWN as isize) << 16) | (HTCLIENT as isize);
        SendMessageW(
            hwnd,
            WM_MOUSEACTIVATE,
            Some(WPARAM(hwnd.0 as usize)),
            Some(LPARAM(lparam)),
        )
        .0
    }
}

pub fn ma_name(v: isize) -> &'static str {
    match v {
        1 => "MA_ACTIVATE（会夺焦点）",
        2 => "MA_ACTIVATEANDEAT（会夺焦点）",
        3 => "MA_NOACTIVATE（不夺焦点）✅",
        4 => "MA_NOACTIVATEANDEAT（不夺焦点）✅",
        _ => "未知",
    }
}

pub fn sendinput_works() -> (u32, POINT, POINT) {
    unsafe {
        let mut before = POINT::default();
        let _ = GetCursorPos(&mut before);
        let (tx, ty) = (300, 300);
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2);
        let ax = (((tx - vx) as i64 * 65535) / (vw as i64 - 1)) as i32;
        let ay = (((ty - vy) as i64 * 65535) / (vh as i64 - 1)) as i32;
        let inp = [INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: ax,
                    dy: ay,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }];
        let n = SendInput(&inp, std::mem::size_of::<INPUT>() as i32);
        std::thread::sleep(Duration::from_millis(120));
        let mut after = POINT::default();
        let _ = GetCursorPos(&mut after);
        let _: BOOL = BOOL::default();
        (n, before, after)
    }
}

// ---------------------------------------------------------------- 入口

/// 「排在 Dock 后面的可见窗口」扫描用的静态槽（`EnumWindows` 回调不能带闭包）
static SCAN_DOCK: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static SCAN_IDX: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static SCAN_DOCK_IDX: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(-1);
static SCAN_BELOW: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// `EnumWindows` 从最上到最下遍历：记下 Dock 的下标，并收集排在它后面、
/// **可见 / 没最小化 / 矩形与 Dock 相交**的非桌面窗口（那些就是 Dock 盖住了的东西）。
unsafe extern "system" fn scan_below_cb(h: HWND, _l: LPARAM) -> BOOL {
    let i = SCAN_IDX.fetch_add(1, Ordering::SeqCst);
    if h.0 as isize == SCAN_DOCK.load(Ordering::SeqCst) {
        SCAN_DOCK_IDX.store(i, Ordering::SeqCst);
        return TRUE;
    }
    if SCAN_DOCK_IDX.load(Ordering::SeqCst) < 0 {
        return TRUE; // Dock 还没出现在枚举里
    }
    if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
        return TRUE;
    }
    let r = window_rect(h);
    if r.right - r.left <= 0 || r.bottom - r.top <= 0 {
        return TRUE;
    }
    // 只关心"会和 Dock 抢同一块地方"的窗口
    let d = window_rect(HWND(SCAN_DOCK.load(Ordering::SeqCst) as *mut core::ffi::c_void));
    if r.right < d.left || r.left > d.right || r.bottom < d.top || r.top > d.bottom {
        return TRUE;
    }
    let mut cls = [0u16; 128];
    let n = GetClassNameW(h, &mut cls);
    let class = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
    if class == "Progman" || class == "WorkerW" {
        return TRUE; // 桌面本来就该在 Dock 下面
    }
    let mut title = [0u16; 200];
    let tn = GetWindowTextW(h, &mut title);
    let t = String::from_utf16_lossy(&title[..tn.max(0) as usize]);
    SCAN_BELOW
        .lock()
        .unwrap()
        .push(format!("idx={i} [{class}] '{t}'"));
    TRUE
}

/// 层级自检（**无条件跑**，不需要点鼠标）：外部把 Dock 设成置顶，必须**从未生效**。
///
/// # 为什么钉这一条
///
/// 2026-09 用户实测："我打开原神的时候，Dock 出现在游戏页面上面"。根因不是 Dock 自己
/// 置顶，而是 `WS_EX_TOPMOST` 是**窗口属性**：任何程序（自动化脚本 / 录屏 / 游戏加速器 /
/// "总在最前"小工具）都能给它设上，而且**粘** —— 设上就不掉。
///
/// 防线在 `win_layer::dock_subclass_proc` 的两条消息里：`WM_WINDOWPOSCHANGING` 补
/// `SWP_NOZORDER`、`WM_STYLECHANGING` 抹掉 `WS_EX_TOPMOST`（都在**改动生效之前**）。
/// 这条自检把那个行为钉住 —— 关键是断言"**当场**就没生效"，而不是"过一会儿被纠正"：
/// 后者（轮询）有最长一个周期的暴露窗口，游戏里那一眼就够用户看见 Dock 了。
pub fn drive_layering_test(dock: HWND) {
    log_info!("\n============= 层级自检：Dock 固定在桌面那一层 =============");
    let mut pass = 0;
    let mut fail = 0;
    let mut check = |ok: bool, what: &str, detail: String| {
        if ok {
            pass += 1;
            log_info!("  [PASS] {what}  {detail}");
        } else {
            fail += 1;
            log_info!("  [FAIL] {what}  {detail}");
        }
    };

    fn ex_style(h: HWND) -> u32 {
        unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) as u32 }
    }
    fn is_topmost(h: HWND) -> bool {
        ex_style(h) & WS_EX_TOPMOST.0 != 0
    }

    check(
        !is_topmost(dock),
        "起始状态：Dock 不置顶（它属于桌面那一层）",
        format!("ex={:#X}", ex_style(dock)),
    );

    // ---- ① 外部用 SetWindowPos 置顶（最常见的路子）----
    let z0 = crate::win_layer::ZORDER_BLOCKS.load(Ordering::SeqCst);
    unsafe {
        let _ = SetWindowPos(
            dock,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
    check(
        !is_topmost(dock),
        "外部 SetWindowPos(HWND_TOPMOST)：**当场**就没生效（是拦截，不是事后纠正）",
        format!("ex={:#X}", ex_style(dock)),
    );
    std::thread::sleep(Duration::from_millis(120));
    check(
        !is_topmost(dock),
        "…再等 120ms 依然没有（轮询版在这里就已经晚了）",
        format!("ex={:#X}", ex_style(dock)),
    );

    // ---- ② 绕过 SetWindowPos、直接改扩展样式 ----
    let s0 = crate::win_layer::TOPMOST_STYLE_BLOCKS.load(Ordering::SeqCst);
    unsafe {
        let ex = GetWindowLongPtrW(dock, GWL_EXSTYLE);
        let _ = SetWindowLongPtrW(dock, GWL_EXSTYLE, ex | WS_EX_TOPMOST.0 as isize);
    }
    check(
        !is_topmost(dock),
        "外部直接改扩展样式加 WS_EX_TOPMOST：一样加不上（WM_STYLECHANGING 里抹掉）",
        format!("ex={:#X}", ex_style(dock)),
    );

    // ---- ④ "最底层"：除了桌面，没有**压在 Dock 头上**的窗口排在它后面 ----
    //
    // 判据用 `EnumWindows` 的顺序（从最上到最下）。排在 Dock **后面**（更靠下）的窗口，
    // 意味着 Dock 盖在它上面 —— 除了桌面（Progman / WorkerW）以外，一个都不该有。
    // 只看"可见、没最小化、矩形与 Dock 相交"的窗口：不跟 Dock 打架的（最小化的、
    // 屏幕外的、桌面的）不算。
    SCAN_DOCK.store(dock.0 as isize, Ordering::SeqCst);
    SCAN_IDX.store(0, Ordering::SeqCst);
    SCAN_DOCK_IDX.store(-1, Ordering::SeqCst);
    SCAN_BELOW.lock().unwrap().clear();
    unsafe {
        let _ = EnumWindows(Some(scan_below_cb), LPARAM(0));
    }
    let below = SCAN_BELOW.lock().unwrap().clone();
    let dock_idx = SCAN_DOCK_IDX.load(Ordering::SeqCst);
    check(
        below.is_empty(),
        "最底层：没有任何可见窗口排在 Dock **后面**（桌面除外）",
        format!(
            "Dock 下标 {dock_idx}；排在它后面的可疑窗口 {} 个{}",
            below.len(),
            if below.is_empty() {
                String::new()
            } else {
                format!("：{}", below.join(" | "))
            }
        ),
    );

    // ---- ⑤ 桌面不能压在 Dock 上面（explorer 重启后的自愈）----
    //
    // 先跑一次恢复（幂等：只把"错在 Dock 上面"的**可见桌面窗口**沉到最底，
    // 永远不动 Dock 自己的 Z 序），再断言"一个都不剩"。
    let sank = crate::win_layer::recover_if_desktop_above(dock);
    let left = crate::win_layer::visible_desktops_above(dock);
    check(
        left.is_empty(),
        "桌面窗口没有压在 Dock 上面（explorer 重启后会自动把桌面沉回去）",
        format!(
            "本次恢复沉了 {} 个；仍压在 Dock 上面的可见桌面窗口 {} 个",
            if sank { 1 } else { 0 },
            left.len()
        ),
    );

    // ---- ⑥ 钩子确实被走到了（而不是"碰巧没生效"）----
    let z1 = crate::win_layer::ZORDER_BLOCKS.load(Ordering::SeqCst);
    let s1 = crate::win_layer::TOPMOST_STYLE_BLOCKS.load(Ordering::SeqCst);
    check(
        z1 > z0,
        "Z 序请求确实进了子类窗口过程（计数涨了，不是碰巧）",
        format!("{z0} → {z1}"),
    );
    check(
        s1 > s0,
        "样式请求确实进了子类窗口过程（计数涨了，不是碰巧）",
        format!("{s0} → {s1}"),
    );

    log_info!("  小结：{pass} 项通过，{fail} 项失败");
    log_info!("========================================================\n");
}

/// 自检总入口，由 `supervise()` 在窗口层初始化之后调用一次。
///
/// 环境变量开关：
///
/// | 变量 | 默认 | 作用 |
/// |------|------|------|
/// | `DOCK_SELFTEST` | 开 | 设 `0` 跳过焦点矩阵，Dock 保持可用 |
/// | `DOCK_LAYERTEST` | 开 | 设 `0` 跳过层级自检（**不用点鼠标、不用辅助窗口**，所以默认跑） |
/// | `DOCK_OPSTEST`  | 关 | 设 `1` 额外跑应用操作 + IPC 链自检（会 spawn 子进程） |
/// | `DOCK_MENUTEST` | 关 | 设 `1` 额外跑右键菜单（独立窗口）+ 分割线自检 |
/// | `DOCK_LOCTEST`  | 关 | 设 `1` 跑系统位置自检（**会真的开两个资源管理器窗口，跑完自动关**） |
/// | `DOCK_DUMP_APPS`| 关 | 设 `1` 打印枚举结果与图标 alpha 统计 |
pub fn run_if_enabled(app: &tauri::AppHandle, hwnd: HWND) {
    // 层级自检**不依赖合成点击、也不依赖辅助窗口**，所以放在这里无条件跑：
    // 它管的是"Dock 永远不会升到别人上面"，而这一条正是最容易被外部/我们自己的
    // 无心改动破坏的行为（用户 2026-09 明确要求过）。
    if std::env::var("DOCK_LAYERTEST").as_deref() != Ok("0") {
        drive_layering_test(hwnd);
    }
    // 辅助窗口必须由**主线程**创建（只有主线程在跑消息循环），
    // 测试本身在后台线程驱动（要 sleep，不能占用主线程）
    if std::env::var("DOCK_SELFTEST").as_deref() != Ok("0") {
        let (tx, rx) = std::sync::mpsc::channel::<(usize, usize, usize)>();
        let app_sw = app.clone();
        let _ = app.run_on_main_thread(move || {
            let _ = &app_sw;
            let r = create_windows();
            let _ = tx.send(match r {
                Some(w) => (
                    w.settings.0 as usize,
                    w.fake.0 as usize,
                    w.fake_open.0 as usize,
                ),
                None => (0, 0, 0),
            });
        });

        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok((s, f, fo)) if s != 0 && f != 0 && fo != 0 => {
                let tw = TestWindows {
                    settings: HWND(s as *mut std::ffi::c_void),
                    fake: HWND(f as *mut std::ffi::c_void),
                    fake_open: HWND(fo as *mut std::ffi::c_void),
                };
                std::thread::sleep(Duration::from_millis(700));
                drive(hwnd, &tw);

                // BL-4 前置实验：拖出窗口后 pointer 事件还在不在
                if std::env::var("DOCK_DRAGPROBE").as_deref() == Ok("1") {
                    drive_drag_probe(app);
                }
                // Dock 内拖拽：排序 / 拖出删除 / 非法顺序拒绝
                if std::env::var("DOCK_DRAGTEST").as_deref() == Ok("1") {
                    drive_drag_test(app);
                }
                // 右键菜单（独立窗口）+ 分割线
                if std::env::var("DOCK_MENUTEST").as_deref() == Ok("1") {
                    drive_menu_test(app, tw.fake);
                }
                // 系统位置（此电脑 / 回收站）：真的打开一次再关掉
                if std::env::var("DOCK_LOCTEST").as_deref() == Ok("1") {
                    drive_location_test(app);
                }

                // 应用操作自检（激活 / 最小化 / 还原）—— 单独开关，会 spawn 子进程
                if std::env::var("DOCK_OPSTEST").as_deref() == Ok("1") {
                    drive_app_ops(app, tw.settings);
                    drive_ipc_test(app, tw.settings);
                    drive_settings_test(app);
                    drive_drop_test(app);
                }

                let _ = app.run_on_main_thread(move || {
                    destroy_windows(&TestWindows {
                        settings: HWND(s as *mut std::ffi::c_void),
                        fake: HWND(f as *mut std::ffi::c_void),
                        fake_open: HWND(fo as *mut std::ffi::c_void),
                    });
                });
            }
            _ => log_error!("[自检] 创建辅助窗口失败或被跳过"),
        }
    } else {
        log_info!("[窗口层] 已跳过自检（DOCK_SELFTEST=0），窗口保持就绪");
    }

    if std::env::var("DOCK_DUMP_APPS").as_deref() == Ok("1") {
        dump_apps();
    }

    // 量「图标间距与图形留白」——排查"某几个图标之间显得空"
    if std::env::var("DOCK_ICONBOX").as_deref() == Ok("1") {
        // 等页面把面板画出来再量
        std::thread::sleep(Duration::from_millis(2500));
        dump_icon_boxes(app);
    }

    // 开发用：把右键菜单打开并保持一段时间，用于**截图核对观感**
    // （与产品构建里的 `DOCK_OPENSETTINGS` 同类 —— 光读代码判断不了好不好看）。
    // 值 = 保持多少秒，默认 20。配 `DOCK_SELFTEST=0` 用，免得焦点矩阵先动鼠标。
    if let Ok(v) = std::env::var("DOCK_MENUSHOT") {
        menu_shot(app, v.parse().unwrap_or(20));
    }
}

/// 打开右键菜单并保持 `secs` 秒（截图用）。见 `run_if_enabled` 里的说明。
fn menu_shot(app: &tauri::AppHandle, secs: u64) {
    use tauri::Manager;

    // 等 Dock 页面把面板尺寸报上来，否则第一屏还没有图标
    std::thread::sleep(Duration::from_millis(3500));
    let first = app
        .state::<crate::store::PrefsState>()
        .0
        .lock()
        .unwrap()
        .pinned
        .first()
        .map(|p| p.id.clone());
    let Some(id) = first else {
        log_error!("[浮层] 截图模式：Dock 是空的");
        return;
    };
    let target = match crate::commands::menu_target(app, &id) {
        Ok(t) => t,
        Err(e) => {
            log_error!("[浮层] 截图模式取目标失败: {e}");
            return;
        }
    };
    let dock_hwnd = app
        .get_webview_window("dock")
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as u64)
        .unwrap_or(0);
    // 第一个图标的中心：行左右各留 12 逻辑像素内边距，图标宽 iconSize
    let icon = app
        .state::<crate::store::PrefsState>()
        .0
        .lock()
        .unwrap()
        .icon_size as f64;
    let anchor = 12.0 + icon / 2.0;

    // 第一个条目**是文件夹**就展开文件夹、否则弹菜单 —— 两种形态都要能截。
    // （返回值只用来判断成没成功，两种形态都是 bool）
    let shown = if target.is_folder {
        crate::commands::show_panel_folder(app.clone(), dock_hwnd, id.clone(), anchor)
    } else {
        crate::panel_window::show_menu(app, dock_hwnd, target, anchor, false)
    };
    match shown {
        Ok(_) => log_info!("[浮层] 截图模式：已打开，保持 {secs} 秒"),
        Err(e) => log_error!("[浮层] 截图模式打开失败: {e}"),
    }
    std::thread::sleep(Duration::from_secs(secs));
    crate::panel_window::hide_all();
}

/// 打印应用模型层的枚举结果，用于离线核对 AUMID / 图标抽取。
///
/// 这里**不**做「ASCII 画轮廓」那类调试输出 —— 判断图标底板要用像素统计数据
/// （全透明/全不透明的比例），肉眼数点阵既慢又容易看错。
pub fn dump_apps() {
    let apps = crate::apps::enumerate_apps();
    log_info!("\n==================== 应用枚举结果 ====================");
    log_info!("共 {} 个应用", apps.len());
    for a in &apps {
        log_info!(
            "  {:<26} [{:?}] 窗口={} 前台={:<5} 提权={:<5}\n      id     = {}\n      target = {}",
            a.display_name,
            a.kind,
            a.windows.len(),
            a.has_foreground,
            a.is_elevated,
            a.id,
            a.target
        );
        for w in &a.windows {
            let t: String = w.title.chars().take(60).collect();
            log_debug!("      hwnd=0x{:X} 最小化={:<5} '{}'", w.hwnd, w.minimized, t);
        }
        // 直接试取图标 —— 用来验证 UWP 的 shell:AppsFolder 路径是否真的能取到图标
        match crate::icons::extract_icon(&a.target, 128) {
            Some(d) => {
                let s = crate::icons::alpha_stats(&d);
                log_info!(
                    "      图标: 成功 {}x{}  alpha 范围 {}..{}  全透明 {:.1}%  全不透明 {:.1}%",
                    d.width,
                    d.height,
                    s.min,
                    s.max,
                    s.transparent_ratio * 100.0,
                    s.opaque_ratio * 100.0
                );
            }
            None => log_info!("      图标: **失败**（前端会退回首字母兜底）"),
        }
    }
    log_info!("======================================================\n");
}
