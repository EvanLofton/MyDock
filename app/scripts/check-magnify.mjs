// 鱼眼放大数学的独立数值校验（不依赖 DOM / 渲染）
// 先由 tsc 把 magnify.ts 编译到 .tmpcheck/，再运行本脚本。
import { computeLayout, scaleAt, DEFAULT_MAGNIFY, maxMagnifiedWidth } from "../.tmpcheck/magnify.js";

const o = DEFAULT_MAGNIFY;
const W = 56;
const GAP = 12;
const N = 8;
const widths = Array(N).fill(W);

let fail = 0;
const ok = (cond, msg) => {
  console.log(`${cond ? "PASS" : "FAIL"}  ${msg}`);
  if (!cond) fail++;
};
const near = (a, b, eps = 1e-9) => Math.abs(a - b) < eps;

console.log("== scaleAt ==");
ok(near(scaleAt(0, o), 1 + o.amount), `scaleAt(0) = 1+amount = ${(1 + o.amount).toFixed(3)}`);
ok(near(scaleAt(o.radius, o), 1), "scaleAt(radius) = 1");
ok(near(scaleAt(o.radius + 100, o), 1), "scaleAt(>radius) = 1（超出影响半径不放大）");
ok(near(scaleAt(-50, o), scaleAt(50, o)), "scaleAt 对称");

let mono = true;
let prev = scaleAt(0, o);
for (let d = 0; d <= o.radius; d += 1) {
  const s = scaleAt(d, o);
  if (s > prev + 1e-12) mono = false;
  prev = s;
}
ok(mono, "scaleAt 在 [0, radius] 上单调不增");

console.log("\n== 静止布局 ==");
const base = computeLayout(widths, null, GAP, o);
ok(base.items.every((it) => it.scale === 1 && it.dx === 0), "无光标时全部复位（scale=1, dx=0）");
ok(near(base.rowWidth, W * N + GAP * (N - 1)), `rowWidth = ${base.rowWidth}`);
ok(near(base.items[0].baseLeft, 0), "第一个图标 baseLeft = 0");
ok(
  near(base.items[N - 1].baseLeft + W, base.rowWidth),
  "最后一个图标右边界 = rowWidth（行内不溢出）",
);

console.log("\n== 放大布局（光标在整行中心）==");
const mag = computeLayout(widths, 0, GAP, o);
const baseCenters = base.items.map((it) => it.baseLeft + W / 2);
const magCenters = mag.items.map((it, i) => base.items[i].baseLeft + W / 2 + it.dx);
const mid = (a) => (a[0] + a[a.length - 1]) / 2;

ok(near(mid(magCenters), mid(baseCenters)), "整行中心保持不动");
ok(mag.items[3].scale > 1.5, `光标附近缩放 = ${mag.items[3].scale.toFixed(3)}（应 > 1.5）`);
ok(mag.items[3].scale >= mag.items[0].scale, "越靠光标放大越多");

let noOverlap = true;
let worst = Infinity;
for (let i = 0; i < N - 1; i++) {
  const rightI = magCenters[i] + (W * mag.items[i].scale) / 2;
  const leftNext = magCenters[i + 1] - (W * mag.items[i + 1].scale) / 2;
  worst = Math.min(worst, leftNext - rightI);
  if (rightI > leftNext + 1e-9) noOverlap = false;
}
ok(noOverlap, `放大后相邻图标不重叠（最小间隙 ${worst.toFixed(3)}px）`);

// 光标在边缘
const edge = computeLayout(widths, -1000, GAP, o);
ok(edge.items.every((it) => it.scale === 1), "光标远在行外时全部复位");

console.log("\n== 分割线（fixed：不放大 + 两侧间距更紧）==");
// Dock 上第 3 项是分割线：它不放大，邻居照样给它让位；而且它两侧的间距比图标之间更小
const SEP = 9;
const SEP_GAP = 4;
const w2 = [W, W, SEP, W, W];
const fixed = [false, false, true, false, false];
// 逐对间距：只要相邻的一方是分割线，就用 SEP_GAP
const gaps2 = [];
for (let i = 0; i + 1 < w2.length; i++) gaps2.push(fixed[i] || fixed[i + 1] ? SEP_GAP : GAP);
const mix = computeLayout(w2, 0, GAP, o, fixed, gaps2);
const baseMix = computeLayout(w2, null, GAP, o, fixed, gaps2);
ok(mix.items[2].scale === 1, `分割线缩放 = ${mix.items[2].scale}（应为 1）`);
ok(
  Math.abs(mix.items[2].scale - baseMix.items[2].scale) < 1e-12,
  "分割线的倍率不受光标影响",
);
{
  // 元素是 transform-origin: bottom center 缩放的 —— 中心不动，左右各扩 scale/2。
  // 用中心算边界，跟容器里 `left + translateX + scale` 的写法一致。
  const center = mix.items.map((it, i) => baseMix.items[i].baseLeft + w2[i] / 2 + it.dx);
  const left = center.map((c, i) => c - (w2[i] * mix.items[i].scale) / 2);
  const right = center.map((c, i) => c + (w2[i] * mix.items[i].scale) / 2);
  let bad = 0;
  let minGap = Infinity;
  for (let i = 0; i < w2.length - 1; i++) {
    minGap = Math.min(minGap, left[i + 1] - right[i]);
    if (right[i] > left[i + 1] + 1e-9) bad++; // 重叠
    if (left[i + 1] - right[i] <= 0) bad++; // 没有间隙
  }
  ok(bad === 0, `含分割线时相邻不重叠且留有空隙（最小间隙 ${minGap.toFixed(2)}px）`);
  ok(
    near(baseMix.rowWidth, W * 4 + SEP + GAP * 2 + SEP_GAP * 2),
    `静止行宽 = ${baseMix.rowWidth}（2 个图标间距 + 2 个分割线紧间距）`,
  );
  // 分割线到两侧图标的**可见空白** = 紧间距 + 半个分割线宽
  const half = SEP / 2;
  const visual = SEP_GAP + half;
  ok(
    visual < GAP + half && visual < 10,
    `分割线两侧可见空白 ${visual}px（图标间距 ${GAP}px；第一版是 ${GAP + 13 / 2}px，太松）`,
  );
}

// 面板宽度按「最大放大宽度」预留：分割线不能按放大算，否则会白留一段宽度
const magW = maxMagnifiedWidth(w2, GAP, o, fixed, gaps2);
const magWNoFixed = maxMagnifiedWidth(w2, GAP, o);
ok(
  magW < magWNoFixed,
  `含分割线的最大放大宽度 ${magW.toFixed(1)} < 不标记时 ${magWNoFixed.toFixed(1)}（少留了分割线的放大量）`,
);

console.log(fail === 0 ? "\n✅ 全部通过" : `\n❌ ${fail} 项失败`);
process.exit(fail === 0 ? 0 : 1);
