import { invoke } from "@tauri-apps/api/core";

/**
 * 把前端的错误送进 Dock 的日志文件。
 *
 * # 为什么必须有它
 *
 * WebView 是**独立进程**：页面里的 `console.error`、未捕获异常、React 渲染报错
 * 都只活在那边 —— Dock 自己的日志（Rust 侧）一个字都看不到。
 * 而"点了没反应 / 图标全没了 / 页面一片空白"这类问题**恰恰全在页面里**，
 * 没有这条通道就只能靠猜。
 *
 * 落到日志里长这样（`page` 会标出是哪个窗口）：
 *
 * ```text
 * 2026-09-25 10:20:31.412 ERROR [dock_app::commands] [前端 dock] TypeError: ...
 * ```
 *
 * # 三条约定
 *
 * 1. **绝不因为上报失败而抛**：日志通道自己出错不能把页面带崩（`catch` 掉）；
 * 2. **不递归**：这里只调 `invoke`，不碰 `console` —— 否则 `console.error` 的钩子
 *    会把自己再叫一遍；
 * 3. `console.error` / `console.warn` 原样保留（转发一份，不吞掉）。
 */
export function installFrontendLogging(page: string): void {
  const send = (level: string, msg: string) => {
    try {
      void invoke("js_log", { level, page, msg }).catch(() => {});
    } catch {
      // invoke 本身不可用时静默 —— 页面的正确性不该依赖日志通道
    }
  };

  window.addEventListener("error", (e) => {
    const where = e.filename ? `${e.filename}:${e.lineno}:${e.colno}` : "位置未知";
    const stack = e.error instanceof Error && e.error.stack ? `\n${e.error.stack}` : "";
    send("error", `未捕获异常：${e.message} @ ${where}${stack}`);
  });

  window.addEventListener("unhandledrejection", (e) => {
    const r = (e as PromiseRejectionEvent).reason;
    const body = r instanceof Error ? `${r.message}\n${r.stack ?? ""}` : String(r);
    send("error", `未处理的 Promise 拒绝：${body}`);
  });

  // React 的渲染错误、我们自己 `console.warn` 的提示都走 console —— 一并转发
  const rawError = console.error.bind(console);
  const rawWarn = console.warn.bind(console);
  console.error = (...args: unknown[]) => {
    send("error", `console.error: ${args.map(stringify).join(" ")}`);
    rawError(...args);
  };
  console.warn = (...args: unknown[]) => {
    send("warn", `console.warn: ${args.map(stringify).join(" ")}`);
    rawWarn(...args);
  };

  // 页面就绪也记一行：这样"某个窗口压根没起来"一眼能看出来
  send("info", `页面已加载（${location.pathname || "/"}）`);
}

function stringify(v: unknown): string {
  if (typeof v === "string") return v;
  if (v instanceof Error) return `${v.name}: ${v.message}`;
  try {
    return JSON.stringify(v) ?? String(v);
  } catch {
    return String(v);
  }
}
