import { useCallback, useEffect, useRef, useState } from "react";
import {
  dockHwnd,
  getIconImage,
  getPanelState,
  hideIconLabel,
  panelInvoke,
  panelLaunch,
  panelReady,
  showIconLabel,
  showPanelMenu,
  type PanelPayload,
} from "../lib/ipc";

declare global {
  interface Window {
    /**
     * Rust 侧（`panel_window::present`）用 `webview.eval` 调用它，参数是本次显示的序号。
     * 这是 **Rust → 页面** 的单向调用，不经过前端 invoke，因此不需要 capability。
     */
    __dockPanel?: (seq: number) => void;
  }
}

/** 文件夹网格里一格要显示的图标（异步取好后缓存在组件里） */
function useFolderIcons(payload: PanelPayload | null) {
  const [icons, setIcons] = useState<Record<string, string | null>>({});
  useEffect(() => {
    if (!payload || payload.content.kind !== "folder") return;
    let alive = true;
    const items = payload.content.items;
    void (async () => {
      const entries = await Promise.all(
        items.map(async (it) => {
          // 和 Dock 用同一条取图标的路（含光学归一化），所以同一张图只会编码一次
          const img = await getIconImage(it.target, 96);
          return [it.id, img.url] as const;
        }),
      );
      if (alive) setIcons(Object.fromEntries(entries));
    })();
    return () => {
      alive = false;
    };
  }, [payload]);
  return icons;
}

/**
 * Dock 的**浮层**页面（右键菜单 + 文件夹面板）。
 *
 * # 这个页面是**纯渲染器**
 *
 * 有哪些菜单项、文件夹里有什么、点了干什么，全部由 Rust 决定
 * （见 `src-tauri/src/menu.rs` 与 `panel_window.rs`）。
 * 页面只做三件事：把 Rust 给的内容画出来、把点击回传、在渲染完成后
 * 告诉 Rust「可以显示了」。
 *
 * 为什么不在前端决定：菜单里几乎每一项都要调 Win32（激活窗口、提权启动、
 * 在资源管理器里定位文件），前端判断不了「这个条目现在还在不在运行」。
 * 放在 Rust 一处判断，前端就永远不会和真实状态不一致。
 *
 * # 为什么菜单和文件夹是同一个页面
 *
 * 两者共用同一个浮层窗口（同一时刻只该有一个浮层），页面按 `content.kind` 分支渲染。
 * 尺寸（行高、格子边长、列数、内边距）全部由 Rust 通过 payload 传进来 ——
 * 窗口大小是 Rust 按同一组数字算的，CSS 里再写一份就会出现"最后一行被裁掉"。
 */
export default function Panel() {
  const [state, setState] = useState<PanelPayload | null>(null);
  /** 待确认的序号：页面渲染完要把它回传给 Rust（见 panel_window.rs 的显示时序） */
  const seqRef = useRef<number | null>(null);
  const icons = useFolderIcons(state);

  const reload = useCallback(async () => {
    try {
      setState(await getPanelState());
    } catch (e) {
      console.warn("[浮层] 读取内容失败", e);
      setState(null);
    }
  }, []);

  // Rust 通知：有新内容，读一次
  useEffect(() => {
    window.__dockPanel = (seq: number) => {
      seqRef.current = seq;
      void reload();
    };
    void reload(); // 首次挂载也读一次（窗口是启动时创建、隐藏着的）
    return () => {
      delete window.__dockPanel;
    };
  }, [reload]);

  // 渲染完成后才让 Rust 显示窗口 —— 反过来做会先看到上一次的内容（一次闪烁）。
  //
  // 用 setTimeout(0) 而不是 requestAnimationFrame：**隐藏窗口里 rAF 不会被调用**
  // （Chromium 会挂起不可见窗口的动画帧），那样这个回调永远等不到，
  // 每次都得靠 Rust 侧 250ms 的兜底超时，浮层会明显变慢。
  useEffect(() => {
    const seq = seqRef.current;
    if (seq == null || !state || state.seq !== seq) return;
    seqRef.current = null;
    const t = window.setTimeout(() => {
      panelReady(seq).catch((e) => console.warn("[浮层] 上报就绪失败", e));
    }, 0);
    return () => window.clearTimeout(t);
  }, [state]);

  const pick = useCallback((itemId: string) => {
    // Rust 侧会先关浮层再执行动作（见 commands::panel_invoke），
    // 失败了会在 Dock 上弹 toast —— 这里不需要（也没法）显示错误
    panelInvoke(itemId).catch((e) => console.warn("[浮层] 动作失败", e));
  }, []);

  const launch = useCallback((itemId: string) => {
    panelLaunch(itemId).catch((e) => console.warn("[浮层] 启动失败", e));
  }, []);

  /**
   * 在**面板里**右键一个图标 → 弹出**它自己的菜单**（和 Dock 上右键同一个东西）。
   *
   * `anchorX` 传 `null`：光标在浮层窗口上，页面拿不到"Dock 客户端坐标"这个量，
   * 由 Rust 现取光标位置（见 `commands::show_panel_menu`）。
   *
   * ⚠️ 面板页面**必须自己处理右键**：不处理的话 WebView2 会弹它自己的
   * 「刷新 / 检查」菜单（用户报的"右键弹出的也是错的窗口"就是这个）。
   */
  const contextItem = useCallback(async (itemId: string) => {
    try {
      const owner = await dockHwnd();
      if (!owner) return;
      await showPanelMenu({ ownerHwnd: owner, appId: itemId, anchorX: null });
    } catch (e) {
      console.warn("[浮层] 打开菜单失败", e);
    }
  }, []);

  /**
   * 悬停文件夹里的一格 → 在**面板上方**显示名字。
   *
   * 和 Dock 上同一套机制（复用浮层窗口的 `Label` 形态），只是定位基准换成文件夹面板自己：
   * 格子里的图标离 Dock 很远，气泡跑到屏幕底部就没意义了。
   * 锚点用格子矩形的**中心**，不用 `clientX`（光标能从边缘进入）。
   */
  const hoverItem = useCallback((itemId: string, label: string, el: HTMLElement) => {
    const r = el.getBoundingClientRect();
    void (async () => {
      try {
        const owner = await dockHwnd();
        if (!owner) return;
        await showIconLabel({
          ownerHwnd: owner,
          targetId: itemId,
          text: label,
          anchorX: r.left + r.width / 2,
          inFolder: true,
        });
      } catch (e) {
        console.warn("[浮层] 显示名字失败", e);
      }
    })();
  }, []);

  // 面板里任何一次右键都要拦住 WebView2 的默认菜单。图标上的由 `contextItem` 处理，
  // 空白处的只拦截（不关面板 —— 右键空白处关掉面板不符合任何平台的习惯）。
  useEffect(() => {
    const stop = (e: MouseEvent) => e.preventDefault();
    document.addEventListener("contextmenu", stop);
    return () => document.removeEventListener("contextmenu", stop);
  }, []);

  if (!state) {
    // 没有内容 = 浮层已经关了。画成完全透明，等待隐藏。
    return null;
  }

  const shell = {
    width: state.width,
    // 遮罩颜色跟 Dock 的着色一致（OS 的亚克力 + 这一层一起决定观感）
    "--panel-rgb": state.glassRgb.join(", "),
  } as React.CSSProperties;
  const theme = state.dark ? "panel-dark" : "panel-light";

  // ---- 悬停标签（图标上方的小气泡）----
  if (state.content.kind === "label") {
    const l = state.content;
    return (
      <div className={"panel panel-label " + theme} style={shell}>
        <span style={{ fontSize: l.fontSize }}>{l.text}</span>
      </div>
    );
  }

  if (state.content.kind === "folder") {
    const f = state.content;
    return (
      <div className={"panel panel-folder " + theme} style={shell}>
        <div
          className="folder-grid"
          style={{
            padding: f.gap === 0 ? 0 : undefined,
            gridTemplateColumns: `repeat(${f.cols}, ${f.cell}px)`,
            gap: f.gap,
            placeContent: "center",
          }}
        >
          {f.items.map((it) => (
            <button
              key={it.id}
              type="button"
              className="folder-item"
              style={{ width: f.cell, height: f.cell }}
              // 名字用**自己画的气泡**（图标上方），不是原生 `title` tooltip ——
              // 后者是 WebView2 画的方角系统提示，和 Dock 的观感不是一套。
              // `aria-label` 保留无障碍语义（`title` 一并删掉了）。
              aria-label={it.label}
              onMouseEnter={(e) => hoverItem(it.id, it.label, e.currentTarget)}
              onMouseLeave={() => {
                hideIconLabel().catch(() => {
                  /* 标签本来就没开：忽略 */
                });
              }}
              onClick={() => launch(it.id)}
              onContextMenu={(e) => {
                e.preventDefault();
                contextItem(it.id);
              }}
            >
              <span className="folder-item-icon">
                {icons[it.id] ? (
                  <img src={icons[it.id]!} alt={it.label} draggable={false} />
                ) : (
                  <span className="folder-item-fallback">{it.label.slice(0, 1)}</span>
                )}
              </span>
              {it.running && <span className="folder-dot" />}
            </button>
          ))}
        </div>
      </div>
    );
  }

  // ---- 菜单形态 ----
  const m = state.content;
  return (
    <div
      className={"panel panel-menu " + theme}
      style={{ ...shell, padding: `${state.pad}px 0` }}
    >
      {m.items.map((it, i) =>
        it.divider ? (
          <div key={`divider-${i}`} className="menu-divider" style={{ height: m.dividerH }}>
            <span />
          </div>
        ) : (
          <button
            key={it.id}
            type="button"
            className={"menu-item" + (it.danger ? " menu-danger" : "")}
            style={{ height: m.rowH }}
            disabled={!it.enabled}
            onClick={() => pick(it.id)}
          >
            {it.label}
          </button>
        ),
      )}
    </div>
  );
}
