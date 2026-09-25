# T1 毛玻璃路线矩阵实测结果（v2）

基线（棋盘格直接可见）：mean = 127.5，std = 127.5

## 判定口径

| 分类 | 含义 |
|---|---|
| `BLUR_PERFECT` | 图案被完全抹平且亮度不变 —— 真实模糊 |
| `BLUR` | 图案被显著抹平且亮度基本不变 —— 真实模糊 |
| `TINT_OPAQUE` / `TINT_MIXED` | 图案被抹平但亮度大变 —— 只是罩了一层色，不是模糊 |
| `NO_EFFECT` | 图案依旧清晰 —— 完全没有玻璃 |
| `PARTIAL` | 部分弱化 |
| `INVALID_FOCUS` | 焦点未能按预期转移，该行结论不可用 |

## 全部结果

| 路线 | 窗口样式 | 焦点 | mean | std | std/基线 | 判定 | API | 实际前台窗口 |
|---|---|---|---|---|---|---|---|---|
| ctl-opaque | plain | focused | 76.2 | 0.0 | 0.00 | **TINT_OPAQUE** | n/a | `#ProbeGlass:glass` |
| ctl-opaque | plain | unfocused | 76.2 | 0.0 | 0.00 | **TINT_OPAQUE** | n/a | `#ProbeHolder:FOCUS-HOLDER` |
| ctl-opaque | plain | noactivate | 76.2 | 0.0 | 0.00 | **TINT_OPAQUE** | n/a | `#ProbeHolder:FOCUS-HOLDER` |
| ctl-opaque | layered | focused | 82.3 | 15.0 | 0.12 | **TINT_MIXED** | n/a | `#ProbeGlass:glass` |
| ctl-opaque | layered | unfocused | 82.3 | 15.0 | 0.12 | **TINT_MIXED** | n/a | `#ProbeHolder:FOCUS-HOLDER` |
| ctl-opaque | layered | noactivate | 82.3 | 15.0 | 0.12 | **TINT_MIXED** | n/a | `#ProbeHolder:FOCUS-HOLDER` |
| ctl-opaque | noredir | focused | 127.5 | 127.5 | 1.00 | **NO_EFFECT** | n/a | `#ProbeGlass:glass` |
| ctl-opaque | noredir | unfocused | 127.5 | 127.5 | 1.00 | **NO_EFFECT** | n/a | `#ProbeHolder:FOCUS-HOLDER` |
| ctl-opaque | noredir | noactivate | 127.5 | 127.5 | 1.00 | **NO_EFFECT** | n/a | `#ProbeHolder:FOCUS-HOLDER` |
| ctl-none | plain | focused | 127.5 | 127.5 | 1.00 | **NO_EFFECT** | n/a | `#ProbeGlass:glass` |
| ctl-none | plain | unfocused | 127.5 | 127.5 | 1.00 | **NO_EFFECT** | n/a | `#ProbeHolder:FOCUS-HOLDER` |
| ctl-none | plain | noactivate | 127.5 | 127.5 | 1.00 | **NO_EFFECT** | n/a | `#ProbeHolder:FOCUS-HOLDER` |
| ctl-none | layered | focused | 127.5 | 127.5 | 1.00 | **NO_EFFECT** | n/a | `#ProbeGlass:glass` |
| ctl-none | layered | unfocused | 127.5 | 127.5 | 1.00 | **NO_EFFECT** | n/a | `#ProbeHolder:FOCUS-HOLDER` |
| ctl-none | layered | noactivate | 127.5 | 127.5 | 1.00 | **NO_EFFECT** | n/a | `#ProbeHolder:FOCUS-HOLDER` |
| ctl-none | noredir | focused | 127.5 | 127.5 | 1.00 | **NO_EFFECT** | n/a | `#ProbeGlass:glass` |
| ctl-none | noredir | unfocused | 127.5 | 127.5 | 1.00 | **NO_EFFECT** | n/a | `#ProbeHolder:FOCUS-HOLDER` |
| ctl-none | noredir | noactivate | 127.5 | 127.5 | 1.00 | **NO_EFFECT** | n/a | `#ProbeHolder:FOCUS-HOLDER` |
| A-acrylic | plain | focused | 181.0 | 0.0 | 0.00 | **TINT_OPAQUE** | dwm:ok | `#ProbeGlass:glass` |
| A-acrylic | plain | unfocused | 181.0 | 0.0 | 0.00 | **TINT_OPAQUE** | dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-acrylic | plain | noactivate | 181.0 | 0.0 | 0.00 | **TINT_OPAQUE** | dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-acrylic | layered | focused | 175.0 | 15.0 | 0.12 | **TINT_MIXED** | dwm:ok | `#ProbeGlass:glass` |
| A-acrylic | layered | unfocused | 175.0 | 15.0 | 0.12 | **TINT_MIXED** | dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-acrylic | layered | noactivate | 175.0 | 15.0 | 0.12 | **TINT_MIXED** | dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-acrylic | noredir | focused | 181.0 | 0.0 | 0.00 | **TINT_OPAQUE** | dwm:ok | `#ProbeGlass:glass` |
| A-acrylic | noredir | unfocused | 181.0 | 0.0 | 0.00 | **TINT_OPAQUE** | dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-acrylic | noredir | noactivate | 181.0 | 0.0 | 0.00 | **TINT_OPAQUE** | dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-mica | plain | focused | 243.0 | 0.2 | 0.00 | **TINT_OPAQUE** | dwm:ok | `#ProbeGlass:glass` |
| A-mica | plain | unfocused | 243.0 | 0.2 | 0.00 | **TINT_OPAQUE** | dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-mica | plain | noactivate | 243.0 | 0.2 | 0.00 | **TINT_OPAQUE** | dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-mica | layered | focused | 229.5 | 15.0 | 0.12 | **TINT_MIXED** | dwm:ok | `#ProbeGlass:glass` |
| A-mica | layered | unfocused | 229.5 | 15.0 | 0.12 | **TINT_MIXED** | dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-mica | layered | noactivate | 229.5 | 15.0 | 0.12 | **TINT_MIXED** | dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-mica | noredir | focused | 243.0 | 0.2 | 0.00 | **TINT_OPAQUE** | dwm:ok | `#ProbeGlass:glass` |
| A-mica | noredir | unfocused | 243.0 | 0.2 | 0.00 | **TINT_OPAQUE** | dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-mica | noredir | noactivate | 243.0 | 0.2 | 0.00 | **TINT_OPAQUE** | dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-acrylic+extend | plain | focused | 181.0 | 0.0 | 0.00 | **TINT_OPAQUE** | extend:ok+dwm:ok | `#ProbeGlass:glass` |
| A-acrylic+extend | plain | unfocused | 181.0 | 0.0 | 0.00 | **TINT_OPAQUE** | extend:ok+dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-acrylic+extend | plain | noactivate | 181.0 | 0.0 | 0.00 | **TINT_OPAQUE** | extend:ok+dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-acrylic+extend | layered | focused | 175.0 | 15.0 | 0.12 | **TINT_MIXED** | extend:ok+dwm:ok | `#ProbeGlass:glass` |
| A-acrylic+extend | layered | unfocused | 175.0 | 15.0 | 0.12 | **TINT_MIXED** | extend:ok+dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-acrylic+extend | layered | noactivate | 175.0 | 15.0 | 0.12 | **TINT_MIXED** | extend:ok+dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-acrylic+extend | noredir | focused | 181.0 | 0.0 | 0.00 | **TINT_OPAQUE** | extend:ok+dwm:ok | `#ProbeGlass:glass` |
| A-acrylic+extend | noredir | unfocused | 181.0 | 0.0 | 0.00 | **TINT_OPAQUE** | extend:ok+dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| A-acrylic+extend | noredir | noactivate | 181.0 | 0.0 | 0.00 | **TINT_OPAQUE** | extend:ok+dwm:ok | `#ProbeHolder:FOCUS-HOLDER` |
| B-acrylic | plain | focused | 51.3 | 1.2 | 0.01 | **TINT_MIXED** | swca:1 | `#ProbeGlass:glass` |
| B-acrylic | plain | unfocused | 51.3 | 1.2 | 0.01 | **TINT_MIXED** | swca:1 | `#ProbeHolder:FOCUS-HOLDER` |
| B-acrylic | plain | noactivate | 51.3 | 1.2 | 0.01 | **TINT_MIXED** | swca:1 | `#ProbeHolder:FOCUS-HOLDER` |
| B-acrylic | layered | focused | 60.3 | 15.0 | 0.12 | **TINT_MIXED** | swca:1 | `#ProbeGlass:glass` |
| B-acrylic | layered | unfocused | 60.3 | 15.0 | 0.12 | **TINT_MIXED** | swca:1 | `#ProbeHolder:FOCUS-HOLDER` |
| B-acrylic | layered | noactivate | 60.3 | 15.0 | 0.12 | **TINT_MIXED** | swca:1 | `#ProbeHolder:FOCUS-HOLDER` |
| B-acrylic | noredir | focused | 51.3 | 1.2 | 0.01 | **TINT_MIXED** | swca:1 | `#ProbeGlass:glass` |
| B-acrylic | noredir | unfocused | 51.3 | 1.2 | 0.01 | **TINT_MIXED** | swca:1 | `#ProbeHolder:FOCUS-HOLDER` |
| B-acrylic | noredir | noactivate | 51.3 | 1.2 | 0.01 | **TINT_MIXED** | swca:1 | `#ProbeHolder:FOCUS-HOLDER` |

## 关键子集：失焦 / 不可激活（Dock 的真实工况）

| 路线 | 样式 | 焦点 | 判定 |
|---|---|---|---|
| ctl-opaque | plain | unfocused | **TINT_OPAQUE** |
| ctl-opaque | plain | noactivate | **TINT_OPAQUE** |
| ctl-opaque | layered | unfocused | **TINT_MIXED** |
| ctl-opaque | layered | noactivate | **TINT_MIXED** |
| ctl-opaque | noredir | unfocused | **NO_EFFECT** |
| ctl-opaque | noredir | noactivate | **NO_EFFECT** |
| ctl-none | plain | unfocused | **NO_EFFECT** |
| ctl-none | plain | noactivate | **NO_EFFECT** |
| ctl-none | layered | unfocused | **NO_EFFECT** |
| ctl-none | layered | noactivate | **NO_EFFECT** |
| ctl-none | noredir | unfocused | **NO_EFFECT** |
| ctl-none | noredir | noactivate | **NO_EFFECT** |
| A-acrylic | plain | unfocused | **TINT_OPAQUE** |
| A-acrylic | plain | noactivate | **TINT_OPAQUE** |
| A-acrylic | layered | unfocused | **TINT_MIXED** |
| A-acrylic | layered | noactivate | **TINT_MIXED** |
| A-acrylic | noredir | unfocused | **TINT_OPAQUE** |
| A-acrylic | noredir | noactivate | **TINT_OPAQUE** |
| A-mica | plain | unfocused | **TINT_OPAQUE** |
| A-mica | plain | noactivate | **TINT_OPAQUE** |
| A-mica | layered | unfocused | **TINT_MIXED** |
| A-mica | layered | noactivate | **TINT_MIXED** |
| A-mica | noredir | unfocused | **TINT_OPAQUE** |
| A-mica | noredir | noactivate | **TINT_OPAQUE** |
| A-acrylic+extend | plain | unfocused | **TINT_OPAQUE** |
| A-acrylic+extend | plain | noactivate | **TINT_OPAQUE** |
| A-acrylic+extend | layered | unfocused | **TINT_MIXED** |
| A-acrylic+extend | layered | noactivate | **TINT_MIXED** |
| A-acrylic+extend | noredir | unfocused | **TINT_OPAQUE** |
| A-acrylic+extend | noredir | noactivate | **TINT_OPAQUE** |
| B-acrylic | plain | unfocused | **TINT_MIXED** |
| B-acrylic | plain | noactivate | **TINT_MIXED** |
| B-acrylic | layered | unfocused | **TINT_MIXED** |
| B-acrylic | layered | noactivate | **TINT_MIXED** |
| B-acrylic | noredir | unfocused | **TINT_MIXED** |
| B-acrylic | noredir | noactivate | **TINT_MIXED** |

> 焦点控制成功 54/54 行。
