import { invoke } from "@tauri-apps/api/core";
import type { AppEntry } from "../types";

export async function listApps(): Promise<AppEntry[]> {
  return invoke<AppEntry[]>("list_apps");
}

export async function activateApp(hwnd: number): Promise<void> {
  return invoke("activate_app", { hwnd });
}

export async function minimizeApp(hwnd: number): Promise<void> {
  return invoke("minimize_app", { hwnd });
}

export async function closeApp(hwnd: number): Promise<void> {
  return invoke("close_app", { hwnd });
}

export async function launchApp(target: string, runAsAdmin = false): Promise<void> {
  return invoke("launch_app", { target, runAsAdmin });
}

/** Dock 自身窗口句柄（右键菜单以它为 owner，也用来算菜单该出现在哪儿） */
export async function dockHwnd(): Promise<number | null> {
  return invoke<number | null>("dock_hwnd");
}

/**
 * 与 Rust 侧 `store::Preferences` 对齐 —— **改一边必须改另一边**。
 *
 * 注意这里**故意不声明** `pinned` / `discovered`：它们是 Dock 自己维护的应用列表
 * 状态，Rust 侧 `set_preferences` 会忽略前端传来的值（防止 UI 漏字段把用户添加的
 * 应用清空）。前端拿到它们只是为了展示，不应该也无法写回。
 */
export interface Preferences {
  autoHide: boolean;
  hideDelayMs: number;
  iconSize: number;
  /** 面板高度（逻辑像素）。低于几何下限时前端会兜底抬高，见 Dock.tsx */
  panelHeight: number;
  /** 图标之间的间距（逻辑像素）。这是**方格**间距，视觉间距还会做光学补偿 */
  iconGap: number;
  magnification: number;
  showRunningIndicators: boolean;
  /** 给任务栏预留空间（任务栏自动隐藏时也预留） */
  reserveTaskbar: boolean;
  /** Dock 底边距基准底边的逻辑像素数 —— 由用户决定，0 = 贴底 */
  bottomOffset: number;
  launchAtLogin: boolean;
  glassAlpha: number;
  glassRgb: [number, number, number];
}

export async function getPreferences(): Promise<Preferences> {
  return invoke<Preferences>("get_preferences");
}

export async function setPreferences(prefs: Preferences): Promise<void> {
  return invoke("set_preferences", { prefs });
}

/**
 * 回收站里现在有多少项（BL-8）。当图标的**版本号**用：数字一变，缓存就作废、图标重取。
 * 取不到时返回 0（当作"空"）。
 */
export async function getRecycleBinItems(): Promise<number> {
  return invoke<number>("get_recycle_bin_items");
}

/** 自检回执：把 IPC 测试结果写回 Rust（仅供自动化自检使用） */
export async function reportTest(msg: string): Promise<void> {
  return invoke("selftest_report", { msg });
}

/** 保存 Dock 上图标的新顺序（Dock 内拖拽排序）。Rust 侧会严格校验 id 集合 */
export async function reorderDockApps(ids: string[]): Promise<void> {
  return invoke("reorder_dock_apps", { ids });
}

/** 从 Dock 上移除一个图标（把图标拖出 Dock 松手时用） */
export async function removeDockApp(id: string): Promise<void> {
  return invoke("remove_dock_app", { id });
}

/** 在 `afterId` 后面加一条分割线；`afterId` 为空表示追加到末尾 */
export async function addSeparator(afterId?: string | null): Promise<string> {
  return invoke<string>("add_separator", { afterId: afterId ?? null });
}

/** 打开设置窗口（已开则前置） */
export async function openSettings(): Promise<void> {
  return invoke("open_settings");
}

/**
 * Dock 的基础信息（只读）。
 * 字段与 Rust 侧 `commands::DockInfo` 一一对应 —— 改一边必须改另一边。
 */
export interface DockInfo {
  version: string;
  configPath: string;
  hwnd: string;
  /** Dock 上有多少个图标（= 用户列表里的应用数，不含分割线） */
  pinnedCount: number;
  /** 其中有多少条分割线（纯视觉分组） */
  separatorCount: number;
  /** 当前正在运行的应用数（不影响 Dock 上显示什么，只用来画白点） */
  runningCount: number;
  dpi: number;
  scale: number;
  /** `[left, top, right, bottom]`，物理像素 */
  dockRect: [number, number, number, number];
  /** `[宽, 高]`，逻辑像素 */
  dockLogical: [number, number];
  workArea: [number, number, number, number];
  autoHide: boolean;
  hideDelayMs: number;
  iconSize: number;
  /** 配置里的面板高度（逻辑像素）；实际生效值可能更大 */
  panelHeight: number;
  /** 图标之间的间距（逻辑像素） */
  iconGap: number;
  magnification: number;
  showRunningIndicators: boolean;
  reserveTaskbar: boolean;
  /** Dock 底边距基准底边的逻辑像素数（用户可调） */
  bottomOffset: number;
  launchAtLogin: boolean;
  glassRgb: [number, number, number];
  glassAlpha: number;
}

export async function getDockInfo(): Promise<DockInfo> {
  return invoke<DockInfo>("get_dock_info");
}

/** 右键菜单里的一项。字段与 Rust 侧 `menu::MenuItem` 一一对应 */
export interface MenuItem {
  id: string;
  label: string;
  enabled: boolean;
  /** 危险操作（移出 Dock）：红字 */
  danger: boolean;
  /** 视觉分隔线：没有 label，不可点 */
  divider: boolean;
}


/** 文件夹网格里的一格 */
export interface FolderItem {
  id: string;
  label: string;
  target: string;
  running: boolean;
}

/**
 * 浮层要渲染的内容 —— 菜单 / 文件夹 / 悬停标签是同一个窗口的三种形态。
 * 字段与 Rust 侧 `panel_window::PanelContent` 一一对应（`kind` 是 serde 的 tag）。
 */
export type PanelContent =
  | {
      kind: "menu";
      items: MenuItem[];
      rowH: number;
      dividerH: number;
    }
  | {
      kind: "folder";
      title: string;
      cols: number;
      cell: number;
      gap: number;
      items: FolderItem[];
    }
  | {
      /** 悬停标签：鼠标停在图标上时，图标上方那个小气泡 */
      kind: "label";
      text: string;
      fontSize: number;
    };

/**
 * 浮层页面要渲染的全部内容。字段与 Rust 侧 `panel_window::PanelPayload` 一一对应。
 *
 * 尺寸（行高 / 格子边长 / 列数 / 内边距 / 宽度）**由 Rust 给**，页面直接用：
 * 浮层窗口的尺寸是 Rust 按这同一组数字算出来的，页面另写一份 CSS 数值的话，
 * 两边一旦不一致就是「最后一行被裁掉」或「底部多一块空白」。
 */
export interface PanelPayload {
  seq: number;
  content: PanelContent;
  /** 背景偏暗 → 用浅色文字（Rust 按着色亮度算好的） */
  dark: boolean;
  pad: number;
  width: number;
  glassRgb: [number, number, number];
}

/**
 * 弹出右键菜单（浮层窗口，见 src-tauri/src/panel_window.rs）。
 *
 * 返回值 = 调用之后浮层是不是打开的。同一个图标再右键一次会关掉它（false）——
 * 前端**不需要**自己记「菜单开着没有」，那会有两个真相来源。
 *
 * `anchorX` 是光标在 Dock 窗口里的逻辑横坐标（浮层以它为中心）。
 */
export async function showPanelMenu(args: {
  ownerHwnd: number;
  appId: string;
  /** 光标在 **Dock 客户端**里的逻辑横坐标；`null` = 让 Rust 现取光标位置
   *  （从浮层里右键时用 —— 那时光标在浮层窗口上，页面拿不到 Dock 的坐标系） */
  anchorX: number | null;
}): Promise<boolean> {
  return invoke<boolean>("show_panel_menu", args);
}


/**
 * 展开一个**文件夹**（Dock 大文件夹点开后那个图标网格）。
 *
 * 和菜单共用同一个浮层窗口：同一时刻只可能有一个浮层 ——
 * 打开文件夹会自动关掉菜单，反之亦然。
 */
export async function showPanelFolder(args: {
  ownerHwnd: number;
  folderId: string;
  anchorX: number;
}): Promise<boolean> {
  return invoke<boolean>("show_panel_folder", args);
}

/** 关掉浮层（幂等）。Dock 页面上任何一次按下都会调它 */
export async function hidePanel(): Promise<void> {
  return invoke("hide_panel");
}

/**
 * 鼠标停在图标上 → 在**图标上方**显示名字（macOS 那种小气泡）。
 *
 * 为什么不用 `title`：那是 WebView2/系统原生 tooltip，方角、系统配色、
 * 出现在光标右下角，和 Dock 的玻璃观感不是一套。
 *
 * `inFolder`：图标在大文件夹里面 —— 气泡要贴在**文件夹面板**上方
 * （`anchorX` 也随之变成面板的客户端坐标）。
 */
export async function showIconLabel(args: {
  ownerHwnd: number;
  targetId: string;
  text: string;
  anchorX: number;
  inFolder: boolean;
}): Promise<boolean> {
  return invoke<boolean>("show_icon_label", args);
}

/** 鼠标移开 → 关掉悬停标签（不会误关右键菜单） */
export async function hideIconLabel(): Promise<void> {
  return invoke("hide_icon_label");
}

/** 浮层页面读取要渲染的内容（Rust → 页面的 eval 通知它来读） */
export async function getPanelState(): Promise<PanelPayload | null> {
  return invoke<PanelPayload | null>("panel_state");
}

/** 浮层页面渲染完成，可以显示了（见 panel_window.rs 的显示时序说明） */
export async function panelReady(seq: number): Promise<void> {
  return invoke("panel_ready", { seq });
}

/** 执行一个菜单项（动作全在 Rust 侧执行，页面只是渲染器） */
export async function panelInvoke(itemId: string): Promise<void> {
  return invoke("panel_invoke", { itemId });
}

/** 点了文件夹里的一个图标：启动 / 切到前台（判断也在 Rust 侧做） */
export async function panelLaunch(itemId: string): Promise<void> {
  return invoke("panel_launch", { itemId });
}

/** 把一个条目变成文件夹，返回新文件夹的 id */
export async function createFolder(appId: string): Promise<string> {
  return invoke<string>("create_folder", { appId });
}

/** 把一个顶层条目放进文件夹（把图标拖到文件夹上） */
export async function addAppToFolder(folderId: string, appId: string): Promise<void> {
  return invoke("add_app_to_folder", { folderId, appId });
}

/** 解散文件夹：里面的条目回到顶层，返回放出了几项 */
export async function dissolveFolder(folderId: string): Promise<number> {
  return invoke<number>("dissolve_folder", { folderId });
}

/**
 * 上报 Dock 的逻辑尺寸。
 * 窗口尺寸必须与面板严丝合缝 —— 亚克力是按整个窗口矩形铺的，
 * 窗口比面板大就会在周围露出一块灰底。
 */
export async function setDockSize(
  width: number,
  height: number,
  radius: number,
): Promise<void> {
  return invoke("set_dock_size", { width, height, radius });
}

const iconCache = new Map<string, IconImage>();
const inflight = new Map<string, Promise<IconImage>>();

/**
 * 光学归一化：把图形放大到**在画布内尽可能大**，并回报左右各留了多少白。
 *
 * # 为什么需要（实测数字，别再凭感觉调）
 *
 * 图标的画布是固定方的，但**图形自己在画布里留多少白各不相同**：
 *
 * | 图标 | 图形占画布宽 | 每侧留白（iconSize=38 时） |
 * |---|---|---|
 * | 回收站（shell 图标） | 75% | 4.8 逻辑像素 |
 * | 此电脑（shell 图标） | 88% | 2.4 |
 * | Edge / Chrome / QQ / 微信 | 100% | 0 |
 *
 * 布局间距明明都是 10，量出来的**视觉间距**（布局间距 + 左右留白）却是
 * 「此电脑↔回收站 = 17.1、回收站↔资源管理器 = 14.8、应用之间 = 10」——
 * 用户反馈的「左侧那三个图标的间隔太大」就是这个，不是布局排松了。
 *
 * # 规则：`contain`，绝不放大到画布之外
 *
 * `k = min(画布宽/图形宽, 画布高/图形高)`。⚠️ **不能**允许"纵向超出画布"：
 * 画布就是边界，超出去的部分会被 `drawImage` **裁掉**（第一版给了 1.1 倍纵向余量，
 * 结果回收站的图形顶部被切了一条 —— 自检的"贴边（被裁）"就是查这个的）。
 *
 * 已经填满的图标（应用图标）倍率算出来就是 1，**一律不动** —— 不改变现有观感。
 * 以**图形包围盒中心**为基准放大，不重新居中：图标在 Dock 上是底边对齐的，
 * 重新居中会让不同图标在竖直方向错位。
 *
 * 放大之后仍有留白的（例如回收站这种高瘦图形，横向填不满），
 * 由**布局层**把它补偿掉：`iconInsetX` 回报给 Dock，
 * 相邻间距扣掉两人的留白 → 视觉间距对所有图标都一致。
 */
const OPTICAL_MAX_SCALE = 1.6;

export interface IconImage {
  url: string | null;
  /**
   * 归一化之后，图形**每侧**在方格里的留白（0~0.5，方格宽度的比例）。
   * Dock 用 `iconInsetX × iconSize` 扣相邻间距，让视觉间距一致。
   */
  insetX: number;
}

/** alpha 超过阈值的包围盒（画布像素坐标，含边界） */
function alphaBBox(d: Uint8ClampedArray, w: number, h: number, threshold = 16) {
  let x0 = w;
  let y0 = h;
  let x1 = -1;
  let y1 = -1;
  for (let y = 0; y < h; y++) {
    const row = y * w * 4;
    for (let x = 0; x < w; x++) {
      if (d[row + x * 4 + 3] > threshold) {
        if (x < x0) x0 = x;
        if (x > x1) x1 = x;
        if (y < y0) y0 = y;
        if (y > y1) y1 = y;
      }
    }
  }
  if (x1 < 0) return null;
  return { x0, y0, x1, y1, w: x1 - x0 + 1, h: y1 - y0 + 1 };
}

/** 归一化 + 编码；同时回报归一化后图形的横向留白 */
function encodeOptical(
  src: HTMLCanvasElement,
  rgba: Uint8ClampedArray,
  w: number,
  h: number,
): IconImage {
  const box = alphaBBox(rgba, w, h);
  if (!box) return { url: src.toDataURL("image/png"), insetX: 0 };
  const k = Math.min(w / box.w, h / box.h, OPTICAL_MAX_SCALE);
  if (k <= 1.01) {
    // 已经填满画布（应用图标都走这条）：原样返回，留白按原始包围盒算
    const inset = Math.max(0, (w - box.w) / 2 / w);
    return { url: src.toDataURL("image/png"), insetX: inset };
  }

  const out = document.createElement("canvas");
  out.width = w;
  out.height = h;
  const octx = out.getContext("2d");
  if (!octx) return { url: src.toDataURL("image/png"), insetX: 0 };
  const cx = (box.x0 + box.x1 + 1) / 2;
  const cy = (box.y0 + box.y1 + 1) / 2;
  octx.setTransform(k, 0, 0, k, cx * (1 - k), cy * (1 - k));
  octx.drawImage(src, 0, 0);

  // 放大后图形的实际横向留白：受高度限制时（高瘦图形）横向填不满
  const drawnW = box.w * k;
  const inset = Math.max(0, Math.min(0.5, (w - drawnW) / 2 / w));
  return { url: out.toDataURL("image/png"), insetX: inset };
}

/**
 * 取应用图标：BGRA → PNG data URL，带缓存（同一个 exe 只取一次），
 * 并做光学归一化（见 `encodeOptical`）。
 *
 * Rust 侧返回的是 `[width:u32 LE][height:u32 LE][BGRA...]` 的原始字节
 * （走 Tauri 的二进制通道，避免几十 KB 被序列化成 JSON 数字数组）。
 * 这里用 canvas 做 BGRA→RGBA 转换与 PNG 编码 —— 比在 Rust 里引 PNG 编码库轻得多。
 *
 * `version` = **会变的那部分状态**，进缓存键。目前只有回收站用（BL-8）：
 * 它的图标有空 / 满两态，而图标是按 target 缓存的 —— 不把"里面有几项"编进键里，
 * 图标就永远停在第一次取到的那张（清空回收站也不变）。
 * 其它 target 永远传 0，行为与以前完全一样。
 */
export function getIconImage(path: string, size = 128, version = 0): Promise<IconImage> {
  const key = `${path}@${size}@${version}`;
  const cached = iconCache.get(key);
  if (cached) return Promise.resolve(cached);
  const existing = inflight.get(key);
  if (existing) return existing;

  const task = (async (): Promise<IconImage> => {
    let out: IconImage = { url: null, insetX: 0 };
    try {
      const buf = await invoke<ArrayBuffer>("get_icon_bgra", { path, size });
      if (buf && buf.byteLength > 8) {
        const dv = new DataView(buf);
        const w = dv.getUint32(0, true);
        const h = dv.getUint32(4, true);
        const n = w * h;
        if (n > 0 && buf.byteLength >= 8 + n * 4) {
          const src = new Uint8Array(buf, 8, n * 4);
          const canvas = document.createElement("canvas");
          canvas.width = w;
          canvas.height = h;
          const ctx = canvas.getContext("2d");
          if (ctx) {
            const img = ctx.createImageData(w, h);
            const d = img.data;
            // BGRA -> RGBA
            for (let i = 0, j = 0; i < n; i++, j += 4) {
              d[j] = src[j + 2];
              d[j + 1] = src[j + 1];
              d[j + 2] = src[j];
              d[j + 3] = src[j + 3];
            }
            ctx.putImageData(img, 0, 0);
            out = encodeOptical(canvas, d, w, h);
          }
        }
      }
    } catch (e) {
      console.warn("[icon] 取图标失败:", path, e);
    }
    iconCache.set(key, out);
    inflight.delete(key);
    return out;
  })();

  inflight.set(key, task);
  return task;
}

/** 只要 data URL 的老接口（设置页用；它不关心光学留白） */
export async function getIconUrl(path: string, size = 128): Promise<string | null> {
  return (await getIconImage(path, size)).url;
}
