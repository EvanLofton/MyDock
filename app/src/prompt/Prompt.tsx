import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * Rust 侧 `prompt_window::present` 用 `webview.eval` 调用它（本次要问什么）。
 * 和浮层页面是同一套做法：**Rust → 页面**的单向调用，不需要 capability。
 */
declare global {
  interface Window {
    __dockPrompt?: (seq: number) => void;
  }
}

interface PromptState {
  seq: number;
  /** 窗口标题（「新建文件夹」/「重命名文件夹」） */
  title: string;
  /** 输入框上面的说明文字 */
  label: string;
  /** 输入框里预填的内容（改名时是它现在的名字） */
  value: string;
  /** 最长多少个字符（和 Rust 侧 `store::MAX_FOLDER_NAME_CHARS` 同一个数） */
  maxLength: number;
}

/**
 * 命名输入框（「新建文件夹…」/「重命名…」）。
 *
 * # 为什么是自绘而不是系统输入框
 *
 * 第一版用的是手搓的 Win32 对话框 —— 用户第一眼就说"太丑"，而且**内容还显示不全**
 * （我把 `CreateWindowExW` 的宽高当成客户区尺寸了，实际含标题栏与边框，
 * 于是底部的按钮被裁掉）。系统控件在这个应用里本来也是异类：
 * Dock、浮层、设置页全是同一套玻璃观感，中间弹一个灰头土脸的窗口很突兀。
 *
 * 所以这个页面自己画，材质和浮层完全一致（OS 亚克力在 Rust 侧铺，见 `prompt_window.rs`）。
 *
 * # 交互约定（照 Windows/macOS 的通用习惯）
 *
 * - 打开就把光标放进输入框、**全选**（直接打字即替换，不用先按退格）；
 * - **回车 = 确定**、**Esc = 取消**、点右上角关闭 = 取消；
 * - 「确定」是默认按钮（蓝色主色），「取消」是次要按钮；
 * - 名字为空时「确定」置灰（空名字没有意义）。
 */
export default function Prompt() {
  const [state, setState] = useState<PromptState | null>(null);
  const [text, setText] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  const seqRef = useRef(0);

  const reload = useCallback(async () => {
    try {
      const s = await invoke<PromptState | null>("prompt_state");
      if (s) {
        setState(s);
        setText(s.value);
      }
    } catch (e) {
      console.warn("[命名] 读取内容失败", e);
    }
  }, []);

  useEffect(() => {
    window.__dockPrompt = (seq: number) => {
      seqRef.current = seq;
      void reload();
    };
    void reload();
    return () => {
      delete window.__dockPrompt;
    };
  }, [reload]);

  // 渲染完成后才让 Rust 显示窗口 —— 反过来做会先看到上一次的内容（一次闪烁）。
  // 和浮层页面同一条规矩（见 `panel_window.rs` 的显示时序）。
  //
  // 用 setTimeout(0) 而不是 requestAnimationFrame：**隐藏窗口里 rAF 不会被调用**
  // （Chromium 会挂起不可见窗口的动画帧），那样这个回调永远等不到，
  // 每次都得靠 Rust 侧 400ms 的兜底超时，输入框会明显变慢。
  useEffect(() => {
    const seq = seqRef.current;
    if (!state || state.seq !== seq) return;
    seqRef.current = 0;
    const t = window.setTimeout(() => {
      invoke("prompt_ready", { seq: state.seq }).catch((e) =>
        console.warn("[命名] 上报就绪失败", e),
      );
    }, 0);
    return () => window.clearTimeout(t);
  }, [state]);

  // 内容一到位就把光标塞进输入框并全选。
  // ⚠️ 不能只靠 autoFocus：窗口是**先创建、显示前**就渲染好的，
  // 那时的焦点还在别处；这里等 state 到位（= 真的显示了）再抢一次。
  useEffect(() => {
    if (!state) return;
    const t = window.setTimeout(() => {
      const el = inputRef.current;
      if (!el) return;
      el.focus();
      el.select();
    }, 0);
    return () => window.clearTimeout(t);
  }, [state]);

  const submit = useCallback(() => {
    const v = text.trim();
    if (!v) return;
    invoke("prompt_submit", { text: v }).catch((e) => console.warn("[命名] 提交失败", e));
  }, [text]);

  const cancel = useCallback(() => {
    invoke("prompt_cancel").catch((e) => console.warn("[命名] 取消失败", e));
  }, []);

  if (!state) {
    // 还没拿到内容：画成完全透明（窗口本来就还没显示）
    return null;
  }

  const ok = text.trim().length > 0;

  return (
    <div
      className="prompt"
      // Esc 取消：挂在最外层，输入框里也收得到
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          cancel();
        } else if (e.key === "Enter") {
          e.preventDefault();
          submit();
        }
      }}
    >
      <div className="prompt-title">{state.title}</div>
      <label className="prompt-label" htmlFor="dock-prompt-input">
        {state.label}
      </label>
      <input
        id="dock-prompt-input"
        ref={inputRef}
        className="prompt-input"
        value={text}
        maxLength={state.maxLength}
        spellCheck={false}
        autoComplete="off"
        onChange={(e) => setText(e.target.value)}
      />
      <div className="prompt-actions">
        {/* 字数提示：让"还能打多少"可见，而不是打到上限才发现打不进去 */}
        <span className="prompt-count">
          {text.length}/{state.maxLength}
        </span>
        <button type="button" className="prompt-btn" onClick={cancel}>
          取消
        </button>
        <button
          type="button"
          className="prompt-btn primary"
          disabled={!ok}
          onClick={submit}
        >
          确定
        </button>
      </div>
    </div>
  );
}
