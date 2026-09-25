import { useCallback, useEffect, useRef, useState } from "react";
import {
  Dock,
  PANEL_H,
  PANEL_RADIUS,
  type DockVisualItem,
} from "./components/Dock";
import {
  activateApp,
  addAppToFolder,
  dockHwnd,
  getIconImage,
  getPreferences,
  hideIconLabel,
  hidePanel,
  launchApp,
  listApps,
  minimizeApp,
  getRecycleBinItems,
  removeDockApp,
  reorderDockApps,
  reportTest,
  setDockSize,
  showIconLabel,
  showPanelFolder,
  showPanelMenu,
  type Preferences,
} from "./lib/ipc";
import "./styles/glass.css";

declare global {
  interface Window {
    /** 自动化自检钩子：Rust 侧通过 webview.eval 调用，验证整条 IPC 链路 */
    __dockIpcTest?: (hwnd: number) => void;
    /**
     * 配置变化钩子：设置窗口改了配置后，Rust 侧用 `webview.eval` 调用它，
     * 让 Dock 立刻重读配置（见 `commands::apply_live`）。
     * 这是 **Rust → 页面** 的单向调用，不经过前端 invoke，因此不需要 capability。
     */
    __dockReloadPrefs?: () => void;
    /**
     * 应用列表立刻刷新：拖放添加应用 / 右键固定之后由 Rust 调用，
     * 不用等下一次轮询（见 `drop::refresh_apps`）。
     */
    __dockRefreshApps?: () => void;
    /** 从 Rust 侧弹一句提示（拖放的反馈：已添加 / 已在 Dock 上 / 为什么不收） */
    __dockToast?: (msg: string) => void;
  }
}

/** 图标按物理像素请求，保证 125% DPI 下也清晰 */
const ICON_PX = 128;

/**
 * 这个 target 是不是回收站？
 *
 * 只有它的图标会随**状态**变（空 / 满两态），所以要按"里面有几项"给图标缓存带版本号
 * （见 `getIconImage` 的 `version` 参数与 `apps::recycle_bin_items`）。
 * 判断用 `shell:` 命名空间路径 —— 和 Rust 侧 `apps::SYSTEM_LOCATIONS` 里那张表一致。
 */
const RECYCLE_BIN_TARGET = "shell:RecycleBinFolder";
const isRecycleBin = (target: string) => target === RECYCLE_BIN_TARGET;

/**
 * 轮询节奏 —— 为什么不是固定间隔。
 *
 * 固定 4 秒间隔时，点图标启动程序后白点最多要 4 秒才出现（实测 2~4 秒），
 * 观感上就是「点了没反应」。但一直用 250ms 高频轮询也没必要：绝大部分时间
 * 什么都没变。所以改成**自适应**：
 *
 *  - 空闲：`POLL_IDLE_MS` —— 只用来兜住「不是我们触发的」变化
 *    （比如你从任务栏关掉一个程序）；
 *  - 刚发生变化 / 刚点过图标：`POLL_FAST_MS` 连打 `POLL_FAST_ROUNDS` 轮。
 *
 * 关键在于「点图标」是**本地已知事件**（见 `onActivate`），没必要靠盲轮询去发现它，
 * 所以点完立刻查一轮并切到快节奏。程序建窗口要一点时间，快节奏保持约 3.5 秒兜住它。
 *
 * 每轮成本很低：Rust 侧的窗口枚举（几毫秒）+ 几百字节 JSON。
 * 图标是**一次取好就缓存**的（见 `lib/ipc.ts` 的 `iconCache`），不会重复抽取。
 *
 * 后续若要彻底去掉轮询，可换成 Rust 侧 `SetWinEventHook` 推送事件；
 * 但那需要新增 Tauri capability（`core:event:allow-listen`），收益只是把
 * 空闲时的 1.5 秒降到 0，暂不做。
 */
const POLL_IDLE_MS = 1500;const POLL_FAST_MS = 250;
const POLL_FAST_ROUNDS = 14; // ≈ 3.5 秒

const FALLBACK_COLORS = [
  "linear-gradient(160deg,#5ac8fa,#0a84ff)",
  "linear-gradient(160deg,#ffd60a,#ff9f0a)",
  "linear-gradient(160deg,#ff453a,#ff375f)",
  "linear-gradient(160deg,#30d158,#248a3d)",
  "linear-gradient(160deg,#bf5af2,#5e5ce6)",
  "linear-gradient(160deg,#64d2ff,#0a84ff)",
];

export default function App() {
  const [items, setItems] = useState<DockVisualItem[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [prefs, setPrefs] = useState<Preferences | null>(null);

  /** 立刻刷新一轮并切到快节奏轮询（点图标 / 右键操作之后调用） */
  const kickPoll = useRef<() => void>(() => {});

  /**
   * 用 ref 把最新的 `prefs` 带进轮询闭包。
   *
   * 轮询 effect 的依赖是 `[]`，它的闭包会永远捕获**首次渲染**的 `prefs`（那时还是
   * `null`）—— 于是 `showRunningIndicators` 不管配置成什么都会被当成 `true`。
   * 用 ref 读最新值：既修掉这个陈旧闭包，又不必因为 prefs 变化而重启轮询。
   */
  const prefsRef = useRef<Preferences | null>(null);
  prefsRef.current = prefs;

  // 自动化自检钩子：暴露一个函数，Rust 侧用 webview.eval 调用它，
  // 走的是**和真实点击完全相同**的 activateApp 路径，最后把结果回写给 Rust。
  useEffect(() => {
    window.__dockIpcTest = async (hwnd: number) => {
      let msg: string;
      try {
        await activateApp(hwnd);
        msg = "activate_app:ok";
      } catch (e) {
        msg = "activate_app:err:" + String(e);
      }
      try {
        await reportTest(msg);
      } catch {
        /* 回写失败就无从判定，交给 Rust 侧超时 */
      }
    };
    return () => {
      delete window.__dockIpcTest;
    };
  }, []);

  // 配置：启动时读一次，之后由 Rust 在配置变化时用 `eval` 通知重读
  // （见 `commands::apply_live`），所以不必轮询。
  const loadPrefs = useCallback(() => {
    getPreferences()
      .then(setPrefs)
      .catch((e) => console.warn("[prefs] 读取失败", e));
  }, []);

  useEffect(() => {
    loadPrefs();
  }, [loadPrefs]);

  // 设置窗口改了配置 → Rust 调这个让 Dock 立刻重读（拖动色板时不滞后）
  useEffect(() => {
    window.__dockReloadPrefs = loadPrefs;
    return () => {
      delete window.__dockReloadPrefs;
    };
  }, [loadPrefs]);

  // 拖放添加应用 / 右键固定之后 → Rust 调这个让图标列表立刻刷新（见 drop.rs）。
  // 复用轮询那套 `kickPoll`，顺便进入快节奏：新程序可能马上就会启动。
  useEffect(() => {
    window.__dockRefreshApps = () => kickPoll.current();
    window.__dockToast = (msg: string) => {
      setToast(msg);
      window.setTimeout(() => setToast(null), 2600);
    };
    return () => {
      delete window.__dockRefreshApps;
      delete window.__dockToast;
    };
  }, []);

  // 把背景色写到 CSS 变量上：`.dock` 的遮罩层读它（见 glass.css）。
  // 与 Rust 侧 `glass_rgb` 是同一个值，这样「OS 亚克力」和「CSS 遮罩」一起变。
  useEffect(() => {
    const rgb = prefs?.glassRgb ?? [24, 24, 28];
    document.documentElement.style.setProperty("--glass-rgb", rgb.join(", "));
  }, [prefs?.glassRgb]);

  useEffect(() => {
    let alive = true;
    let timer: number | undefined;
    /** > 0 表示还在快节奏里 */
    let fastRounds = POLL_FAST_ROUNDS;
    /** 上一轮的可见状态指纹，用来判断「变了没有」 */
    let lastSig = "";

    /** 查一轮；返回**可见状态**有没有变化 */
    const load = async (): Promise<boolean> => {
      try {
        const apps = await listApps();
        // 回收站的图标有「空 / 满」两态（BL-8）。图标按 target 缓存，所以要把
        // **里面有几项**当作版本号喂给缓存键 —— 否则清空回收站之后图标不会变。
        // 只有 Dock 上真有回收站时才问（一次 shell 查询）。
        let binVersion = 0;
        if (apps.some((a) => isRecycleBin(a.target))) {
          try {
            binVersion = await getRecycleBinItems();
          } catch (e) {
            console.warn("[apps] 读回收站项数失败", e);
          }
        }
        const versionOf = (target: string) => (isRecycleBin(target) ? binVersion : 0);
        const built = await Promise.all(
          apps.map(async (a, i) => {
            // 分割线不是应用：没有图标可取（它连 target 都是空的）
            const icon = a.separator
              ? null
              : await getIconImage(a.target, ICON_PX, versionOf(a.target));
            // 文件夹：再取**里面前 4 个**的图标画预览（2×2）。
            // 只取 4 个 —— 预览就那么大，取多了是白花时间。
            const preview = a.isFolder
              ? await Promise.all(
                  a.children.slice(0, 4).map(async (c) => ({
                    id: c.id,
                    iconUrl: (await getIconImage(c.target, ICON_PX, versionOf(c.target))).url,
                  })),
                )
              : undefined;
            const first = a.windows[0];
            return {
              id: a.id,
              label: a.displayName,
              color: FALLBACK_COLORS[i % FALLBACK_COLORS.length],
              iconUrl: icon?.url ?? null,
              // 图形在方格里的留白（逻辑像素）—— 布局用它把视觉间距拉平
              iconInset: (icon?.insetX ?? 0) * (prefsRef.current?.iconSize ?? 38),
              isFolder: a.isFolder,
              preview,
              // 固定但未运行的应用也显示，但没有运行指示点
              running: a.running && (prefsRef.current?.showRunningIndicators ?? true),
              elevated: a.isElevated,
              hwnd: first ? first.hwnd : null,
              hasForeground: a.hasForeground,
              windowCount: a.windows.length,
              target: a.target,
              appRunning: a.running,
              separator: a.separator,
            } satisfies DockVisualItem;
          }),
        );
        if (!alive) return false;

        // 指纹只包含**看得见**的东西：运行状态和前台状态决定白点，窗口数决定会不会
        // 多出条目。图标和名字不进指纹 —— 它们不会因为轮询而变化（图标是缓存的），
        // 进了反而会让指纹每轮都不同，快节奏永远退不出去。
        //
        // 而且**先排序**：指纹对顺序不敏感，才不会因为一次无关紧要的重排就认为
        // 「变了」，把快节奏一直续下去（那会变成 250ms 的死循环）。
        const sig = apps
          .map(
            (a) =>
              `${a.id}|${a.running ? 1 : 0}|${a.hasForeground ? 1 : 0}|${a.windows.length}`,
          )
          .sort()
          .join(",");
        const changed = sig !== lastSig;
        lastSig = sig;

        setItems(built);
        setError(null);
        return changed;
      } catch (e) {
        console.error("[apps] 枚举失败", e);
        if (alive) setError(String(e));
        return false;
      }
    };

    const tick = async () => {
      const changed = await load();
      if (!alive) return;
      // 变了就继续快节奏（程序启动过程中会连着变几次：建窗口 → 拿到前台）；
      // 没变就一轮轮退回空闲节奏，别在没事的时候也一直高频唤醒
      fastRounds = changed ? POLL_FAST_ROUNDS : fastRounds - 1;
      timer = window.setTimeout(tick, fastRounds > 0 ? POLL_FAST_MS : POLL_IDLE_MS);
    };

    // 点图标之后调用：取消当前等待，立刻查一轮，并切回快节奏
    kickPoll.current = () => {
      if (timer !== undefined) clearTimeout(timer);
      fastRounds = POLL_FAST_ROUNDS;
      void tick();
    };

    void tick();
    return () => {
      alive = false;
      if (timer !== undefined) clearTimeout(timer);
      kickPoll.current = () => {};
    };
  }, []);

  /**
   * 点击行为：
   *  - **分割线**      → 什么都不做（它只是个视觉分组，不是应用）
   *  - **文件夹**      → 展开里面的图标（浮层，见 panel_window.rs）
   *  - 固定但**未运行** → 启动（这是「日常启动应用」的入口）
   *  - 已在前台        → 最小化（再次点击收起）
   *  - 其余            → 激活它的窗口
   */
  const onActivate = useCallback(async (item: DockVisualItem, anchorX: number) => {
    if (item.separator) return;
    if (item.isFolder) {
      // 点文件夹 = 展开/收起。同一个文件夹再点一次是**关掉**（判断在 Rust 侧，
      // 它才知道浮层现在开着的是不是这个文件夹）
      try {
        const owner = await dockHwnd();
        if (!owner) return;
        await showPanelFolder({ ownerHwnd: owner, folderId: item.id, anchorX });
        setToast(null);
      } catch (e) {
        // 空文件夹之类要说话（不然点了没反应，用户以为坏了）
        console.warn("[dock] 展开文件夹失败", e);
        setToast(typeof e === "string" ? e : String(e));
        window.setTimeout(() => setToast(null), 2600);
      }
      return;
    }
    try {
      if (!item.appRunning) {
        if (!item.target) return;
        await launchApp(item.target, false);
      } else if (item.hasForeground && item.hwnd != null) {
        await minimizeApp(item.hwnd);
      } else if (item.hwnd != null) {
        await activateApp(item.hwnd);
      }
      setToast(null);
    } catch (e) {
      // 绝不静默失败：前台锁定 / 提权窗口 / 启动失败都要说清楚
      console.warn("[apps] 操作失败", e);
      setToast(typeof e === "string" ? e : String(e));
      setTimeout(() => setToast(null), 2600);
    }
    // 用户正盯着看白点出现/消失，别等下一次空闲轮询（那是白点延迟的来源）
    kickPoll.current();
  }, []);

  /**
   * 右键：弹出**浮层窗口**（右键菜单，见 src-tauri/src/panel_window.rs）。
   *
   * `item === null` = 右键落在面板空白处 → 关掉浮层。
   * 同一个图标再右键一次也是关掉 —— 那个「开关」判断在 Rust 侧做，
   * 因为「浮层现在开着没有」只有一处真相（否则看门线程关掉之后，
   * 前端以为还开着，再右键就变成「关掉一个已经关掉的浮层」= 打不开）。
   */
  const onContext = useCallback(
    async (item: DockVisualItem | null, anchorX: number) => {
      try {
        if (!item) {
          await hidePanel();
          return;
        }
        const owner = await dockHwnd();
        if (!owner) return;
        await showPanelMenu({ ownerHwnd: owner, appId: item.id, anchorX });
      } catch (e) {
        console.warn("[apps] 右键菜单失败", e);
        setToast(typeof e === "string" ? e : String(e));
        window.setTimeout(() => setToast(null), 2600);
      }
    },
    [],
  );

  /** 面板上任何一次按下：先关浮层（浮层里的动作会自己触发刷新） */
  const onDismissMenu = useCallback(() => {
    hidePanel().catch(() => {
      /* 浮层本来就没开：忽略 */
    });
  }, []);

  /**
   * 鼠标停在图标上 → 在**图标上方**显示名字（macOS 的小气泡）。
   *
   * 两个细节：
   *  - **延迟 120ms**：鼠标扫过一排图标时不要一路弹过去（停留一下才出更稳）；
   *  - **去重**：同一个图标上鼠标微动会重复触发 `mouseenter`，不重复请求 ——
   *    否则每次都走一遍"摆窗口 + 通知页面 + 等回调"的显示时序。
   */
  const hoverRef = useRef<{ id: string; timer: number | null }>({ id: "", timer: null });
  const onHover = useCallback((item: DockVisualItem, anchorX: number) => {
    // 分割线没有名字可显示
    if (item.separator) return;
    const h = hoverRef.current;
    if (h.id === item.id) return; // 还是同一个图标：什么都不做
    if (h.timer !== null) window.clearTimeout(h.timer);
    h.id = item.id;
    h.timer = window.setTimeout(() => {
      h.timer = null;
      void (async () => {
        try {
          const owner = await dockHwnd();
          if (!owner) return;
          await showIconLabel({
            ownerHwnd: owner,
            targetId: item.id,
            text: item.label,
            anchorX,
            inFolder: false,
          });
        } catch (e) {
          console.warn("[dock] 显示名字失败", e);
        }
      })();
    }, 120);
  }, []);

  const onHoverEnd = useCallback(() => {
    const h = hoverRef.current;
    if (h.timer !== null) {
      window.clearTimeout(h.timer);
      h.timer = null;
    }
    if (!h.id) return;
    h.id = "";
    hideIconLabel().catch(() => {
      /* 标签本来就没开：忽略 */
    });
  }, []);

  /**
   * 把一个图标拖到**文件夹**上：放进文件夹。
   *
   * 这是"把应用放进文件夹"的主要入口（iOS/Android 上也是这个动作）。
   * 成功后 Rust 会重画 Dock，新图标从顶层消失、出现在文件夹的预览里。
   */
  const onDropToFolder = useCallback(async (folderId: string, draggedId: string) => {
    try {
      await addAppToFolder(folderId, draggedId);
    } catch (e) {
      // 拒绝的理由（已在文件夹里 / 不能嵌套 / 分割线不能放）都要说清楚
      console.warn("[dock] 放进文件夹失败", e);
      setToast(typeof e === "string" ? e : String(e));
      window.setTimeout(() => setToast(null), 2600);
    }
    kickPoll.current();
  }, []);

  /**
   * 上报 Dock 尺寸给 Rust。
   * 窗口必须与面板严丝合缝 —— 亚克力是按整个窗口矩形铺的，
   * 窗口比面板大就会在周围露出一块灰底。
   */
  const onSizeChange = useCallback((w: number, h: number, r: number) => {
    setDockSize(w, h, r).catch((e) => console.warn("[dock] 上报尺寸失败", e));
  }, []);

  /**
   * Dock 内拖拽排序结束：把新顺序交给 Rust 落盘，然后立刻刷新。
   *
   * 拖拽过程中界面已经是新顺序了（靠 transform 平移），所以这里**不做乐观更新** ——
   * 直接以 Rust 落盘后的结果为准，避免两边不一致。Rust 会严格校验 id 集合，
   * 校验不过就整条拒绝（见 `store::reorder`），此时界面会在下一次刷新时回到原状。
   */
  const onReorder = useCallback(async (ids: string[]) => {
    try {
      await reorderDockApps(ids);
    } catch (e) {
      console.warn("[dock] 保存顺序失败", e);
      setToast(typeof e === "string" ? e : String(e));
      window.setTimeout(() => setToast(null), 2600);
    }
    kickPoll.current();
  }, []);

  /** 把图标拖出 Dock 松手 = 从 Dock 移除 */
  const onRemove = useCallback(async (id: string) => {
    try {
      await removeDockApp(id);
    } catch (e) {
      console.warn("[dock] 移除失败", e);
      setToast(typeof e === "string" ? e : String(e));
      window.setTimeout(() => setToast(null), 2600);
    }
    kickPoll.current();
  }, []);

  // 没有应用 / 出错时也要上报一个尺寸，否则窗口会停留在上一次的大小
  useEffect(() => {
    if (items.length === 0 || error) {
      onSizeChange(260, prefs?.panelHeight ?? PANEL_H, PANEL_RADIUS);
    }
  }, [items.length, error, onSizeChange, prefs?.panelHeight]);

  if (error) {
    return <div className="dock dock-error">应用枚举失败：{error}</div>;
  }

  if (items.length === 0) {
    // Dock 为空只可能是用户把图标都移除了 —— 显示什么完全由用户的列表决定，
    // 所以这里要告诉他怎么加回来，而不是（旧模型下的）「没有正在运行的应用」。
    return (
      <div className="dock dock-empty">
        Dock 是空的 · 从桌面拖一个程序进来，或右键托盘图标选「添加应用…」
      </div>
    );
  }

  return (
    <>
      <Dock
        items={items}
        iconSize={prefs?.iconSize ?? 44}
        iconGap={prefs?.iconGap ?? 10}
        panelHeight={prefs?.panelHeight ?? PANEL_H}
        magnification={prefs?.magnification ?? 0.35}
        onSizeChange={onSizeChange}
        onActivate={onActivate}
        onContextMenu={onContext}
        onDismissMenu={onDismissMenu}
        onReorder={onReorder}
        onRemove={onRemove}
        onDropToFolder={onDropToFolder}
        onHover={onHover}
        onHoverEnd={onHoverEnd}
      />
      {toast && <div className="dock-toast">{toast}</div>}
    </>
  );
}
