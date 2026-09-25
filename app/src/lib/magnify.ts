/**
 * 鱼眼放大（macOS Dock 的 magnification）纯数学。
 *
 * 本模块**不依赖 DOM、不依赖 React**，因此可以直接单测。
 * 组件层只负责把算出来的 `baseLeft / scale / dx` 写进 style。
 *
 * 设计要点（都在文档 §4.3 里有对应说明）：
 *  1. 缩放用**余弦平方**衰减，在影响半径边界处一阶导为 0，不会出现折角；
 *  2. 图标放大后**不能互相重叠** —— 按「放大后的宽度」重新顺序排布来保证；
 *  3. **整行中心保持不动** —— 否则光标附近放大时整排会整体漂移，很晕；
 *  4. 最终只输出 `translateX + scale`，**绝不改 width/left**，避免每帧触发布局。
 */

export interface MagnifyOptions {
  /** 影响半径（逻辑像素）：距光标超过它的图标不放大 */
  radius: number;
  /** 最大放大增量：0.7 表示光标正下方的图标放大到 1.7 倍 */
  amount: number;
}

export const DEFAULT_MAGNIFY: MagnifyOptions = { radius: 150, amount: 0.7 };

/**
 * 距离 → 缩放倍率。
 * `scaleAt(0) === 1 + amount`，`scaleAt(radius) === 1`，中间平滑过渡。
 */
export function scaleAt(distance: number, o: MagnifyOptions = DEFAULT_MAGNIFY): number {
  const d = Math.abs(distance);
  if (d >= o.radius) return 1;
  const t = Math.cos((Math.PI * d) / (2 * o.radius));
  return 1 + o.amount * t * t;
}

export interface PlacedIcon {
  /**
   * 静止状态的左边界，**相对行的左边缘（0 基）** —— 可以直接写进 CSS `left`。
   *
   * 注意：内部排布是以「行中心为 0」算的，这里已经加回 rowWidth/2 转成 0 基。
   * 早期版本直接把中心坐标当 `left` 用，图标被推到行外（实测偏左 320px），
   * 这个换算不能省。
   */
  baseLeft: number;
  /** 放大倍率 */
  scale: number;
  /** 相对静止位置的水平位移（逻辑像素） */
  dx: number;
}

export interface DockLayout {
  /** 静止状态的整行宽度 */
  rowWidth: number;
  /** 放大后的整行宽度（用于同步背景面板，可选） */
  magnifiedWidth: number;
  items: PlacedIcon[];
}

/**
 * 按给定宽度顺序排布，返回各项左边界；整行以 0 为中心。
 *
 * `gapAt(i)` 给出第 i 项与第 i+1 项之间的间距 —— 间距做成**逐对**而不是一个标量，
 * 是因为分割线两侧要更紧（见 `computeLayout` 的 `gaps` 参数）。
 */
function rowLefts(widths: number[], gapAt: (i: number) => number): number[] {
  let total = 0;
  for (let i = 0; i < widths.length; i++) {
    total += widths[i];
    if (i < widths.length - 1) total += gapAt(i);
  }
  let x = -total / 2;
  return widths.map((w, i) => {
    const left = x;
    x += w + gapAt(i);
    return left;
  });
}

/**
 * 计算整行布局。
 *
 * @param widths  各图标静止宽度（逻辑像素）
 * @param cursorX 光标相对**整行中心**的横坐标；null 表示无光标（全部复位）
 * @param gap     图标间距（默认值；`gaps` 给了就按 `gaps`）
 * @param o       放大参数
 * @param fixed   可选：`fixed[i] === true` 的项**不放大**（宽度与倍率都不变）。
 *                Dock 上的**分割线**用它 —— 分割线是分隔符不是图标，
 *                跟着邻居一起长大在观感上是错的（而且一条 1px 的线放大 1.7 倍
 *                只会变粗，不会"变大"）。它仍然参与排布，所以邻居让位照常发生。
 * @param gaps    可选：逐对间距（长度应为 `n-1`），覆盖 `gap`。
 *                分割线两侧要比图标之间更紧，否则一条 1px 的竖线会浮在一大块空白里。
 *                **规则由调用方决定**（Dock.tsx 知道谁是分割线），这里只管几何。
 */
export function computeLayout(
  widths: number[],
  cursorX: number | null,
  gap: number,
  o: MagnifyOptions = DEFAULT_MAGNIFY,
  fixed?: boolean[],
  gaps?: number[],
): DockLayout {
  const n = widths.length;
  const gapAt = (i: number) => gaps?.[i] ?? gap;
  const gapTotal = widths.slice(0, Math.max(0, n - 1)).reduce((a, _, i) => a + gapAt(i), 0);
  const baseWidths = widths.slice();
  const baseLefts = rowLefts(baseWidths, gapAt);
  const baseRowWidth = baseWidths.reduce((a, b) => a + b, 0) + gapTotal;

  if (n === 0) {
    return { rowWidth: 0, magnifiedWidth: 0, items: [] };
  }

  // 静止中心 = 左边界 + 半宽
  const baseCenters = baseLefts.map((l, i) => l + baseWidths[i] / 2);

  const scales =
    cursorX === null
      ? baseCenters.map(() => 1)
      : baseCenters.map((c, i) => (fixed?.[i] ? 1 : scaleAt(cursorX - c, o)));

  // 按放大后的宽度重排 —— 这一步是「不重叠」的保证
  const magWidths = baseWidths.map((w, i) => w * scales[i]);
  const magLefts = rowLefts(magWidths, gapAt);
  const magCenters = magLefts.map((l, i) => l + magWidths[i] / 2);

  const items: PlacedIcon[] = baseWidths.map((_, i) => ({
    // 转成 0 基（行左边缘 = 0），方便直接用作 CSS left
    baseLeft: baseLefts[i] + baseRowWidth / 2,
    scale: scales[i],
    dx: magCenters[i] - baseCenters[i],
  }));

  const magnifiedWidth = magWidths.reduce((a, b) => a + b, 0) + gapTotal;
  return { rowWidth: baseRowWidth, magnifiedWidth, items };
}

/** 行内最亮的缩放峰值位置（调试/自检用） */
export function peakScale(layout: DockLayout): number {
  return layout.items.reduce((m, it) => Math.max(m, it.scale), 1);
}

/**
 * 放大到最大时整行的宽度。
 *
 * 用来给窗口/面板**预留固定尺寸**：亚克力是按整个窗口矩形铺的，
 * 如果面板在悬停时变宽而窗口不变，就会露出灰底；反过来若窗口跟着变，
 * 又要每帧改窗口（闪烁 + 开销）。所以面板直接按「最大宽度」定死。
 *
 * 取法：光标落在每个图标中心时分别算一次，取最大值
 * （放大后的总宽在光标对准某个图标时达到峰值）。
 */
export function maxMagnifiedWidth(
  widths: number[],
  gap: number,
  o: MagnifyOptions = DEFAULT_MAGNIFY,
  fixed?: boolean[],
  gaps?: number[],
): number {
  if (widths.length === 0) return 0;
  const base = computeLayout(widths, null, gap, o, fixed, gaps);
  const center = base.rowWidth / 2;
  let best = base.rowWidth;
  for (let i = 0; i < widths.length; i++) {
    const c = base.items[i].baseLeft + widths[i] / 2;
    const layout = computeLayout(widths, c - center, gap, o, fixed, gaps);
    if (layout.magnifiedWidth > best) best = layout.magnifiedWidth;
  }
  return best;
}
