import { useCallback, useEffect, useRef, useState } from "react";
import { PANEL_H_MAX } from "../components/Dock";
import {
  getDockInfo,
  getIconUrl,
  getPreferences,
  listApps,
  setPreferences,
  type DockInfo,
  type Preferences,
} from "../lib/ipc";
import "./settings.css";

/** 基础信息的刷新周期。运行中应用数、窗口矩形这些都是实时值 */
const INFO_REFRESH_MS = 2000;
/** 拖色板/滑块时不要每动一下都写盘，攒一下再发 */
const SAVE_DEBOUNCE_MS = 150;
/** 前端 `.dock` 遮罩这一层的固定 alpha（与 glass.css 里保持一致） */
const CSS_LAYER_ALPHA = 0.34;
/** 预览里最多画几个图标 */
const PREVIEW_ICONS = 7;
/** 配置还没读回来时的兜底（与 Rust 侧默认值一致） */
const DEFAULT_RGB: [number, number, number] = [24, 24, 28];
const DEFAULT_ALPHA = 150;

/** 各滑块量程。放大倍率的几何上限约 0.41（见 store.rs 的推导），这里到 0.4 */
const RANGE = {
  bottomOffset: { min: 0, max: 500, step: 1, unit: "px" },
  iconSize: { min: 32, max: 72, step: 1, unit: "px" },
  // 面板高度：下限按「装得下当前图标」动态算（见 panelMin），上限取自 Dock.tsx
  panelHeight: { min: 0, max: PANEL_H_MAX, step: 1, unit: "px" },
  // 图标间距：0 = 贴到一起（几乎没法看），30 = 很松。
  // 这不是"逐对间距"—— 逐对还会按图形留白做光学补偿（见 Dock.tsx 的 gapBetween）
  iconGap: { min: 0, max: 30, step: 1, unit: "px" },
  magnification: { min: 0, max: 0.4, step: 0.01, unit: "×" },
  hideDelayMs: { min: 100, max: 2000, step: 20, unit: "ms" },
} as const;

const PRESETS: { name: string; rgb: [number, number, number]; alpha: number }[] = [
  { name: "石墨", rgb: [24, 24, 28], alpha: 150 },
  { name: "深空蓝", rgb: [16, 27, 44], alpha: 165 },
  { name: "暖褐", rgb: [40, 29, 22], alpha: 160 },
  { name: "墨绿", rgb: [18, 36, 30], alpha: 160 },
  { name: "浅雾", rgb: [205, 210, 220], alpha: 130 },
];

const FALLBACK_COLORS = [
  "linear-gradient(160deg,#5ac8fa,#0a84ff)",
  "linear-gradient(160deg,#ffd60a,#ff9f0a)",
  "linear-gradient(160deg,#ff453a,#ff375f)",
  "linear-gradient(160deg,#30d158,#248a3d)",
  "linear-gradient(160deg,#bf5af2,#5e5ce6)",
  "linear-gradient(160deg,#64d2ff,#0a84ff)",
  "linear-gradient(160deg,#ff9f0a,#ff6b22)",
];

function toHex([r, g, b]: [number, number, number]): string {
  return "#" + [r, g, b].map((v) => v.toString(16).padStart(2, "0")).join("");
}

function fromHex(s: string): [number, number, number] | null {
  const m = /^#?([0-9a-f]{6})$/i.exec(s.trim());
  if (!m) return null;
  const n = parseInt(m[1], 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

const pct = (v: number, min: number, max: number) =>
  `${(Math.max(0, Math.min(1, (v - min) / (max - min))) * 100).toFixed(2)}%`;

// ---------------------------------------------------------------- 小控件

/** 线性图标。用内联 SVG 而不是 unicode 字形 —— 后者在不同字体下会长得不一样。 */
function Icon({ name }: { name: "appearance" | "position" | "behavior" | "info" | "dock" }) {
  const p = {
    width: 17,
    height: 17,
    viewBox: "0 0 16 16",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.4,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };
  switch (name) {
    case "appearance":
      return (
        <svg {...p}>
          <circle cx="8" cy="8" r="6" />
          <path d="M8 2a6 6 0 0 0 0 12z" fill="currentColor" stroke="none" opacity=".45" />
        </svg>
      );
    case "position":
      return (
        <svg {...p}>
          <rect x="1.6" y="2.4" width="12.8" height="11.2" rx="2" />
          <rect x="4.6" y="9.4" width="6.8" height="2.2" rx="1.1" fill="currentColor" stroke="none" />
        </svg>
      );
    case "behavior":
      return (
        <svg {...p}>
          <rect x="1.4" y="5" width="13.2" height="6.4" rx="3.2" />
          <circle cx="11" cy="8.2" r="2.1" fill="currentColor" stroke="none" />
        </svg>
      );
    case "info":
      return (
        <svg {...p}>
          <circle cx="8" cy="8" r="6" />
          <path d="M8 7.2v3.6M8 5.1h.01" />
        </svg>
      );
    case "dock":
      return (
        <svg {...p} width={17} height={17} strokeWidth={1.5}>
          <rect x="1.5" y="4.5" width="3.4" height="7" rx="1.1" />
          <rect x="6.3" y="3" width="3.4" height="10" rx="1.1" />
          <rect x="11.1" y="4.5" width="3.4" height="7" rx="1.1" />
        </svg>
      );
  }
}

function Field({
  title,
  desc,
  children,
}: {
  title: string;
  desc?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="field">
      <div className="meta">
        <div className="t">{title}</div>
        {desc && <div className="d">{desc}</div>}
      </div>
      <div className="ctl">{children}</div>
    </div>
  );
}

function Slider({
  value,
  range,
  digits = 0,
  onChange,
}: {
  value: number;
  range: { min: number; max: number; step: number; unit: string };
  digits?: number;
  onChange: (v: number) => void;
}) {
  return (
    <>
      <input
        type="range"
        min={range.min}
        max={range.max}
        step={range.step}
        value={value}
        style={{ ["--fill" as string]: pct(value, range.min, range.max) }}
        onChange={(e) => onChange(Number(e.target.value))}
      />
      <span className="val">
        {value.toFixed(digits)}
        <span className="u">{range.unit}</span>
      </span>
    </>
  );
}

function Toggle({ on, onChange }: { on: boolean; onChange: (v: boolean) => void }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      className={"toggle" + (on ? " on" : "")}
      onClick={() => onChange(!on)}
    />
  );
}

function KV({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <>
      <div className="k">{k}</div>
      <div className={mono ? "v mono" : "v"}>{v}</div>
    </>
  );
}

// ---------------------------------------------------------------- 页面

const PANES = [
  { id: "appearance", label: "外观", icon: "appearance" as const, sub: "背景与图标的观感" },
  { id: "position", label: "位置", icon: "position" as const, sub: "Dock 停在屏幕的哪里" },
  { id: "behavior", label: "行为", icon: "behavior" as const, sub: "什么时候出现、显示什么" },
  { id: "info", label: "信息", icon: "info" as const, sub: "Dock 的运行时状态（只读）" },
];

interface PreviewItem {
  id: string;
  label: string;
  iconUrl: string | null;
  color: string;
}

export default function Settings() {
  const [info, setInfo] = useState<DockInfo | null>(null);
  const [prefs, setPrefs] = useState<Preferences | null>(null);
  const [previewItems, setPreviewItems] = useState<PreviewItem[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [applied, setApplied] = useState(false);
  const [pane, setPane] = useState<string>("appearance");

  const saveTimer = useRef<number | undefined>(undefined);

  useEffect(() => {
    getPreferences()
      .then(setPrefs)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const i = await getDockInfo();
        if (alive) setInfo(i);
      } catch (e) {
        if (alive) setError(String(e));
      }
    };
    void tick();
    const t = window.setInterval(tick, INFO_REFRESH_MS);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, []);

  // 预览用的图标：读一次就够（图标在 ipc.ts 里按 target 缓存，不会重复抽）
  useEffect(() => {
    let alive = true;
    listApps()
      .then(async (apps) => {
        const built = await Promise.all(
          apps.slice(0, PREVIEW_ICONS).map(async (a, i) => ({
            id: a.id,
            label: a.displayName,
            iconUrl: await getIconUrl(a.target, 64),
            color: FALLBACK_COLORS[i % FALLBACK_COLORS.length],
          })),
        );
        if (alive) setPreviewItems(built);
      })
      .catch(() => {
        /* 预览拿不到图标不影响设置功能，静默 */
      });
    return () => {
      alive = false;
    };
  }, []);

  /**
   * 改任意一项设置：**本地立刻反映**（滑块/色板要跟手），写盘防抖。
   *
   * Rust 侧收到后会同步给正在运行的 Dock（重应用亚克力、重算位置、通知页面重读配置），
   * 所以 Dock 本体基本是立刻跟着变的。
   *
   * 用「打补丁」而不是「传整份配置」：调用点只关心自己那一项，
   * 不会因为别的字段忘了带上而把设置改坏。
   */
  const apply = useCallback((patch: Partial<Preferences>) => {
    setApplied(false);
    setPrefs((cur) => {
      if (!cur) return cur;
      const next = { ...cur, ...patch };
      if (saveTimer.current !== undefined) clearTimeout(saveTimer.current);
      saveTimer.current = window.setTimeout(async () => {
        try {
          await setPreferences(next);
          setError(null);
          setApplied(true);
        } catch (e) {
          setError(String(e));
        }
      }, SAVE_DEBOUNCE_MS);
      return next;
    });
  }, []);

  const rgb = prefs?.glassRgb ?? DEFAULT_RGB;
  const alpha = prefs?.glassAlpha ?? DEFAULT_ALPHA;
  const offset = prefs?.bottomOffset ?? 0;
  const iconSize = prefs?.iconSize ?? 44;
  const mag = prefs?.magnification ?? 0.35;
  const autohide = prefs?.autoHide ?? false;
  const hideDelay = prefs?.hideDelayMs ?? 420;
  const showDots = prefs?.showRunningIndicators ?? true;
  const reserveTb = prefs?.reserveTaskbar ?? true;
  const launchAtLogin = prefs?.launchAtLogin ?? false;
  const panelHeight = prefs?.panelHeight ?? 72;
  /** 图标之间的间距（逻辑像素） */
  const iconGap = prefs?.iconGap ?? 10;
  const rgbCss = rgb.join(",");
  const alphaRatio = alpha / 255;

  /**
   * 面板高度的下限：必须装得下「图标 + 底部间距 8 + 顶部余量 2 + 一点放大余地」。
   * 比这个还矮的话图标会被窗口裁掉 —— 所以滑块的 min 跟着图标尺寸走，
   * 而不是写死一个数。公式与 `Dock.tsx` 里那处兜底钳制保持一致。
   */
  const panelMin = iconSize + 8 + 2 + 4;

  /**
   * 滚轮**不该**改滑块的值。
   *
   * ❗实测踩过（不是理论问题）：设置页开着、鼠标停在滑块上滚页面，
   * Chromium 会把滚轮当成「调滑块」—— 值被改掉还**自动存了盘**。
   *
   * 做法：在 range 上挂**非 passive** 的 wheel 监听（React 的 onWheel 是 passive 的，
   * `preventDefault` 不生效），拦掉浏览器的默认改值，然后**手动滚动内容区**。
   * 已经聚焦的滑块保留原生行为（键盘用户用方向键调值是合理的）。
   */
  const ready = prefs !== null;
  useEffect(() => {
    if (!ready) return;
    const scroller = document.querySelector(".content");
    const onWheel = (e: WheelEvent) => {
      const el = e.currentTarget as HTMLElement;
      if (document.activeElement === el) return;
      e.preventDefault();
      scroller?.scrollBy({ top: e.deltaY });
    };
    const inputs = Array.from(
      document.querySelectorAll<HTMLElement>('input[type="range"]'),
    );
    inputs.forEach((el) => el.addEventListener("wheel", onWheel, { passive: false }));
    return () => inputs.forEach((el) => el.removeEventListener("wheel", onWheel));
  }, [ready]);

  /**
   * 位置示意图：**按真实几何等比缩放**画，不是随手画。
   * 这样改「面板高度」或「距底部」时，图里的 Dock 条会真的变厚 / 上移 ——
   * 参数与效果对得上，图才有意义。
   */
  const panelH = info?.dockLogical[1] ?? panelHeight;
  const workH = info ? (info.workArea[3] - info.workArea[1]) / info.scale : 864;
  const maxOffset = Math.max(1, workH - panelH);
  // `.screen` 高 176、四周内边距 10 → 156px 代表 workH 个逻辑像素
  const mockScale = 156 / Math.max(1, workH);
  const mockTaskbarH = Math.max(7, Math.min(16, 60 * mockScale));
  const mockDockH = Math.max(5, Math.min(156 - mockTaskbarH - 4, panelH * mockScale));
  const mockDockBottom = 10 + mockTaskbarH + offset * mockScale;

  const activeSub = PANES.find((p) => p.id === pane)?.sub ?? "";

  return (
    <div className="app">
      {/* ---------------- 侧边栏 ---------------- */}
      <aside className="sidebar">
        <div className="brand">
          <div className="mark">
            <Icon name="dock" />
          </div>
          <div>
            <div className="name">Dock</div>
            <div className="ver">v{info?.version ?? "…"}</div>
          </div>
        </div>

        <nav className="nav">
          {PANES.map((p) => (
            <button
              key={p.id}
              className={pane === p.id ? "active" : ""}
              onClick={() => setPane(p.id)}
            >
              <span className="ico">
                <Icon name={p.icon} />
              </span>
              {p.label}
            </button>
          ))}
        </nav>

        <div className={"save" + (applied ? " on" : "")}>
          <span className="tick">✓</span> 已保存
        </div>
      </aside>

      {/* ---------------- 内容 ---------------- */}
      <main className="content">
        <div className="pane-head">
          <h1>{PANES.find((p) => p.id === pane)?.label}</h1>
          <p>{activeSub}</p>
        </div>

        {error && <div className="err">⚠ {error}</div>}

        {/* ============ 外观 ============ */}
        <section className={"pane" + (pane === "appearance" ? " active" : "")}>
          <div className="group">
            <div className="gtitle">预览</div>
            <div className="gdesc">
              背景是<b>两层</b>叠出来的：OS 亚克力（模糊 + 染色）与前端一层薄遮罩。
              这里的模糊用 <code>backdrop-filter</code> 近似，真实的是系统做的。
            </div>
            <div className="stage">
              <div className="wallpaper" />
              <div
                className="preview-dock"
                style={{ background: `rgba(${rgbCss}, ${alphaRatio.toFixed(3)})` }}
              >
                <div
                  className="preview-tint"
                  style={{ background: `rgba(${rgbCss}, ${CSS_LAYER_ALPHA})` }}
                />
                {previewItems.length === 0
                  ? Array.from({ length: PREVIEW_ICONS }).map((_, i) => (
                      <span
                        key={i}
                        className="ph"
                        style={{ background: "rgba(255,255,255,.14)" }}
                      />
                    ))
                  : previewItems.map((it) =>
                      it.iconUrl ? (
                        <img key={it.id} src={it.iconUrl} alt={it.label} draggable={false} />
                      ) : (
                        <span key={it.id} className="ph" style={{ background: it.color }}>
                          {it.label.slice(0, 1)}
                        </span>
                      ),
                    )}
              </div>
            </div>
          </div>

          <div className="group">
            <div className="gtitle">颜色</div>
            <div className="gdesc">
              「色调」同时喂给两层；「浓度」只控制亚克力那层 —— 越浓越实、越看不见背后的模糊。
            </div>

            <Field title="色调">
              <div className="color-ctl">
                <input
                  type="color"
                  value={toHex(rgb)}
                  onChange={(e) => {
                    const v = fromHex(e.target.value);
                    if (v) apply({ glassRgb: v });
                  }}
                  title="选择色调"
                />
                <input
                  className="hex"
                  value={toHex(rgb)}
                  spellCheck={false}
                  onChange={(e) => {
                    const v = fromHex(e.target.value);
                    if (v) apply({ glassRgb: v });
                  }}
                />
              </div>
            </Field>

            <Field title="浓度" desc="亚克力着色的不透明度">
              <Slider
                value={alpha}
                range={{ min: 30, max: 255, step: 1, unit: "" }}
                onChange={(v) => apply({ glassAlpha: v })}
              />
            </Field>

            <Field title="预设">
              <div className="swatches">
                {PRESETS.map((p) => (
                  <button
                    key={p.name}
                    className="swatch"
                    onClick={() => apply({ glassRgb: p.rgb, glassAlpha: p.alpha })}
                    title={`${toHex(p.rgb)} · 浓度 ${p.alpha}`}
                  >
                    <span
                      className="dot"
                      style={{
                        background: `rgba(${p.rgb.join(",")}, ${(p.alpha / 255).toFixed(2)})`,
                      }}
                    />
                    {p.name}
                  </button>
                ))}
              </div>
            </Field>
          </div>

          <div className="group">
            <div className="gtitle">尺寸</div>
            <Field title="图标边长">
              <Slider
                value={iconSize}
                range={RANGE.iconSize}
                onChange={(v) => apply({ iconSize: v })}
              />
            </Field>
            <Field
              title="面板高度"
              desc={`Dock 那条玻璃的总高。下限 ${panelMin}（装得下当前图标）`}
            >
              {/* 下限跟着图标尺寸走：面板比图标还矮的话，图标会被窗口裁掉 */}
              <Slider
                value={Math.max(panelHeight, panelMin)}
                range={{ ...RANGE.panelHeight, min: panelMin }}
                onChange={(v) => apply({ panelHeight: v })}
              />
            </Field>
            <Field
              title="图标间距"
              desc="看不出你想找的图标时调大；想在一屏里塞更多就调小"
            >
              <Slider
                value={iconGap}
                range={RANGE.iconGap}
                onChange={(v) => apply({ iconGap: v })}
              />
            </Field>
            <Field title="悬停放大" desc="鼠标经过时图标放大的比例，上限由面板高度决定">
              <Slider
                value={mag}
                range={RANGE.magnification}
                digits={2}
                onChange={(v) => apply({ magnification: v })}
              />
            </Field>
          </div>
        </section>

        {/* ============ 位置 ============ */}
        <section className={"pane" + (pane === "position" ? " active" : "")}>
          <div className="group">
            <div className="gtitle">距屏幕底边</div>
            <div className="gdesc">
              虚线框是「基准底边」—— 已经给任务栏留了空间，所以调大了 Dock 只会整体上浮。
            </div>

            <div className="screen">
              <div className="workarea" />
              <div className="corner">屏幕</div>
              <div className="taskbar" style={{ height: mockTaskbarH }} />
              <div
                className="dockbar"
                style={{ bottom: mockDockBottom, height: mockDockH }}
              />
              <div className="cap">
                面板高 {Math.round(panelH)} · 距基准底边 {offset} 逻辑像素
              </div>
            </div>

            <Field title="距底部" desc={`可用行程 0 – ${Math.round(maxOffset)} 逻辑像素`}>
              <Slider
                value={offset}
                range={RANGE.bottomOffset}
                onChange={(v) => apply({ bottomOffset: v })}
              />
            </Field>
          </div>

          <div className="group">
            <div className="gtitle">任务栏</div>
            <Field
              title="给任务栏留空间"
              desc="任务栏自动隐藏时也留出它弹出来的高度，避免它弹出时盖住 Dock"
            >
              <Toggle on={reserveTb} onChange={(v) => apply({ reserveTaskbar: v })} />
            </Field>
            {/* 这里曾经有一个「接管任务栏背景」的开关（往任务栏上敷亚克力）。
                实测证明那条路**只能让任务栏变得更实**、做不到"更透明"，
                而 Win11 上真正的完全透明需要注入 explorer.exe（见 backlog BL-17），
                所以撤掉了 —— 留一个做不到承诺的开关比没有更糟。 */}
          </div>
        </section>

        {/* ============ 行为 ============ */}
        <section className={"pane" + (pane === "behavior" ? " active" : "")}>
          <div className="group">
            <div className="gtitle">自动隐藏</div>
            <Field title="贴边自动隐藏" desc="鼠标移到屏幕最底边时唤出，离开后自动收起">
              <Toggle on={autohide} onChange={(v) => apply({ autoHide: v })} />
            </Field>
            {autohide && (
              <Field title="收起延迟" desc="鼠标离开后等多久收起">
                <Slider
                  value={hideDelay}
                  range={RANGE.hideDelayMs}
                  onChange={(v) => apply({ hideDelayMs: v })}
                />
              </Field>
            )}
          </div>

          <div className="group">
            <div className="gtitle">显示</div>
            <Field title="运行指示点" desc="给正在运行的应用在图标下方画一个白点">
              <Toggle on={showDots} onChange={(v) => apply({ showRunningIndicators: v })} />
            </Field>
            <Field title="开机自启" desc="登录 Windows 后自动启动 Dock（写当前用户的 Run 项，不需要管理员）">
              <Toggle on={launchAtLogin} onChange={(v) => apply({ launchAtLogin: v })} />
            </Field>
          </div>
        </section>

        {/* ============ 信息 ============ */}
        <section className={"pane" + (pane === "info" ? " active" : "")}>
          <div className="group">
            <div className="gtitle">应用</div>
            <div className="kv">
              <KV
                k="Dock 上的图标"
                v={`${info?.pinnedCount ?? "…"} 个${
                  info?.separatorCount ? ` + ${info.separatorCount} 条分割线` : ""
                }（顺序完全由你决定）`}
              />
              <KV k="正在运行" v={`${info?.runningCount ?? "…"} 个（只影响白点，不影响位置）`} />
            </div>
          </div>

          <div className="group">
            <div className="gtitle">窗口</div>
            <div className="kv">
              <KV k="句柄" v={info?.hwnd ?? "…"} mono />
              <KV
                k="位置"
                v={
                  info
                    ? `(${info.dockRect[0]}, ${info.dockRect[1]})`
                    : "…"
                }
              />
              <KV
                k="面板尺寸"
                v={
                  info
                    ? `${info.dockLogical[0]} × ${info.dockLogical[1]} 逻辑像素${
                        info.panelHeight !== Math.round(info.dockLogical[1])
                          ? `（配置 ${info.panelHeight}，被几何下限抬高了）`
                          : ""
                      }`
                    : "…"
                }
              />
              <KV
                k="屏幕工作区"
                v={
                  info
                    ? `(${info.workArea[0]}, ${info.workArea[1]}) – (${info.workArea[2]}, ${info.workArea[3]})`
                    : "…"
                }
              />
            </div>
          </div>

          <div className="group">
            <div className="gtitle">环境</div>
            <div className="kv">
              <KV
                k="缩放 / DPI"
                v={info ? `${info.scale.toFixed(2)}× / ${info.dpi}` : "…"}
              />
              <KV k="配置文件" v={info?.configPath ?? "…"} mono />
            </div>
          </div>
        </section>
      </main>
    </div>
  );
}
