import { useEffect, useMemo, useRef, useState } from "react";
import { computeLayout, DEFAULT_MAGNIFY, maxMagnifiedWidth } from "../lib/magnify";

export interface DockVisualItem {
  id: string;
  label: string;
  /** 图标底色（占位 / 图标加载失败时的兜底） */
  color: string;
  /** 真实图标（PNG data URL）；为空则用底色 + 首字母兜底 */
  iconUrl?: string | null;
  running?: boolean;
  /** 以管理员权限运行 —— 画盾牌角标 */
  elevated?: boolean;
  /** 该应用某个窗口的句柄，用于激活 / 最小化 */
  hwnd?: number | null;
  /** 该应用当前是否在前台 */
  hasForeground?: boolean;
  /** 窗口数（多窗口时点击行为可区分） */
  windowCount?: number;
  /** 可执行文件全路径（右键菜单里「打开文件所在位置」「以管理员权限运行」要用） */
  target?: string;
  /** 应用是否有窗口在运行（false = 没在运行，点击应「启动」） */
  appRunning?: boolean;
  /** 这一条是**分割线**（纯视觉分组）：不是应用，点击无动作 */
  separator?: boolean;
  /**
   * 图形在方格里的**横向留白**（逻辑像素，每侧）。
   *
   * 用途：不同图标的图形在画布里留白不同（「回收站」的图形只占方格宽 75%），
   * 于是"看起来的间距" = 布局间距 + 两边留白。布局用它把相邻间距扣掉，
   * 让**视觉间距**对所有图标一致（见 `lib/ipc.ts` 的 `encodeOptical`）。
   */
  iconInset?: number;
  /** 这一条是**文件夹**（Dock 大文件夹）：占一个图标位，点开显示里面的应用 */
  isFolder?: boolean;
  /**
   * 文件夹里的前几项，用来画图标上的 2×2 预览（**只前端用**，最多 4 个）。
   * 点开之后的完整网格由浮层窗口渲染（见 `panel_window.rs`）。
   */
  preview?: { id: string; iconUrl: string | null }[];
}

interface Props {
  items: DockVisualItem[];
  /** 图标边长（逻辑像素） */
  iconSize?: number;
  /** 图标之间的间距（逻辑像素，配置项 `iconGap`）。视觉间距还会做光学补偿 */
  iconGap?: number;
  /** 面板左右内边距 */
  pad?: number;
  /** 配置里的放大增量；会被几何上限自动收敛，避免放大后超出面板被裁 */
  magnification?: number;
  /** 面板高度（逻辑像素），配置项 `panelHeight`。会被几何下限兜底钳制 */
  panelHeight?: number;
  /** 尺寸变化时上报给 Rust（窗口必须与面板严丝合缝） */
  onSizeChange?: (width: number, height: number, radius: number) => void;
  /** 点击图标。`anchorX` 是光标在窗口里的逻辑横坐标（展开文件夹时浮层以它为中心） */
  onActivate?: (item: DockVisualItem, anchorX: number) => void;
  /**
   * 右键菜单。
   * `item = null` 表示右键落在**面板空白处**（不是图标）—— 用来关掉已打开的浮层。
   * `anchorX` 是光标在窗口里的逻辑横坐标（浮层以它为中心，见 `panel_window::place`）。
   */
  onContextMenu?: (item: DockVisualItem | null, anchorX: number) => void;
  /** 面板上任何一次非右键按下：用来关掉已打开的浮层 */
  onDismissMenu?: () => void;
  /** 拖拽排序结束：给出**新的完整 id 顺序** */
  onReorder?: (ids: string[]) => void;
  /** 把图标拖出 Dock 后松手 —— 视为「从 Dock 移除」 */
  onRemove?: (id: string) => void;
  /**
   * 把一个图标**拖到文件夹上**（松手时调用）。
   *
   * 这是"把应用放进文件夹"的主要入口：iOS/Android 上也是这个动作，
   * 比"先建文件夹、再去菜单里挑"顺手得多。
   */
  onDropToFolder?: (folderId: string, draggedId: string) => void;
  /**
   * 鼠标停在图标上 / 移开。
   *
   * `anchorX` 是光标在窗口里的逻辑横坐标 —— 名字标签以它为中心显示在**图标上方**
   * （macOS 的观感，见 `panel_window::show_label`）。
   */
  onHover?: (item: DockVisualItem, anchorX: number) => void;
  onHoverEnd?: () => void;
}

/** 面板高度的默认值（逻辑像素）。实际值由配置 `panelHeight` 给，见下面的 `panelH` */
export const PANEL_H = 72;
/** 面板圆角；必须与 Rust 侧窗口区域裁剪用同一个值 */
export const PANEL_RADIUS = 24;
/** 图标距面板底边 */
const ICON_BOTTOM = 8;
/** 顶部留一点点余量，避免放大后正好贴到边 */
const TOP_MARGIN = 2;
/** 面板高度至少要比「图标 + 底部间距 + 顶部余量」多这么多，才有放大的余地 */
const MIN_PANEL_EXTRA = 4;
/** 面板高度上限（逻辑像素）。再高就占掉小半个屏幕了 */
export const PANEL_H_MAX = 200;
/** 超过这个位移才算「拖拽」（否则当点击）：避免手抖就吞掉启动 */
const DRAG_THRESHOLD = 5;
/** 指针超过面板边缘这么多逻辑像素就算「拖出去了」，松手即移除 */
const DELETE_MARGIN = 26;
/**
 * 分割线在行里占的宽度（逻辑像素）。
 *
 * 它包含「线本身 + 两侧的空白」，之所以不设成 1px：那点宽度根本点不中，
 * 而分割线**必须能被右键、能被拖走**（用户要求位置完全由用户决定）。
 * 9 是折中：物理 11px 的命中区够用，又不会让一条 1px 的线浮在空地中央。
 */
const SEP_W = 9;
/**
 * 分割线**两侧**的间距（逻辑像素），比图标之间的 `iconGap`(默认 10) 更紧。
 *
 * 为什么单独给一个值：图标之间有 10px 是"两个图标不要挤在一起"，
 * 而分割线只是一条线 —— 套用同一个间距，它两侧就会各留出
 * `10 + SEP_W/2`（约 14.5px）的空白，整条线看起来是"浮在一大块空地中间"（用户反馈）。
 */
const SEP_GAP = 4;
/**
 * 相邻两项之间的**最小**间距（逻辑像素）。
 *
 * 光学补偿（扣掉图形留白）之后剩下的间距不能低于它 —— 否则两个图形会贴到一起，
 * 看起来像一组而不是两个。
 */
const MIN_GAP = 3;
/** 分割线的高度 = 图标边长 × 这个比例（跟着图标尺寸一起变） */
const SEP_H_RATIO = 0.68;

/** 拖拽中的状态。放在 ref 里 —— 每帧都要读，不能进 React state */
interface DragState {
  /** 被拖的图标下标 */
  index: number;
  pointerId: number;
  /** 按下时的指针位置（判断是否超过拖动阈值） */
  startX: number;
  startY: number;
  /** 指针相对**行左边缘**的 x（逻辑像素） */
  rowX: number;
  /** 指针相对**面板顶边**的 y；负数 = 到了面板上方 */
  panelY: number;
  /** 是否已经真的拖动过（决定松手时是「点击」还是「排序」） */
  moved: boolean;
  /** 当前是否在「拖出去要删除」的区域 */
  out: boolean;
  /** 松手会落在第几个槽位 */
  slot: number;
}

/** 计算相邻两项之间该留多少间距时只看这两样（避免传整个 item） */
interface GapItem {
  separator: boolean;
  inset: number;
}

/**
 * 相邻两项之间的间距 —— 目标是**视觉间距**一致，而不是"方格的间距"一致。
 *
 * 三种情况：
 *  1. 分割线两侧：用 `SEP_GAP`（它是一条线，不是图标 —— 套用图标间距会显得浮在空地中间）；
 *  2. 图形在画布里留白多的图标（如「回收站」只占方格宽 81%）：把留白从间距里扣掉；
 *  3. 其余（应用图标都填满画布）：就是 `iconGap`。
 *
 * ⚠️ 这个函数是**唯一**的间距规则。初始布局和拖拽时的让位预览都必须走它 ——
 * 两处各写一份的话，拖拽预览会和松手后的真实排布差几个像素（松手瞬间"跳"一下）。
 */
function gapBetween(a: GapItem, b: GapItem, iconGap: number): number {
  if (a.separator || b.separator) return SEP_GAP;
  return Math.max(MIN_GAP, iconGap - a.inset - b.inset);
}

/**
 * Dock 图标行。
 *
 * ## 四条必须一起成立的约束（改动前务必读）
 *
 * 1. **鼠标位置绝不进 React state**：每帧 `setState` 会重渲染整棵树；
 *    这里在 rAF 里直接写 `transform`，完全绕过 React 渲染。
 * 2. **`transform` 只能有一个写入者。** 鱼眼和拖拽都在动图标，如果各写各的就会
 *    互相覆盖（表现为闪烁/抽搐）。做法是**只有 `apply()` 一个函数写 transform**，
 *    它根据「当前是否在拖拽」走两条分支之一；拖拽期间**不做鱼眼放大**。
 *    （macOS 的 Dock 拖拽时也是停止放大的观感；我在这台机器上无法验证 macOS 的实现，
 *    但这个约束本身与平台无关 —— 同一个属性不能有两个作者。）
 * 3. **移动只用 `translateX + scale`**，不改 `width/left`，不触发布局。
 *    拖拽让位也是整槽平移（`slotW * Δ`），不是重排 DOM。
 * 4. **面板尺寸固定**，不随悬停/拖拽变化：亚克力是按整个窗口矩形铺的，
 *    面板若变宽而窗口不变就会露出灰底。所以拖拽**不需要**把窗口加高 ——
 *    实测指针拖到窗口外 158 逻辑像素处，`pointermove` / `pointerup` **仍然送达**
 *    （`setPointerCapture` 在这个 `WS_EX_NOACTIVATE` 工具窗口里可用，见 selftest 的
 *    `drive_drag_probe`）。这一点是「拖出去删除」能成立的前提。
 */
export function Dock({
  items,
  iconSize = 44,
  iconGap = 10,
  panelHeight = PANEL_H,
  pad = 12,
  magnification = DEFAULT_MAGNIFY.amount,
  onSizeChange,
  onActivate,
  onContextMenu,
  onDismissMenu,
  onReorder,
  onRemove,
  onDropToFolder,
  onHover,
  onHoverEnd,
}: Props) {
  const panelRef = useRef<HTMLDivElement>(null);
  const rowRef = useRef<HTMLDivElement>(null);
  const nodesRef = useRef<(HTMLButtonElement | null)[]>([]);

  /** 拖拽中（只用于 class，改变次数 = 每次拖拽 1 次，不进每帧路径） */
  const [dragging, setDragging] = useState(false);
  /** 当前处在「松手即删除」区域 */
  const [removeIntent, setRemoveIntent] = useState(false);
  /** 正在被拖的图标下标（只用于提示文字，同样不进每帧路径） */
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  /** 松手会落进的**文件夹**下标；只用于高亮，不进每帧路径 */
  const [dropFolderIndex, setDropFolderIndex] = useState<number | null>(null);

  /**
   * 最新的 `items`。`apply()` 跑在 rAF 里、effect 的依赖里没有 `items`，
   * 所以它闭包里的 `items` 会是旧的 —— 而"这一格是不是文件夹"必须是最新的
   * （新建文件夹时条目数不变，effect 不会重跑）。
   */
  const itemsRef = useRef(items);
  itemsRef.current = items;

  const dragRef = useRef<DragState | null>(null);
  /** 拖动过之后要吞掉紧随其后的 click —— 否则松手会启动程序 */
  const suppressClickRef = useRef(false);
  /** rAF 调度器；由下面那个 effect 装配，pointer 处理器调用它 */
  const scheduleRef = useRef<((pos: { x: number; y: number } | null) => void) | null>(null);

  const widths = useMemo(
    () => items.map((it) => (it.separator ? SEP_W : iconSize)),
    [items, iconSize],
  );
  /** 分割线不参与放大（见 `computeLayout` 的 `fixed` 参数） */
  const fixedWidths = useMemo(() => items.map((it) => !!it.separator), [items]);
  /** 各图标图形自己的横向留白（逻辑像素，每侧） */
  const insets = useMemo(() => items.map((it) => it.iconInset ?? 0), [items]);
  /** 间距规则只看这两样，抽出来给 `gapBetween` 用 */
  const gapItems = useMemo(
    () => items.map((it) => ({ separator: !!it.separator, inset: it.iconInset ?? 0 })),
    [items],
  );

  /** 逐对间距（规则见 `gapBetween`） */
  const gaps = useMemo(() => {
    const out: number[] = [];
    for (let i = 0; i + 1 < gapItems.length; i++) {
      out.push(gapBetween(gapItems[i], gapItems[i + 1], iconGap));
    }
    return out;
  }, [gapItems, iconGap]);

  /**
   * 实际生效的面板高度 = 配置值，但**有几何下限**。
   *
   * 下限 = 图标 + 底部间距 + 顶部余量 + 一点放大余地。低于它图标会被窗口裁掉。
   * 配置是人手写的，可能把面板调得比图标还矮，所以这里兜一道 ——
   * 宁可面板比配置高一点，也不要裁掉图标。
   */
  const panelH = Math.min(
    PANEL_H_MAX,
    Math.max(panelHeight, iconSize + ICON_BOTTOM + TOP_MARGIN + MIN_PANEL_EXTRA),
  );

  // 放大倍率的几何上限：图标最高只能到「面板高 - 底部间距 - 顶部余量」
  const maxIconH = panelH - ICON_BOTTOM - TOP_MARGIN;
  const maxAmount = Math.max(0, maxIconH / iconSize - 1);
  const amount = Math.min(Math.max(0, magnification), maxAmount);
  const opts = useMemo(() => ({ ...DEFAULT_MAGNIFY, amount }), [amount]);

  const baseLayout = useMemo(
    () => computeLayout(widths, null, iconGap, opts, fixedWidths, gaps),
    [widths, iconGap, opts, fixedWidths, gaps],
  );

  // 面板按「放大到最大时的行宽」定死，这样悬停时不需要改窗口
  const panelW = useMemo(() => {
    if (widths.length === 0) return pad * 2;
    return maxMagnifiedWidth(widths, iconGap, opts, fixedWidths, gaps) + pad * 2;
  }, [widths, iconGap, opts, pad, fixedWidths, gaps]);

  /** 各图标的静止左边界（相对行左边缘）。等宽时就是 i*(iconSize+iconGap) */
  const baseLefts = useMemo(() => baseLayout.items.map((it) => it.baseLeft), [baseLayout]);
  /** 静止行宽 —— 拖拽让位时要在它里面重新居中（拖拽期间面板尺寸不变） */
  const baseRowW = baseLayout.rowWidth;

  const cfgRef = useRef({ widths, iconGap, opts, baseLefts, baseRowW, fixedWidths, insets, gaps });
  cfgRef.current = { widths, iconGap, opts, baseLefts, baseRowW, fixedWidths, insets, gaps };

  // 上报尺寸：窗口必须与面板严丝合缝。
  // ⚠️ `panelH` 必须在依赖里 —— 少了它，**只改面板高度**时不会重新上报，
  // 窗口就停在旧高度（上一条自检实测：高度 +0 而不是 +35）。
  useEffect(() => {
    onSizeChange?.(panelW, panelH, PANEL_RADIUS);
  }, [panelW, panelH, onSizeChange]);

  // ---- 唯一的 transform 写入者 ----
  useEffect(() => {
    const row = rowRef.current;
    if (!row) return;

    let raf = 0;
    /** 上一次高亮过的文件夹下标（用来避免每帧 setState） */
    let lastDropFolder: number | null = null;

    const apply = (pos: { x: number; y: number } | null) => {
      const {
        widths: w,
        iconGap: g,
        opts: o,
        baseLefts: bl,
        baseRowW,
        fixedWidths: fx,
        insets: ins,
      } = cfgRef.current;
      if (w.length === 0) return;
      const rect = row.getBoundingClientRect();
      const d = dragRef.current;

      // ---- 分支一：拖拽中 ----
      if (d) {
        const n = w.length;
        const cursorRowX = pos ? pos.x - rect.left : d.rowX;
        if (pos) {
          d.rowX = cursorRowX;
          d.panelY = pos.y;
        }
        // 光标离哪个槽位中心最近 → 松手就插到那儿。这就是「实时显示正确位置」。
        // 注意**不能**用 `round(rowX / (图标宽+间距))`：分割线比图标窄，
        // 等差除法算出来的槽位会偏一格。
        d.slot = 0;
        let best = Infinity;
        for (let i = 0; i < n; i++) {
          const dist = Math.abs(cursorRowX - (bl[i] + w[i] / 2));
          if (dist < best) {
            best = dist;
            d.slot = i;
          }
        }

        // 让位：把被拖的项从数组里取出来、插到 slot，然后按**新顺序**算一遍布局。
        //
        // 为什么不是「整槽平移 k*槽宽」：分割线比图标窄，槽宽不再相等；
        // 而且行宽会随顺序变化（分割线在不在两端），所以新布局还要在**旧行宽**里
        // 重新居中 —— 旧行宽才是一行实际占的宽度（拖拽期间面板尺寸不变）。
        const order = w.map((_, i) => i).filter((i) => i !== d.index);
        order.splice(d.slot, 0, d.index);
        const nfx = order.map((i) => fx[i]);
        const ng: number[] = [];
        for (let i = 0; i + 1 < order.length; i++) {
          ng.push(
            gapBetween(
              { separator: !!nfx[i], inset: ins[i] },
              { separator: !!nfx[i + 1], inset: ins[i + 1] },
              g,
            ),
          );
        }
        const nl = computeLayout(
          order.map((i) => w[i]),
          null,
          g,
          o,
          nfx,
          ng,
        );
        const off = (baseRowW - nl.rowWidth) / 2;

        // 松手会落进的文件夹 → 高亮它（"放到这个文件夹里"）。
        // 只在**变化时** setState：每帧都设会让整棵树每帧重渲染，
        // 那是这个组件最忌讳的事（见文件头的四条约束）。
        const overFolder =
          itemsRef.current[d.slot]?.isFolder && d.slot !== d.index ? d.slot : null;
        if (overFolder !== lastDropFolder) {
          lastDropFolder = overFolder;
          setDropFolderIndex(overFolder);
        }

        for (let i = 0; i < n; i++) {
          const el = nodesRef.current[i];
          if (!el || i === d.index) continue;
          const j = order.indexOf(i);
          el.style.transform = `translateX(${(nl.items[j].baseLeft + off - bl[i]).toFixed(2)}px) scale(1)`;
        }

        const el = nodesRef.current[d.index];
        if (el) {
          const dx = d.rowX - w[d.index] / 2 - bl[d.index];
          // 竖直方向也跟手：往外拖时图标会跟着往上走、直到被窗口裁掉 ——
          // 这个「被裁掉」本身就是「要删了」的视觉反馈。
          // 图标的静止中心 y（图标是 bottom: ICON_BOTTOM 定位的）
          const iconCenterY = panelH - ICON_BOTTOM - w[d.index] / 2;
          const dy = d.panelY - iconCenterY;
          el.style.transform = `translate(${dx.toFixed(2)}px, ${dy.toFixed(2)}px) scale(1.08)`;
        }
        return;
      }

      // ---- 分支二：常规鱼眼 ----
      const cursorX = pos ? pos.x - (rect.left + rect.width / 2) : null;
      const layout = computeLayout(w, cursorX, g, o, fx);
      nodesRef.current.forEach((el, i) => {
        const it = layout.items[i];
        if (!el || !it) return;
        el.style.transform = `translateX(${it.dx.toFixed(2)}px) scale(${it.scale.toFixed(4)})`;
      });
    };

    scheduleRef.current = (pos) => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => apply(pos));
    };

    const onMove = (e: MouseEvent) => {
      // 拖拽期间由图标的 pointermove 驱动（指针可能在窗口外，行收不到 mousemove）
      if (dragRef.current) return;
      scheduleRef.current?.({ x: e.clientX, y: e.clientY });
    };
    const onLeave = () => {
      if (dragRef.current) return;
      scheduleRef.current?.(null);
    };

    row.addEventListener("mousemove", onMove);
    row.addEventListener("mouseleave", onLeave);
    apply(null);

    return () => {
      cancelAnimationFrame(raf);
      row.removeEventListener("mousemove", onMove);
      row.removeEventListener("mouseleave", onLeave);
      scheduleRef.current = null;
    };
    // `iconSize` 与 `panelH` 是闭包捕获的（不在 cfgRef 里），改了必须重挂 effect
  }, [items.length, iconSize, panelH]);

  // ---- 拖拽的指针事件 ----

  const onPointerDown = (e: React.PointerEvent<HTMLButtonElement>, index: number) => {
    if (e.button !== 0) return; // 只认左键
    const row = rowRef.current;
    if (!row) return;
    const rect = row.getBoundingClientRect();
    dragRef.current = {
      index,
      pointerId: e.pointerId,
      startX: e.clientX,
      startY: e.clientY,
      rowX: e.clientX - rect.left,
      panelY: e.clientY,
      moved: false,
      out: false,
      slot: index,
    };
    // 捕获指针 —— 这是「拖到窗口外仍能收到事件」的关键（已实测）
    e.currentTarget.setPointerCapture(e.pointerId);
    // 这里**不** preventDefault：没拖动时后面的 click 还要正常触发
  };

  const onPointerMove = (e: React.PointerEvent<HTMLButtonElement>) => {
    const d = dragRef.current;
    if (!d || e.pointerId !== d.pointerId) return;

    if (!d.moved) {
      const dist = Math.hypot(e.clientX - d.startX, e.clientY - d.startY);
      if (dist < DRAG_THRESHOLD) return; // 还没到阈值，仍按点击处理
      d.moved = true;
      setDragging(true);
      setDragIndex(d.index);
    }

    const out = e.clientY < -DELETE_MARGIN || e.clientY > panelH + DELETE_MARGIN;
    if (out !== d.out) {
      d.out = out;
      setRemoveIntent(out);
    }
    scheduleRef.current?.({ x: e.clientX, y: e.clientY });
  };

  const finishDrag = (e: React.PointerEvent<HTMLButtonElement>, cancelled: boolean) => {
    const d = dragRef.current;
    if (!d || e.pointerId !== d.pointerId) return;
    dragRef.current = null;
    setDragging(false);
    setRemoveIntent(false);
    setDragIndex(null);
    setDropFolderIndex(null);
    scheduleRef.current?.(null); // 复位所有 transform

    if (!d.moved) return; // 没拖动过：交给 click 走正常启动

    // 拖动过 → 吞掉紧随其后的 click（否则松手会启动程序）
    suppressClickRef.current = true;
    const el = e.currentTarget;
    if (el.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);

    if (cancelled) return;

    if (d.out) {
      const id = items[d.index]?.id;
      if (id) onRemove?.(id);
      return;
    }

    // ---- 拖到**文件夹**上 = 放进文件夹 ----
    //
    // 判据是"松手时落在哪一格"，和排序共用同一个 `slot`：
    // 落在文件夹那一格 → 进文件夹；否则照常排序。
    // 拖到自己身上、或拖的是文件夹本身，都不进（不支持嵌套）。
    const targetItem = items[d.slot];
    const draggedItem = items[d.index];
    if (
      targetItem?.isFolder &&
      targetItem.id !== draggedItem?.id &&
      !draggedItem?.isFolder
    ) {
      onDropToFolder?.(targetItem.id, draggedItem!.id);
      return;
    }

    if (d.slot !== d.index) {
      const ids = items.map((x) => x.id);
      const [moved] = ids.splice(d.index, 1);
      ids.splice(d.slot, 0, moved);
      onReorder?.(ids);
    }
  };

  const rowW = baseLayout.rowWidth;

  return (
    <div
      className={
        "dock" + (removeIntent ? " remove-intent" : "") + (dragging ? " dragging" : "")
      }
      ref={panelRef}
      // 面板上任何一次**非右键**按下都关掉菜单。右键不在这里关 ——
      // 右键图标是「换一个目标」或者「关掉」（同一个图标再右键一次），
      // 由 onContextMenu 决定，两边都管会互相打架。
      onMouseDown={(e) => {
        if (e.button !== 2) onDismissMenu?.();
      }}
      // 右键落在面板空白处（没点中任何图标）：关掉菜单，并且**阻止 WebView2 的默认菜单**
      // —— 不拦的话这里会弹出浏览器的「刷新 / 检查」菜单，那是明显不属于 Dock 的东西。
      onContextMenu={(e) => {
        e.preventDefault();
        onContextMenu?.(null, e.clientX);
      }}
    >
      {/* 拖到面板外时的**文字**提示。
          位置是面板顶部 y=0..20 那条空白带（图标占 y=20..64），所以不会盖住图标。
          面板只有 72px 高、又不能弹窗（会被窗口裁掉），这是唯一放得下文字的地方。
          带上应用名 —— 只说「移除」不够明确，用户得知道移除的是哪一个。 */}
      {removeIntent && (
        <div className="dock-hint">
          松开即移除
          {dragIndex != null && items[dragIndex]
            ? `「${items[dragIndex].separator ? "分割线" : items[dragIndex].label}」`
            : ""}
        </div>
      )}
      <div
        className="dock-row"
        ref={rowRef}
        style={{ width: rowW, left: "50%", marginLeft: -rowW / 2 }}
      >
        {items.map((it, i) => (
          <button
            key={it.id}
            ref={(el) => {
              nodesRef.current[i] = el;
            }}
            className={
              // 分割线 / 文件夹都复用同一个 button 元素：它们需要**完全一样**的
              // 拖拽 / 右键 / 点击行为（用户要求「位置完全由用户决定」）。
              // 区别只在画什么：`dock-sep` 画一条竖线，`dock-folder` 画 2×2 预览。
              "dock-icon" +
              (it.separator ? " dock-sep" : "") +
              (it.isFolder ? " dock-folder" : "") +
              (it.iconUrl ? " has-icon" : "") +
              (dragging && dragRef.current?.index === i ? " dragging" : "") +
              // 拖到它上面松手 = 放进这个文件夹（和"要删了"同一套高亮语言）
              (dragging && dropFolderIndex === i ? " drop-target" : "") +
              (removeIntent && dragRef.current?.index === i ? " will-remove" : "")
            }
            style={{
              width: widths[i],
              height: iconSize,
              left: baseLayout.items[i]?.baseLeft ?? 0,
              background: it.separator || it.iconUrl || it.isFolder ? "transparent" : it.color,
            }}
            // ❗**不用 `title`**：那是 WebView2/系统原生 tooltip（方角、系统配色、
            // 出现在光标右下角），和 Dock 的玻璃观感不是一套。名字由**浮层**画在
            // 图标上方（macOS 的观感，见 `onHover` → `panel_window::show_label`）。
            // 只用 aria-label 保留无障碍语义。
            aria-label={it.separator ? "分割线" : it.label}
            // 用**图标中心**而不是光标 x 当锚点：光标在同一个图标里怎么动，
            // 名字都不会跟着晃（macOS 也是钉在图标上的）。
            // `getBoundingClientRect` 反映的是变换后的位置，所以鱼眼放大时也跟得住。
            onMouseEnter={(e) => {
              // 拖拽过程中不弹名字：这时候指针在"搬东西"，标签跟着跑只是噪音
              // （而且它不是拖拽提示，拖拽提示在 Dock 上方那条 toast 上）。
              if (dragRef.current) return;
              const r = e.currentTarget.getBoundingClientRect();
              onHover?.(it, r.left + r.width / 2);
            }}
            onMouseLeave={() => onHoverEnd?.()}
            onPointerDown={(e) => onPointerDown(e, i)}
            onPointerMove={onPointerMove}
            onPointerUp={(e) => finishDrag(e, false)}
            onPointerCancel={(e) => finishDrag(e, true)}
            onClick={(e) => {
              // 拖动过就吞掉这次 click —— 需求：拖完松开不能触发启动
              if (suppressClickRef.current) {
                suppressClickRef.current = false;
                return;
              }
              onActivate?.(it, e.clientX);
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              // 不只是阻止冒泡：面板那层的处理会 `onContextMenu(null)`（= 关浮层）
              e.stopPropagation();
              onContextMenu?.(it, e.clientX);
            }}
          >
            {it.separator ? (
              <span
                className="dock-sep-line"
                style={{ height: Math.round(iconSize * SEP_H_RATIO) }}
              />
            ) : it.isFolder ? (
              // 大文件夹：一个圆角底 + 里面前 4 个应用的缩略图（2×2）。
              // 这是"一眼看出里面是什么"的关键 —— 空文件夹也不至于看起来像坏掉的图标。
              <span className="dock-folder-grid">
                {[0, 1, 2, 3].map((k) => {
                  const child = it.preview?.[k];
                  return (
                    <span key={child?.id ?? k} className="dock-folder-cell">
                      {child?.iconUrl ? (
                        <img src={child.iconUrl} alt="" draggable={false} />
                      ) : null}
                    </span>
                  );
                })}
              </span>
            ) : it.iconUrl ? (
              <img className="dock-icon-img" src={it.iconUrl} alt={it.label} draggable={false} />
            ) : (
              <span className="dock-icon-label">{it.label.slice(0, 1)}</span>
            )}
            {it.running && <span className="dock-dot" />}
            {/* 盾牌角标：`title` 会变成原生 tooltip（和名字提示一样的问题），
                所以只留无障碍语义，说明文字由悬停气泡承担 */}
            {it.elevated && <span className="dock-shield" role="img" aria-label="以管理员权限运行" />}
          </button>
        ))}
      </div>
    </div>
  );
}
