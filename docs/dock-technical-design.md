# Windows 11 macOS 风格 Dock（程序坞）技术方案

| 项目 | 内容 |
|---|---|
| 代号 | Dock |
| 目标平台 | Windows 11（build 22000+，亚克力效果建议 22621+） |
| 技术栈 | Tauri v2 + Rust + WebView2 + **React** |
| 文档状态 | 设计草案 **v5**（Q1-Q5 决策 + P0 实测 + P1 全部复验与修正；**P1 已实现完成**） |
| 编写环境 | Win11 build 26200 / Node 24 / Rust 1.95 |

> **v2 变更摘要**：Q1 与任务栏并存（改用 `rcWork` 定位）；Q2 改为「右键菜单提供以管理员身份运行」，Dock 自身不提权；Q3 前端定为 React；Q4 不做多语言与自动更新；Q5 最小化动画改为「缩放效果优先、精灵效果可选」。详见 §11 决策记录。
>
> **v3 变更摘要**：P0 可行性验证完成。毛玻璃**选定路线 A**（官方 DWM 系统背景）；社区流传的「失焦后 acrylic 失效」**实测未复现**并已删除相关变通做法；新增同进程焦点约束（必须补 `WM_MOUSEACTIVATE → MA_NOACTIVATE`）；风险 R5 销案，新增 R15/R16/R17。**完整实测数据见 `docs/p0-report.md`**。
>
> **v4 变更摘要**：P1 在**真实 Tauri 窗口**上复验，发现 **P0 的毛玻璃路线结论不成立**——路线 A 在 WebView2 子窗口覆盖客户端的拓扑下完全不产生模糊（剖面平坦、对比度 0），**改选路线 B**。同时落地了窗口层六项（`NOACTIVATE` / `TOOLWINDOW` / 去 `LAYERED` / `WM_MOUSEACTIVATE` 子类化 / 毛玻璃 / `rcWork×DPI` 定位），R15 经**受控实验**确认钩子有效。详见 `docs/p1-window-layer-findings.md`。
>
> **v5 变更摘要**：P1 **实现完成**。三项重要修正：① R15 的钩子在真实点击时**走不到**（消息落到无法子类化的深层 Chromium 窗口），实际防护改为「Dock 不创建会获得焦点的自有窗口」；② R18（与自动隐藏任务栏抢底边）**已解决**，量 `Shell_TrayWnd` 几何后预留空间，实测零重叠；③ 修正 §2.1 的内存结论——**实测 191 MB，不是 40-80 MB**，Tauri 的真实优势是包体与 Rust 互操作。另：自动隐藏采用**单窗口**方案而非原设计的双窗口（见 §3.2 注）。项目入口见 `README.md`。

---

## 1. 项目概述

### 1.1 目标

在 Windows 11 上实现一个常驻桌面的 macOS 风格 Dock：

- 屏幕底部悬浮、无边框、圆角、毛玻璃质感，**与 Windows 任务栏并存**（占据任务栏上方的工作区）
- 鼠标悬停时图标按距离衰减放大（鱼眼 / magnification）
- 显示已固定应用 + 正在运行的应用，带运行状态指示点
- 点击启动 / 切换到 / 最小化应用，右键菜单可退出
- 右键菜单提供**以管理员身份运行**（Dock 自身不提权）
- 支持拖拽排序、从资源管理器拖入 .exe 固定
- 鼠标移开后自动隐藏到屏幕边缘，贴边时自动弹出
- 多显示器 + 高 DPI 正确显示
- 系统托盘、开机自启、设置界面

### 1.2 非目标（明确不做，避免范围蔓延）

| 不做的事 | 原因 |
|---|---|
| 替换 / 隐藏 Windows 任务栏 | **Q1 决策：与任务栏并存**，Dock 只占用工作区（`rcWork`） |
| Dock 自身以管理员权限运行 | **Q2 决策**：改为在右键菜单提供「以管理员身份运行」，见 §4.1.1 |
| 直接操作（关闭 / 最小化 / 激活）提权窗口 | UIPI 限制；改为 UI 降级 + 盾牌标注，见 §4.1.1 |
| 覆盖独占全屏（exclusive fullscreen）应用 | Windows 机制限制：`TOPMOST` 无法压过 D3D 独占全屏，**不可修复** |
| 支持 Windows 10 及更早 | 亚克力 / 圆角 / 现代 DWM 属性缺失，适配成本远超收益 |
| 多语言（i18n）与自动更新 | **Q4 决策：暂不做**，架构不为此预留复杂度，仅界面文案抽常量 |
| 任何形式的低层鼠标钩子（`WH_MOUSE_LL`） | 易被杀软 / 反作弊拦截，用轮询替代 |
| 精确复刻 macOS 手势与 Mission Control | 属于系统级功能，无公开 API |

### 1.3 可行性结论

**可行性 9/10。** 核心功能（悬浮窗、毛玻璃、枚举与操作窗口、悬停放大、自动隐藏）全部有成熟且公开的 Windows API 支撑。真正的成本不在"能不能做"，而在两处：

1. **Windows 缺少 macOS 的"应用（App）"抽象**，需要用 AppUserModelID + 进程路径自己拼装应用模型（§4.2）；
2. **毛玻璃路线需要实测选定**（§4.1），这是全案唯一的高不确定项。

Q2 的决策把原先最大的风险点（UIPI 权限墙 + 代码签名成本）**从架构层移除了**，方案整体风险等级由"中高"降到"中"。

---

## 2. 技术选型

### 2.1 为什么是 Tauri v2

> ⚠️ **本节在 P1 被实测修正过。** 原文用「Tauri 常驻内存 40-80 MB，Electron 150-250 MB」
> 作为主要理由，**实测证明这个数字是错的**（详见 `docs/p1-window-layer-findings.md` §6.11）。
> 真实的 Tauri 内存优势远没有那么大 —— 但**包体**与 **Rust 互操作**两条理由依然成立。

Dock 是**开机即启动、全天常驻**的程序，包体与资源占用都是硬指标。

**实测数据**（本机，Dock 只有一个 900×96 窗口）：

| 指标 | 实测 | 说明 |
|---|---|---|
| 主进程私有内存 | **8.0 MB** | 只有 Rust 那一层，非常小 |
| WebView2 子进程私有内存合计 | **183 MB**（6 个进程） | 成本几乎全在这里 |
| **私有内存合计** | **191 MB** | 可比数字（不共享，可跨进程求和） |
| 工作集合计 | 391 MB | 跨进程求和会**重复计算共享页**，偏大，不可直接对比 |
| 前端产物 | **71 kB gzip** | React 19 + 业务代码 |
| 主进程句柄数 | 384（4 分钟稳定不增长） | |

**结论修正**：

- **内存上 Tauri 与 Electron 基本是平手**，因为两者都用多进程 Chromium
  （WebView2 vs 内置 Chromium）。原方案的「40-80 MB」是把主进程内存当成了全部，**不成立**；
- **真正成立的优势是两条**：
  1. **包体** —— Tauri 复用系统 WebView2 运行时，不需要打包整个 Chromium
     （Electron 约 200 MB 包体，Tauri 约 5-10 MB）；
  2. **Rust 侧能直接调 Win32** —— 本项目 80% 的难点在 Shell 互操作
     （窗口枚举、图标提取、DWM、注册表），这一条才是决定性的。

**前端用 HTML/CSS 的好处**同样成立：图标放大、弹跳、圆角、阴影这些视觉细节，
CSS 的表达力远超任何原生 UI 框架。

> 如果将来内存成为硬约束，可考虑的方向是**减少 WebView2 进程数**
> 或用 `--single-process` 类参数（有稳定性代价），而不是换框架。

**关于 React 的体积**（Q3 决策带来的唯一代价）：React + ReactDOM 生产构建约 45 KB gzip，对 WebView2 的常驻内存影响可忽略，**不需要为此做特殊处理**。真正要避免的是引入重型 UI 组件库。若后期发现体积不可接受，可在 Vite 中一条 alias 切到 `preact/compat`（体积降到约 1/10，API 兼容），属于无风险的后备手段。

### 2.2 依赖选型

| 用途 | 方案 |
|---|---|
| 应用框架 | `tauri` v2 |
| Win32 API | `windows` crate（`Win32_UI_WindowsAndMessaging`、`Win32_Graphics_Dwm`、`Win32_UI_Shell`、`Win32_System_Com` 等） |
| 毛玻璃 | `window-vibrancy`（封装 `SetWindowCompositionAttribute`）+ 必要时手写 `DwmSetWindowAttribute` |
| 配置持久化 | `tauri-plugin-store`（JSON） |
| 开机自启 | `tauri-plugin-autostart` |
| 单实例 | `tauri-plugin-single-instance` |
| 托盘 | Tauri 内置 `TrayIcon` |
| 图标处理 | `image`（缩放 / 转 PNG）+ Shell `IShellItemImageFactory`（取图标源） |
| 前端 | TypeScript + **React 19** + Vite，**不引入 UI 组件库** |
| 状态管理 | React 内置（`useReducer` + Context）即可，图标数量级为几十，无需 Redux/Zustand |
| 打包 | `tauri build` + NSIS |

### 2.3 被否决的方案

| 方案 | 否决理由 |
|---|---|
| Electron | 常驻内存过高；Win32 互操作需 native addon |
| Rust + egui/wgpu 纯原生 | 动画控制力最强（精灵效果满分唯一路径），但圆角、阴影、渐变、字体渲染全要手写，UI 工期翻倍 |
| C# WPF / WinUI 3 | Win32 互操作与动画都优秀，但需 .NET 运行时，且 CSS 级别的视觉迭代速度仍不如 Web |

> **保留的升级路径**：若 P4 决定实现精灵形变动画，把**动画渲染层**单独换成 wgpu 子窗口（叠加在 Dock 上方），UI 其余部分不动。架构上通过"动画层可替换"来预留这个出口。

---

## 3. 系统架构

### 3.1 分层

```
┌─────────────────────────────────────────────────────────────┐
│  表现层（WebView2 / React + TS + CSS）                       │
│  · 图标布局与鱼眼放大    · 启动弹跳 / 提示气泡               │
│  · 右键菜单（含以管理员身份运行）  · 设置界面                │
│  · 运行指示点 / 盾牌角标 · 拖拽排序交互                      │
└───────────────┬─────────────────────────┬───────────────────┘
        invoke(命令) │                     │ emit(事件)
┌───────────────▼─────────────────────────▼───────────────────┐
│  应用层（Rust：commands.rs / model.rs / store.rs）           │
│  · 应用模型（用户列表 ∪ 运行态，**顺序只由用户列表决定**）    │
│  · 配置读写与迁移                   · 状态广播               │
└───────────────┬─────────────────────────┬───────────────────┘
                │                         │
┌───────────────▼───────────┐  ┌──────────▼───────────────────┐
│  平台层（Rust: win/）      │  │  窗口层（Rust: win/window.rs）│
│  · 窗口枚举 / 过滤         │  │  · Dock 主窗口                │
│  · AUMID / 进程身份        │  │  · 边缘触发条窗口             │
│  · 提权状态检测            │  │  · 玻璃 / 置顶 / 不抢焦点     │
│  · 图标提取                │  │  · 显示器几何与 DPI           │
│  · 启动（含 runas）/ 激活  │  │  · 显示 / 隐藏动画            │
│  · 缩略图捕获              │  │  · 工作区定位（rcWork）       │
└───────────────────────────┘  └──────────────────────────────┘
```

**关键原则：Win32 调用只出现在 `win/` 目录内。** 其余模块只依赖纯 Rust 数据结构，这让绝大部分业务逻辑可以单元测试，也保留了换平台的余地。

### 3.2 双窗口设计（重要）

> **P1 实现时的修正：最终采用了「单窗口」方案。**
> 原设计（本节以下内容）提出「触发条窗口 + 主窗口」两个窗口，理由是隐藏时主窗口若仍有
> 透明区域会挡住桌面点击。实现时发现：**把主窗口整个移出屏幕**同样能解决这个问题
> （移出屏幕的窗口不可能挡住任何东西），于是**不需要第二个窗口** ——
> 少一个窗口就少一套生命周期与命中测试。实测验证见
> `docs/p1-window-layer-findings.md` §6.9。以下原设计保留作为备选方案记录。

这是整个架构里最容易踩坑、也最值得先定下来的设计。

**问题**：Dock 需要自动隐藏。如果用一个窗口承载"隐藏时的走廊 + 显示时的条"，那么窗口的透明区域会挡住桌面点击。

**`WS_EX_TRANSPARENT` 解决不了**，因为它对整窗口生效，会让整个 Dock 都点不动。

**方案：拆成两个窗口**

| 窗口 | 尺寸 | 特性 | 职责 |
|---|---|---|---|
| **触发条窗口** `trigger` | 贴边 1-3 px 高 × 屏幕宽 | 不透明（或极低 alpha）、`WS_EX_NOACTIVATE`、`WS_EX_TOOLWINDOW` | 只做一件事：检测鼠标贴边 → 通知 Dock 显示。永远不作为视觉元素出现 |
| **Dock 主窗口** `dock` | 恰好等于 Dock 内容尺寸（如 900×90） | 透明、无边框、`WS_EX_NOACTIVATE`、`WS_EX_TOOLWINDOW`、**普通 Z 序（不置顶）** | 渲染 Dock 本体；隐藏时用 `set_ignore_cursor_events(true)` 变全通透 |

> **Dock 本体不置顶**（2026-09 决策，见 §4.1.2）。原设计写的是 `TOPMOST`，实现后改为普通窗口：
> Dock 只该出现在桌面上，任何被打开的应用窗口都该盖住它。

**收益**：
- 隐藏时主窗口只移动约 80 px 到屏幕下方，**不需要改变尺寸**，移动开销极小且丝滑；
- 隐藏态的主窗口可设为完全忽略鼠标事件，绝不影响桌面操作；
- 触发条极窄且不可见，即使偶发误触也无感。

**备选方案**（若不想维护两个窗口）：每帧 `SetWindowPos` 同时改位置与尺寸，靠窗口裁剪天然实现命中测试。代价是每帧触发 DWM 重排，高刷屏上可能掉帧，且亚克力背景每帧重算成本高。**不推荐，但作为降级方案记录。**

### 3.3 线程与生命周期

- **主线程**：Tauri 事件循环 + 所有窗口操作。Win32 窗口 API 必须在此线程调用。
- **Win32 常驻线程**：一个专用线程跑 60 Hz 定时器（`SetTimer` 或 `WaitableTimer`），负责：鼠标位置轮询、贴边判定、显示/隐藏动画推进。**动画绝不能依赖前端 rAF**，因为窗口自身位置要变。
- **工作线程（`tokio` task）**：窗口枚举（可耗时至数十毫秒）、图标提取与解码、缩略图抓取。结果通过 channel 回主线程，再 `emit` 给前端。
- **窗口枚举的频率**：不要轮询。用 `SetWinEventHook` 监听 `EVENT_OBJECT_CREATE` / `EVENT_OBJECT_DESTROY` / `EVENT_OBJECT_SHOW` / `EVENT_OBJECT_HIDE`（`WINEVENT_OUTOFCONTEXT`，进程外安全），事件触发后做 300 ms 防抖再重新枚举。这比轮询省电且实时。

---

## 4. 关键模块设计

### 4.1 窗口层与毛玻璃

**Tauri 窗口配置**（`tauri.conf.json`）：

```jsonc
{
  "label": "dock",
  "transparent": true,
  "decorations": false,
  "alwaysOnTop": false,   // 见 §4.1.2：Dock 本体不置顶，只该出现在桌面上
  "skipTaskbar": true,
  "resizable": false,
  "maximizable": false,
  "minimizable": false,
  "closable": false,
  "shadow": false,        // 关键：缺省的原生阴影会在透明窗口上留下矩形边框
  "focus": false,
  "visible": false
}
```

**玻璃效果：路线在 P1 被推翻并改为路线 B**（完整实测见 `docs/p1-window-layer-findings.md` §6.6）。

| 路线 | API | 裸 Win32 窗口（P0） | **真实 Tauri 窗口（P1 权威）** |
|---|---|---|---|
| A. 官方 DWM 系统背景 | `DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW` | 76 px / 对比度 92（看着可用） | ❌ **剖面完全平坦、对比度 0 —— 不采样背后内容，无模糊** |
| **B. 传统合成属性** | `SetWindowCompositionAttribute` + `ACCENT_ENABLE_ACRYLICBLURBEHIND` | 82 px / 62 | ✅ **真实模糊，约 200px 渐变** → **采用** |
| — Mica（`DWMSBT_MAINWINDOW`） | 同上 | 对比度 ≈ 1 | ❌ 不可用：Mica 模糊壁纸，不模糊窗口背后内容 |

> **⚠️ 这是本项目最重要的一次方案修正。** P0 用**裸 Win32 窗口**测得路线 A 可用（76px 模糊），
> 但在**真实 Tauri/WebView2 窗口**上复验时，路线 A 完全不产生模糊，路线 B 才产生。
> 差异来自 **WebView2 子窗口覆盖了客户端区域**。
> **教训：窗口层行为必须在真实框架窗口上定案，裸 Win32 拓扑近似不足以作为依据。**
> 生态现状也印证这一点 —— `window-vibrancy` 等 Tauri 毛玻璃实现用的都是路线 B。

**实测数据**（同一机器、同一窗口，只改路线）：

| 设置 | 亮度剖面（背后是左黑右白的锐利边缘） | 对比度 | 判定 |
|---|---|---|---|
| 无玻璃（基线） | `12 ×8 → 160 ×8` | 148 | 锐利台阶（数值与理论值精确吻合，证明量测可信） |
| 路线 A | `61` 全平坦 | 0 | ❌ 无模糊 |
| **路线 B** | `26→28→27→31→39→45→50` | 26 | ✅ 真实模糊 |

**当前实现**（2026-09 更新）：**只有路线 B 一条**。路线 A 的代码与 `DOCK_GLASS` 开关
**已全部删除** —— 它在真实 Tauri/WebView2 窗口上不产生模糊（见 §3.3 上方的实测表，
以及 `p1-window-layer-findings.md` §6.6）。毛玻璃代码从 `win_layer.rs` 抽到了独立的
`glass.rs`：`glass::apply_acrylic` 负责亚克力，`glass::apply_window_material` 负责
**圆角 / 去边框 / 深色**三项（这三项仍然必须做，且必须由 DWM 做 ——
`SetWindowRgn` 裁不掉亚克力材质）。

**着色 alpha 已调定**（2026-09 更新，取代下面原来的「待调优」）：默认 **150**，
实测标定（背后为左黑右白的锐利边缘，量 10%→90% 对比度）：

| alpha | 对比度 | 观感 |
|---|---|---|
| 220 | 25 | 几乎看不到模糊，像块死板的深色板 |
| 170 | 67 | |
| **150** | **~85** | **默认值**，兼顾「有玻璃感」与「不糊成一片」 |
| 120 | 111 | |
| 70 | 153 | 几乎全透，失去 Dock 的实体感 |

注意这层之上还有前端 `.dock` 的 `rgba(24,24,28,0.34)`，最终观感由两层共同决定，
调的时候要一起调。标定方法见 `p1-window-layer-findings.md` §6.6。

**实现要点（实测踩坑）**：

1. `SetWindowCompositionAttribute` **必须 `LoadLibrary` + `GetProcAddress` 动态取地址**——它是 `user32.dll` 的导出（序号 2391），但**不在 Windows SDK 的 `user32.lib` 导入库里**，静态链接必然 `LNK2019`（`window-vibrancy` 亦如此）；
2. 路线 B 的 `AccentFlags` 必须为 `0`（不是 `BLURBEHIND` 用的 `2`）；`GradientColor` 打包格式为 `0xAABBGGRR`（低字节是 R），且 **alpha 不能为 0**（需钳到 1）；
3. **不要使用 `WS_EX_LAYERED`**：P0 实测它会把模糊从 76 px 削弱到 66 px；
4. **`WS_EX_NOREDIRECTIONBITMAP` 窗口完全不渲染 GDI 绘制内容**（实测：用 GDI 画红色，抓屏得到背景原样）。对我们无影响（内容由 WebView2 合成）；
5. 圆角仍由自己绘制：`DWMWA_WINDOW_CORNER_PREFERENCE /*33*/ = DWMWCP_DONOTROUND /*1*/` + `DWMWA_BORDER_COLOR /*34*/ = 0xFFFFFFFE` 去边框。

若两者都不可接受，**降级方案**：半透明纯色底（如 `rgba(28,28,30,0.55)`）+ 细噪点纹理 + 内高光边 + 1px 描边，做出"看起来像毛玻璃"的**静态质感**。注意此时**不能用 `backdrop-filter` 兜底**，原因见下。

#### 为什么毛玻璃不是"前端的事"

一个常见误解：既然 UI 用 React/CSS 写，那 `backdrop-filter: blur()` 不就搞定了？

**`backdrop-filter` 模糊的对象是"页面内、该元素背后的 DOM 内容"，而不是"窗口背后桌面上的内容"。** 浏览器的渲染引擎只能看到自己这一页的像素；桌面壁纸和其它应用的窗口属于操作系统的合成器，页面**根本拿不到那些像素**。

合成栈：

```
桌面壁纸 + 其它应用窗口          ← OS 合成器（DWM）持有，网页不可见
        │
        ├─ 毛玻璃必须在【这一层】完成：只有 DWM 才采得到下面这层的内容
        │
   [ Dock 窗口 ]                ← DWM 采样窗口背后的内容做模糊，再与窗口像素混合
        │
   [ WebView2 页面 ]            ← 网页只渲染到这里；透明区域直接透出，网页无从"模糊"它
        │
   [ React 组件树 ]
```

所以在 Tauri 的透明窗口里对一个 div 写 `backdrop-filter: blur(30px)`，能模糊的只有页面内部位于它下方的元素；页面背景本身是透明的，**模糊空气等于没有模糊**。

**因此毛玻璃必须由 DWM 完成，前端无法代劳。** 它之所以是本案唯一的高不确定项，不是"不知道该调用什么 API"，而是两条可用路线各有硬伤，且：

1. **外观由系统控制**——官方路线的模糊半径、着色、饱和度都不给调，只能在其上再叠 CSS 半透明色去"染"成 macOS 的样子，属于 **OS 模糊 + CSS 染色**的混合方案；
2. **移动窗口要重算模糊**——Dock 自动隐藏时窗口每帧位移，DWM 每帧都要重新采样背景做模糊。这是合成器级 GPU 开销，**与 React 无关**，也是风险 R5 的来源；
3. **未公开 API 行为不保证**——路线 B 的 `SetWindowCompositionAttribute` 微软随时可能改，且历史上有输入延迟报告。

**分工结论**：

| 部分 | 归属 | 说明 |
|---|---|---|
| 窗口背后内容的**模糊** | **Win32 / DWM** | 唯一真难点，前端无 API 可用 |
| 圆角、着色、噪点、内高光、描边、阴影 | **React / CSS** | 这才是 CSS 主场，比原生 UI 框架强得多 |
| 图标鱼眼放大、弹跳、过渡 | **React / CSS** | 纯页面内渲染，与 OS 无关 |
| 窗口位置移动、显示/隐藏 | **Rust + Win32** | 窗口自身要动，前端动不了 |

换句话说：**玻璃的"形"是前端的事，玻璃的"透"是系统的事。** 前者 React 一天能写十版，后者才是 P0 要花两天实测的东西。

**窗口样式（Rust 侧 GetWindowLongPtr / SetWindowLongPtr）**：

| 样式 / 消息处理 | 值 | 作用 |
|---|---|---|
| `WS_EX_NOACTIVATE` | `0x08000000` | 点击 Dock 不夺取焦点（**跨进程有效**） |
| **`WM_MOUSEACTIVATE` → `MA_NOACTIVATE`** | 消息处理，返回 `3` | **必须补上**：实测 `WS_EX_NOACTIVATE` 对**同一进程**的窗口不生效 |
| `WS_EX_TOOLWINDOW` | `0x00000080` | 不出现在 Alt+Tab 列表 |
| ~~`WS_EX_TOPMOST`~~ | — | **Dock 本体不要用**（见 §4.1.2）；只有浮层窗口（菜单 / 文件夹 / 悬停标签）用 |
| ~~`WS_EX_LAYERED`~~ | — | **不要用**：会削弱毛玻璃效果（实测 76 px → 66 px） |

> **P0 实测焦点矩阵**（详见 `docs/p0-report.md` §3）：
>
> | 焦点目标 | 仅 `WS_EX_NOACTIVATE` | 仅 `MA_NOACTIVATE` | 两者都有 |
> |---|---|---|---|
> | **另一进程**（Dock 真实工况） | ✅ 不夺焦点 | ✅ | ✅ |
> | **同一进程**（右键菜单/设置窗打开时点 Dock 本体） | ❌ **仍会夺焦点** | ✅ 不夺焦点 | ✅ |
>
> **结论：两个都做**，代价为零。微软文档说 `WS_EX_NOACTIVATE` 可防点击激活——**只在跨进程时成立**，同进程窗口之间仍可互相激活。
>
> ⚠️ **对 P1 的直接影响**：Tauri/tao **没有暴露 `WM_MOUSEACTIVATE` 的处理入口**。需要 `SetWindowSubclass` 子类化 Tauri 窗口的 HWND 来补这个 handler（风险 **R15**）。**P1 的第一件事应该是在真实 Tauri 窗口上复跑 T2 探针**，先确认 tao 是否已经自行处理了该消息。

**激活他人窗口的难题**：Windows 有前台锁定（foreground lock），后台进程调用 `SetForegroundWindow` 常常静默失败。缓解手段（按可靠性排序）：

1. `ShowWindow(hwnd, SW_RESTORE)` / `SW_MINIMIZE` 优先，先改变窗口状态；
2. `AttachThreadInput(our_tid, foreground_tid, TRUE)` → `SetForegroundWindow` → 再 `AttachThreadInput(..., FALSE)`；
3. 先 `SetWindowPos(hwnd, HWND_TOP, ..., SWP_NOMOVE|SWP_NOSIZE)` 再激活；
4. 兜底：`SwitchToThisWindow`。

**这一块必须做好失败重试与降级**（例如失败时至少把窗口带到 `TOPMOST` 并闪烁任务栏），否则用户会感觉"点了没反应"，这是 Dock 最致命的体验缺陷。

#### 4.1.1 管理员权限方案（Q2 决策）

**设计原则：Dock 自身保持普通权限，只提供"以管理员身份启动"的能力。**

**启动侧（做什么）**

右键菜单提供「以管理员身份运行」：

```rust
// 关键：lpVerb = "runas" 触发 UAC 提权启动
let mut sei = SHELLEXECUTEINFOW { cbSize: size_of::<SHELLEXECUTEINFOW>() as u32, ..Default::default() };
sei.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
sei.lpVerb = w!("runas");
sei.lpFile = target_wide.as_ptr();
sei.nShow = SW_SHOWNORMAL as i32;

if ShellExecuteExW(&mut sei).is_err() {
    // 用户点了 UAC 的"否" → ERROR_CANCELLED (1223)，静默忽略，不要报错
    if GetLastError() == ERROR_CANCELLED { return Ok(LaunchOutcome::Cancelled); }
    return Err(...);
}
```

配套细节：
- 「以管理员身份运行」是**独立菜单项**，不影响普通点击启动；
- 对已在运行的普通实例，可另提供「以管理员身份重新启动」，但**必须二次确认**（先关闭再提权启动会丢失未保存数据），建议 P2 再实现，P1 只做"新开一个提权实例"；
- 若目标本身已要求提权（manifest `requireAdministrator`），普通启动就会弹 UAC，行为一致，无需特判。

**运行侧（能力边界，**必须在 P1 实测确认**）**

Dock 保持普通权限意味着它**无法操作提权窗口**（UIPI 拦截）。各操作的实际可行性：

| 操作 | 预期 | 说明 |
|---|---|---|
| 枚举窗口、读标题、取图标 | ✅ 通常可行 | 只读操作不受 UIPI 限制 |
| `PostMessage(WM_CLOSE)` / `SendMessage` | ❌ 被拦截 | 消息类操作全部被 UIPI 阻止 |
| `ShowWindow(SW_MINIMIZE)` | ⚠️ **需实测** | 非消息路径，可能成功 |
| `SetForegroundWindow` | ⚠️ **需实测** | 低完整性到高完整性通常失败 |
| `OpenProcess` + 查提权状态 | ✅ 可行 | 用 `PROCESS_QUERY_LIMITED_INFORMATION`，跨完整性级别查询是被允许的（任务管理器即如此） |

**提权状态检测**（前端据此渲染盾牌角标、决定菜单项禁用）：

```rust
let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?;
let mut token = HANDLE::default();
OpenProcessToken(h, TOKEN_QUERY, &mut token)?;
let mut elevation = TOKEN_ELEVATION::default();
let mut ret = 0u32;
GetTokenInformation(token, TokenElevation, Some(&mut elevation as *mut _ as *mut _),
                    size_of::<TOKEN_ELEVATION>() as u32, &mut ret)?;
let is_elevated = elevation.TokenIsElevated != 0;
```

**UI 降级策略（关键，决定用户是否困惑）**

检测到窗口属于提权进程时：
1. 图标右下角叠加**盾牌角标**（复用系统 `UAC` 盾牌图像）；
2. 右键菜单中「关闭窗口」「结束进程」**置灰**并附提示"需要管理员权限"；
3. 保留可用项：「以管理员身份运行」「在资源管理器中显示」「固定到 Dock」；
4. **绝不静默失败**——静默无反应比功能缺失更伤体验。

**兜底开关**：设置中提供「以管理员身份启动 Dock 自身」，**默认关闭**，并在 UI 中明确副作用：每次启动弹 UAC，且**从资源管理器拖拽文件到 Dock 会失效**（UIPI 阻止低权限 Explorer 向高权限窗口发送拖放消息）。这是 Windows 的已知行为，也是我们把提权做成"可选兜底"而非默认的原因。

#### 4.1.2 层级：Dock 在桌面，浮层在最上

**需求**：Dock 只该出现在桌面上，其他任何窗口都该盖住它；但**浮层**（右键菜单 / 大文件夹 / 悬停标签）
必须永远压在所有东西上面，否则菜单会被刚打开的应用窗口盖掉。

于是窗口分成**两档**，而不是"全都置顶"：

| 窗口 | 标签 | Z 序 | 理由 |
|---|---|---|---|
| Dock 本体 | `dock` | **普通**（`alwaysOnTop: false`） | 它是"桌面的一部分"。任何打开的应用都该盖住它 |
| 浮层（三合一） | `panel` / `menu` | **`TOPMOST`** | 它们是**用户刚要求出现的临时界面**，被盖住就是"点了没反应" |

代码里对应五处，缺一不可：

1. `tauri.conf.json` / `main.rs`：Dock 窗口 `always_on_top(false)`；
2. `win_layer::layout_dock` 的 `SetWindowPos` 带 `SWP_NOZORDER` —— 否则每次自动隐藏 / 重排都会
   把 Dock 顺手顶到最前；
3. `reveal::move_to` 同样用 `SWP_NOACTIVATE | SWP_NOZORDER` —— 自动隐藏的进出屏动画
   **不能**改 Z 序（一旦改，Dock 会在每次显示后被顶到所有窗口之上，需求就废了）；
4. `win_layer::dock_subclass_proc` 的 `WM_WINDOWPOSCHANGING` / `WM_STYLECHANGING` ——
   **外部**想把它置顶时在生效前拦掉（见下）；
5. `reveal::clear_topmost_if_set` —— 每 5 秒的兜底体检（正常情况下永不触发）。

**实测证据**（`WindowFromPoint` 取 Dock 矩形中心点的最上层窗口）：

```
1) 无遮挡时该点最上层：                  'Dock'
2) 普通窗口盖住该点后：                  'ZTEST-COVER'     ← 需求达成
3) 同一个窗口改成 TopMost 后：            'ZTEST-COVER'
4) 关掉遮盖窗口后：                      'Dock'
窗口扩展样式：Dock EX=0x8000190 TOPMOST=False / 'Dock 浮层' EX=0x8000198 TOPMOST=True
```

**代价（刻意接受）**：最大化的窗口会盖住 Dock。这是"只在主桌面上"的直接推论，不是 bug。
若哪天要改回来，正确的做法是**只对最大化窗口**特判（普通窗口仍盖住 Dock），而不是整体恢复置顶。

**启动时沉到 Z 序最底**（2026-09 用户要求：「任何时刻都在最底层，不能有任何上升层级的行为」）：

窗口刚创建时会被插到普通窗口带的**最前面**，于是"启动 Dock 之前就存在、之后一直没被激活过"
的窗口会被 Dock 压住底部一条 —— 那不符合"最底层"。所以 `setup_window_layer` 里多了一步
`win_layer::sink_to_bottom`（`SetWindowPos(HWND_BOTTOM)`）。**它的位置很关键**：

```
1.   apply_dock_ex_style               扩展样式
1.5  sink_to_bottom                    ← 必须在装钩子之前！装完之后连我们自己都改不动
2.   install_mouseactivate_hook_tree   ← 层级钩子在这里装上（拦掉此后所有 Z 序变更）
```

实测（本机 1920×1080 @125%；`EnumWindows` 下标越小越靠上）：

| 窗口 | 下标 |
|---|---|
| 各普通窗口（含最大化的浏览器窗口） | 0 ~ 126 |
| **Dock** | **132** |
| `Progman`（桌面） | 133 |

也就是**除桌面以外，没有任何可见窗口排在它后面** —— 字面意义上的"最底层"，
同时**没有掉到壁纸后面**（掉下去就看不出来了，那才是过犹不及）。
普通窗口盖上来时也在它上面：自检验证方式是**纯枚举**（"排在 Dock 后面的可见窗口"必须为空），
不需要合成点击，所以它在任何环境下都能跑（见 `DOCK_LAYERTEST`）。

**桌面跑到 Dock 上面 → 把桌面沉回去（风险已清除，2026-09 补齐）**：

"不许上升"曾经带来一个真实的两难：explorer 重启时 shell 会重建桌面窗口（`Progman` / `WorkerW`），
万一它被插到 Dock **之上**，Dock 会被壁纸整条盖住 —— 而我们**不能**把自己抬回来。
直觉修法是"再沉一次底"，但那要改 Dock 自己的 Z 序，也就得给钩子开一个
"允许自己改一次"的旁路，而**旁路本身就是外部能钻的洞**。

换个方向就没有这个问题：**修桌面那一边**。`reveal` 主循环扫 Z 序
（`win_layer::recover_if_desktop_above`），一旦发现有**可见的**桌面系窗口
（`Progman` / `WorkerW` / `SHELLDLL_DefView`）排在 Dock **之前**，就把**那些窗口**
`SetWindowPos(…, HWND_BOTTOM)` 沉到底。跨进程沉 shell 的窗口是允许的（实测返回成功、
无拒绝访问，同完整性级别）。Dock 自己的 Z 序**一次都不动** —— 不变式依旧绝对，钩子依旧零旁路。

判据为什么不看"Dock 中心点上是谁"：那个点通常被**应用窗口**盖着（应用本来就该盖住 Dock），
于是"最上层是桌面"这种状态根本观察不到 —— 可此时 Dock 已经整条被壁纸盖住了（桌面是全屏窗口）。
所以判据直接比 Z 序：只要有任何**可见**桌面系窗口排在 Dock 之前就算出事。
（只认可见的：本机有十几个隐藏的 `WorkerW` 宿主窗口 —— Wallpaper Engine 之类 ——
它们不绘制、不挡 Dock，动它们纯属惹事。）

实测（把一个隐藏的 `WorkerW` 显示出来制造故障；`EnumWindows` 下标越小越靠上）：

| 时刻 | WorkerW 下标 | Dock 下标 | 状态 |
|---|---|---|---|
| 故障前（它是隐藏的） | 34 | 91 | 正常 |
| 显示它之后**立刻** | **34** | 91 | 桌面压在 Dock 上面（Dock 被壁纸盖住）|
| 3 秒后 | **88** | 87 | 已被沉到 Dock 下方 —— **自愈** |

自检 `drive_layering_test` 里那条"桌面窗口没有压在 Dock 上面"把它钉住了
（先跑一次幂等的恢复，再断言一个不剩）。

**❗"按 Win+D，Dock 就没了"：从 2 秒轮询改到"点探测 + 三路信号"（2026-09 用户实测，已修）**：

用户报的现象是「在桌面按 `Win+D`，Dock 会隐藏」。100ms 采样实测，按下去之后：

| 时刻 | `Progman` 下标 | Dock 下标 | Dock 中心点上是谁 |
|---|---|---|---|
| 按之前 | 116 | 113 | 别的窗口（正常：应用盖住 Dock）|
| **+100ms ~ +1600ms** | **16** | 113 | **`Progman`** ← 桌面被 shell 抬到最前，Dock 整条被壁纸盖住 |
| +1700ms 起 | 116 | 112 | `Dock` ← 被 2 秒一次的轮询救回来 |

也就是说：**模式没错，时效错了** —— Dock 有整整 **1.7 秒**是"消失"的，而用户按 `Win+D`
本来就是为了看桌面，那一瞬间 Dock 不在 = 一眼就看见的 bug。

`Win+D` 抬桌面这件事**拦不住**（我们只能改自己窗口的 Z 序，而按不变式，Dock 的 Z 序一次都不许动），
所以只能"**抬起来就把它按回去**"。剩下的问题全是"**多久发现**"。这里踩了三轮，值得逐轮记下来：

| 第几版 | 触发方式 | 实测（20ms 采样，5 次 `Win+D`） | 为什么不够 |
|---|---|---|---|
| ① | 每 **2 秒**扫一次 Z 序 | 空档 **1700ms**（用户报的就是这个） | 暴露窗口 = 一个周期 |
| ② | **WinEvent 钩子 + 每帧前台轮询**，500ms 兜底 | 空档 0~260ms，平均 76~113ms，**时好时坏** | 见下：`Win+D` 时 WinEvent **没送到**、前台也没变成桌面系，实际几乎全靠 500ms 兜底 |
| ③ | **点探测**（每帧问你中心那个点是谁的）+ ②的三条 + 500ms 兜底 | **10 次全部 0ms**（20ms 分辨率量不出空档） | — |

第②版为什么不够（这一段是这轮最有价值的实测）：钩子本身没问题 —— 用一个独立进程挂同样的
两把钩子、按 `Win+D`，确实收到了 `event=0x0016`（窗口开始最小化，`CabinetWClass`）与
`event=0x0003`（前台变成 `Progman`）。但在 Dock 进程里，`DOCK_DEBUG=1` 的逐帧日志显示：
桌面已经被抬到 Dock 上面的那一刻，日志是 `守卫事件累计 0 次` —— **事件还没投递到消息泵**，
而前台也还不是桌面系（shell 是**先抬桌面、后切前台**）。于是"当帧就该发现"的设计，
实际退化成了 500ms 兜底。教训：**事件是"别人的时机"，不能拿它当唯一触发源**。

第③版的做法：给"桌面压住 Dock"这个状态本身找一个**当帧可问、且必然为真**的判据 ——
桌面是**全屏窗口**，它一旦压到 Dock 上面，Dock 矩形中心那个点就被它接管：
`GetWindowPos`→`WindowFromPoint`→`GetAncestor(GA_ROOT)`→类名比较。只看
"压在上面的是不是桌面系窗口"，所以应用窗口正常盖住 Dock（用户日常）**不会**误触发。
自动隐藏时 Dock 在屏幕外，中心点也在屏幕外，`WindowFromPoint` 返回空 → 自然为 false，
不需要任何状态耦合。实测按下 `Win+D` 之后：

```
13:35:55.976  按下 Win+D
13:35:55.998  [守卫调试] 开扫：Dock 中心点被桌面系窗口接管（62 帧）      ← 22ms 后
13:35:55.998  [守卫调试] 扫描 tick=319 连扫剩余=61 → 发现桌面在上，已沉回   ← **同一帧**
```

四条触发源最终都留着（**各自都能单独失灵，实测都失灵过**）：

| | 触发源 | 为什么留着 |
|---|---|---|
| ① | **点探测**：Dock 中心点被桌面系窗口接管 | 唯一"当帧必定成立"的一条；`Win+D` 实测 22ms 内开扫、同帧沉回 |
| ② | **前台轮询**：`GetForegroundWindow` 句柄变了且是桌面系 | 点桌面、`Win+D` 的另一半；`GetForegroundWindow` 只要几十纳秒，且只在句柄变化时才比类名 |
| ③ | **WinEvent 钩子**（`EVENT_SYSTEM_FOREGROUND` / `MINIMIZESTART` / `MINIMIZEEND`） | 送到时就是当帧，而且"开始最小化"比"桌面被抬起来"更早；装了没坏处，只是**不能只靠它** |
| ④ | **500ms 兜底轮询**（`EnumWindows` 全量扫 Z 序） | explorer 重启等"既无前台变化、点也没被接管"的极端情况 |

⚠️ 装了事件就必须**连扫若干帧**，不能扫一次就收工（`WATCH_TICKS = 62` 帧 ≈ 1 秒）：
事件（前台切换 / 开始最小化）与"桌面真的压到 Dock 上面"之间隔着几十到几百毫秒
（实测"最小化事件"比"桌面被抬起来"早约 90ms）。第一版这个数是 20 帧（320ms），
实测**不够** —— A/B 对照里"有事件钩子"反而比"只靠前台轮询"更差（最坏 260ms vs 0ms）：
事件太早到，把连扫窗口在桌面抬起之前就用完了。沉完之后还要再连扫 24 帧（≈384ms），
因为 shell 会在动画期间**反复**抬它。

**开销**（都是实测，不是估算）：

| 动作 | 实测 | 频率 | 单核占用 |
|---|---|---|---|
| 点探测（`WindowFromPoint`+`GetAncestor`+类名） | **6.15µs/次**（30 万次平均） | 每帧（62.5Hz） | **0.038%** |
| 前台比较（`GetForegroundWindow` + 句柄比较） | 纳秒级 | 每帧 | ≈0 |
| 全量 Z 序扫描（`EnumWindows` + 类名，约 150 个顶层窗口） | **25µs/次** | 兜底 500ms 一次 | 0.005% |

自检 `drive_layering_test` 新增 3 项钉住它（⑦）：事件钩子确实装上、**复现出**"桌面系窗口压在 Dock 上面"、
并且**很快**被自动沉回 —— 阈值按"点探测当时看不看得到那个点"分档：看得到就 200ms（当帧级），
看不到才 800ms（兜底级）；同时断言"沉回计数器涨了"，证明守卫真的动过手（而不是"这次抬起根本没发生"）。

⚠️ 复现用的**不是**去抬真的 `Progman`，而是自检自己造一个**类名 `WorkerW`** 的辅助窗口
（判据本来就是类名，所以走同一条代码路径）。原因是一个实测结论：**普通状态下 shell 把桌面
钉在 Z 序底部** —— `SetWindowPos(Progman, HWND_TOP)` 返回 `Ok(())` 但立刻被 shell 忽略，
只有在"显示桌面"模式下它才真的抬得起来。早先两轮闸门"通过"其实是环境凑巧（当时桌面正处在
显示状态），换个状态就变成假失败 —— 实测撞到两次才定位到。辅助窗口还必须由**主线程**创建：
守卫是从自动隐藏线程跨线程 `SetWindowPos` 沉它的，而 `SetWindowPos` 会**同步**发
`WM_WINDOWPOSCHANGING` 给窗口过程；如果那个窗口的线程正是卡在自检里等结果的后台线程，
这次沉回会一直阻塞到自检超时（也会变成"守卫没工作"的假失败）。

**顺带补上「被最小化」这条路（`SC_MINIMIZE`）**：Dock 没有任务栏按钮、没有标题栏，
一旦被"最小化所有窗口"（`Win+M`、任务栏右键）收走，**用户没有任何手段把它叫回来**
（软件还在跑、桌面上什么都没有 = 假死）。所以 `dock_subclass_proc` 里多一条：
`WM_SYSCOMMAND` 且 `wParam & 0xFFF0 == SC_MINIMIZE` → 记账后**吃掉，不往下传**
（窗口状态从来没变过，比"被最小化再还原"少一次状态抖动）。绕开 `WM_SYSCOMMAND` 的
`ShowWindow(SW_MINIMIZE)` 由 `reveal` 每帧的 `IsIconic`（纳秒级）兜底还原 ——
用 `SW_SHOWNOACTIVATE` 而不是 `SW_RESTORE`，后者会激活窗口，而 Dock 的焦点策略是"永不夺焦点"。
自检里这一条占 2 项（⑧）：给自己发一条 `SC_MINIMIZE`，断言"**没有**被最小化"且计数确实涨了。

日志（限流 5 秒一条：万一 shell 进入"抬—沉"循环，62.5Hz 的日志会瞬间刷满 8MB 的日志文件；
触发源按实际那条写，不再笼统写成"兜底轮询"，第一版这里说过假话）：

```
[层级] 桌面层被抬到了 Dock 上方（Win+D/显示桌面、点桌面、explorer 重启都会这样）
       —— 已把桌面沉回 Dock 下方（点探测：Dock 中心点被桌面系窗口接管）
```

**❗`WS_EX_TOPMOST` 是"粘"的：在消息里拦掉，而不是定期纠正**（2026-09 用户实测）：

Dock 自己不设这个位，但它是**窗口属性**，任何外部程序都能用
`SetWindowPos(HWND_TOPMOST)` 或 `SetWindowLongPtr(GWL_EXSTYLE, … | WS_EX_TOPMOST)` 给它设上
—— 自动化脚本、录屏、游戏加速器、"窗口总在最前"小工具都会这么干。而一旦设上就**不会自己掉**：
它会浮在所有普通窗口之上，连全屏游戏都盖住，用户看到的就是
"我打开原神，Dock 却出现在游戏画面上面"（这正是用户报的那个 bug）。

实测来源：自检脚本为了让合成点击能落到 Dock 上（它刻意不置顶，会被最大化的窗口盖住），
临时把它顶到最前；收尾的还原和后台守护的再次设置撞在一起，位就留下来了。

**做法：`win_layer::dock_subclass_proc` 里两条消息**（那个子类窗口过程本来就为
`WM_MOUSEACTIVATE` 挂着，加两条分支而已）：

| 路线 | 消息 | 处理 |
|---|---|---|
| `SetWindowPos(…, HWND_TOPMOST, …)` | `WM_WINDOWPOSCHANGING` | 给 `WINDOWPOS.flags` 补 `SWP_NOZORDER` —— 这次 Z 序变更**从来没有发生过** |
| `SetWindowLongPtr(GWL_EXSTYLE, …)` | `WM_STYLECHANGING` | 把 `styleNew` 里的 `WS_EX_TOPMOST` 位抹掉 |

两条都在**改动生效之前**拦（文档明确允许在 `…CHANGING` 里改参数），而且**跨进程也会发过来**
（`SetWindowPos` 是同步调进目标窗口的过程的）。实测：外部置顶后 **40ms 都不到就已确认没生效**
（`ex=0x8000190` 从头到尾没变），自检 `drive_layering_test` 当时 6 项全过（该段 2026-09 已扩到 14 项）—— 关键那条断言的是
"**当场**就没生效"，而不是"过一会儿被纠正"。

**为什么不用轮询**（第一版就是轮询，已改）：轮询有最长一个周期的**暴露窗口** —— 外部设上之后、
下一次检查之前，Dock 已经真的浮在游戏上面了，那一眼就够用户看见。而且：

- `GetWindowLongPtrW` 实测 **8.5 ns/次**（同进程 / 跨进程一样快，user32 在客户端就把
  `GWL_EXSTYLE` 答了）——所以轮询在**性能**上其实完全不是问题，**问题在延迟**；
- 消息拦截的成本≈0（只在真有人改 Z 序时才走），延迟=0，强度也更高（变更根本不发生）。

`reveal::clear_topmost_if_set` 保留为**兜底**，但降到每 5 秒一次（312 帧）：万一还有既不发这两条
消息、又能把窗口设成置顶的野路子（直接操作窗口带 / 内核对象之类），5 秒内纠正，并且日志会
用 `⚠️ 兜底体检发现…` 明确标出"主防线有洞"。正常情况下它永远不该触发。

⚠️ 两个反直觉的实现细节（都踩过，别重犯）：
1. **`HWND_TOPMOST` 在消息里已经被 win32k 解析成具体句柄了**（实测传 `-1` 进去，收到的
   `hwndInsertAfter` 是 `0x610464` 这样的真实 HWND），所以**不能**拿常量去比对——
   因此日志只报"挡下 Z 序变更 N 次"（其中包含 tao 自己发的 `NOTOPMOST`，`always_on_top(false)`
   就会发），不谎称都是"攻击"；能说死的只有 `WM_STYLECHANGING` 那条（新样式里确实带着该位）；
2. **日志别写在消息处理里**：那是别的进程**同步**调进来的调用栈（`SetWindowPos` 会等我们返回），
   在他人的调用栈里写文件既不礼貌也没必要。钩子只 `fetch_add` 一个原子计数，
   由 `reveal` 主循环发现计数变化后打印。

⚠️ 连带影响（跑自检时会撞上）：**Dock 从此不接受任何 Z 序变更**，所以
"用 `SetWindowPos(HWND_TOPMOST)` 把 Dock 临时顶起来跑合成点击"这招**失效了**（这是特意的）。
自检要求 Dock 真的可见 —— 被最大化窗口盖住时，请 `Win+D` / 最小化那个窗口，
而不是想办法把 Dock 顶上来。

⚠️ 排查这类问题时的另两个陷阱：
1. **别用没声明 DPI 感知的进程去量屏幕**：本机 1920×1080 @125%，
   未声明 DPI 感知的 `GetSystemMetrics` / `SPI_GETWORKAREA` 会返回**虚拟化的**
   1536×864 —— 看起来像"分辨率被游戏改了"，其实什么都没变（`SetProcessDPIAware()` 后才是真值）；
2. **Dock 进程消失 ≠ 崩溃**：托盘「退出」是正常退出，事件日志里不会有任何记录。
   要区分就得看它的 stdout/stderr —— 用 `Start-Process -RedirectStandardOutput .dock.log` 启动，
   真 panic 会留在那里（`DOCK_DEBUG=1` 还能拿到完整枚举日志）。

#### 4.1.3 任务栏透明：查清了，**不做**（需要注入 explorer）

用户要过「让任务栏背景透明、壁纸透出来」（参考 TranslucentTB）。
**结论：在 Windows 11 上做不到"不注入"**，本项目不做 —— 这里记下全部证据，免得以后重走一遍。

**曾实现过、已撤掉的错误做法**：往 `Shell_TrayWnd` 敷 `ACCENT_ENABLE_ACRYLICBLURBEHIND`
（和 Dock 自己同一条 API）。实测它**只能让任务栏更实**：

| 施加的东西 | 与系统默认的像素差 | 方向 |
|---|---|---|
| 透明渐变（state 2）alpha=0 | 5.6 | ≈ 无变化 |
| 亚克力 alpha=1 | 6.3 | ≈ 无变化 |
| 亚克力 alpha=120 | **17.5** | 变实 |
| 模糊（state 3）alpha=0 | **30.0** | 变实 |
| 透明渐变 alpha=255 | **35.3** | 变成一块黑板 |

**为什么**：Win11 的任务栏背景是 **XAML 画的**。本机 build 26200 的窗口结构：

```
Shell_TrayWnd  (ex=0x88 TOOLWINDOW|TOPMOST)
  ├─ 'Windows.UI.Composition.DesktopWindowContentBridge'  (0,1020)-(1920,1080)  ← XAML 岛，铺满整条
  ├─ 'Windows.UI.Input.InputSite.WindowClass'
  ├─ 'Windows.UI.Core.CoreWindow'
  ├─ 'Start' / 'TrayNotifyWnd' / 'ReBarWindow32' / 'MSTaskSwWClass'
```

`SetWindowCompositionAttribute` 改的是**窗口自己的背板**，而背板之上还盖着一层不透明的 XAML
内容 → 只能往上加色，改不掉它。**DWM 背板那条路也排除了**：`DWMWA_SYSTEMBACKDROP_TYPE`
当前就是 0（`DWMSBT_AUTO`），设成 `DWMSBT_NONE`/Mica/Acrylic 后截屏**逐像素无变化**。
（`WorkerW` 那条老路也没有：本机任务栏没有 `WorkerW` 子窗口。）

**TranslucentTB 在 Win11 上是怎么做的**（读它的源码，`ExplorerTAP/`）：

```
SetWindowsHookEx(WH_CALLWNDPROC, proc, 自己的DLL, explorer 的线程 id)   ← 把 DLL 塞进 explorer
  → 在 explorer 里 InitializeXamlDiagnosticsEx(...)                    ← Windows.UI.Xaml.dll 的未公开导出
  → 拿到 IXamlDiagnostics，遍历 XAML 可视树，找到任务栏背景元素
  → 把它的 Background 画笔换成透明 / 亚克力（并自带一个 XamlBlurBrush）
```

配套：COM proxy/stub、Detours payload 握手、vcpkg/C++WinRT 工具链，`ExplorerTAP/` 下
10 个文件约 25KB C++，并且它自己的代码里就有「Windows 更新后要求重启 Explorer」的提示
（`IDS_RESTART_REQUIRED`）。**它的许可是 GPL** —— 抄进来会让本项目的许可跟着变。

**所以**：这件事交给 TranslucentTB / Windhawk 这类专门做注入的工具去做，Dock 保持
「不注入任何进程」（与风险登记册 R10 一致）。要做也不是不行，但那是一个独立的
C++ 注入模块（新工具链 + 未公开 API + 每次 Windows 更新都可能失效），
不该混在"一个 Dock"里。

#### 4.1.4 悬停标签：不是原生 tooltip

**问题**：早期用 HTML `title` 属性做悬停名字提示。`title` 由 **WebView2 宿主**画成一个原生
tooltip 窗口 —— 字体、圆角、阴影全是 Windows 的，位置固定在光标右下角，和 Dock 的 macOS 观感
完全不是一套。**不能用**。

**方案**：把标签当成**第三种浮层内容**（`PanelContent::Label`），复用浮层窗口那一套
（菜单 / 文件夹 / 标签三形态共用一个页面与状态机）：

| | 原生 `title` | **浮层标签**（现方案） |
|---|---|---|
| 外观 | Windows 原生，不可定制 | 页面自己画（和菜单同材质、同圆角） |
| 位置 | 光标右下角（可能挡住图标） | **图标正上方居中** |
| 出现时机 | 浏览器自己定（约 1s，不可控） | 120 ms 延迟，且只在没进过这个图标时重新计时 |
| 窗口 | 由 WebView2 内部管理 | `WS_EX_NOACTIVATE`，不抢焦点、不进 Alt+Tab |

前端对应四件事：`Dock.tsx` 里把 `title` 换成 `aria-label`（可访问性不能一起丢掉），
`onMouseEnter` 用 `e.currentTarget.getBoundingClientRect()` 的**中心**当锚点（不是 `clientX` ——
光标可以从图标边缘进入，那样标签会被推到旁边），`App.tsx` 用 `hoverRef` 去重（进出同一个图标不重复弹），
拖拽中不弹（`dragRef` 非空直接跳过 —— 指针在"搬东西"时标签跟着跑只是噪音）。
大文件夹里的格子走同一条路（`Panel.tsx` 的 `hoverItem`），只是 `inFolder: true`。

`place()` 的**基准窗口是可换的**：Dock 图标贴 Dock，文件夹格子贴**文件夹面板**
（`base` 参数）—— 否则一个屏幕底部的气泡会跑去指面板里的小格子。
规则不变：底边贴在 `base.top - GAP_ABOVE_DOCK`，水平以锚点为中心并钳进工作区。

**实测（产品构建，1013×70 的 Dock，逐图标扫一遍）**：

```
Dock (453,950)-(1466,1020)
x=500 → 气泡 (471,911)-(545,940)   74x29
x=620 → 气泡 (555,911)-(694,940)  139x29   「文件资源管理器」
x=1400 → 气泡 (1315,911)-(1510,940) 195x29
x=900 / x=1200（分割线与间隙上）→ 隐藏（标签不给分割线弹）
光标移开 → 隐藏
```

尺寸是**按文字量的**（`text_width`），宽度随名字变；底边恒为 940 = Dock 顶 950 − 8 逻辑像素。

**底色单独提亮**（`LABEL_BRIGHTEN = 0.62`）：菜单/文件夹沿用 Dock 的深色着色是对的
（那里有图标和整行高亮），但标签只有一行字、尺寸又小，深色底 + 白字渲染出来是
"中灰玻璃压白字"（实测底色亮度 ~147，对比不到 2:1）。标签改用
`brighten(glass_rgb, 0.62)` = `[24,24,28] → [167,167,170]`，于是 `is_dark(tint)`
自动翻面 → 页面用深色文字，对比度升到 8 以上。**页面不判断深浅，只认 payload 里的 `dark`**，
所以"提亮 → 自动换文字色"是一处改动、一处真相。实测底色亮度 147 → **191**（9 个图标一致）。

---

### 4.2 应用模型层（Shell 集成）

这是 Windows 上最脏、也最决定成败的部分。

#### 应用顺序模型（**先读这一节**，它决定其他一切）

> 2026-09 修正。早期设计是「固定项 ∪ 运行项」两段式（`[固定项…][未固定的运行中应用…]`），
> **那是错的**，已废弃。

**规则只有一条：Dock 上显示哪些图标、按什么顺序，完全由用户的列表决定。**

- 用户列表 = `Preferences.pinned`（名字是历史遗留，语义就是「Dock 上的图标列表」）。
  出厂由 `store::DEFAULT_PINS` 预置几个作为初值，之后全靠用户：拖放 / 「添加应用…」/ 右键移除。
- **运行状态不参与决定位置，也不决定是否出现。** 一个应用没在跑，图标仍待在原处，
  只是没有运行指示点。运行只影响两件事：**白点**，以及**点击是「启动」还是「切换」**。
- 合并逻辑见 `apps.rs::enumerate_dock_apps()` —— 一趟遍历用户列表，
  在运行集合里按 id 找得到就取运行态（有窗口句柄），找不到就给占位项（点击去启动）。

**为什么废弃两段式**（两个后果都与上面的规则冲突）：

1. 你没添加过的程序**一跑就自己冒出来、退出又消失** —— 位置不是用户决定的；
2. 同一个程序在「运行中」和「未运行」两种状态下**可能落在不同位置**。

顺带消掉的麻烦：`EnumWindows` 的 Z 序导致图标乱跳、需要持久化「首次出现顺序」
（`discovered` 字段，已删）、状态指纹要跟着改。要改顺序，**只动 `pinned` 的顺序**。

#### 分割线：列表里的普通条目

Dock 上可以放**分割线**（纯视觉分组），它没有独立的存储结构 —— `pinned` 里就是一个
`separator: true` 的条目（`display_name` / `target` 都为空）。

这样做的理由：用户的模型是「这一条是分组线，位置我自己定」。如果分割线另存一份
「第几项之后」的坐标，就会出现两份真相，拖拽/删除时必然对不上。放进同一个列表之后，
分割线**自动**获得和图标完全一样的能力：能拖动排序、能拖出 Dock 删除、能被右键移除。

- 渲染：`Dock.tsx` 里复用同一个 `<button>`（行为一致），只换内容 —— 画一条竖线。
- 宽度：`SEP_W = 9`（线 1px + 两侧可点中的空白），**可点中**是硬要求（要能右键/拖走）。
- 间距：分割线两侧用 `SEP_GAP = 4`，比图标之间的 `gap = 10` 更紧。
  1px 的线套用图标间距的话，两侧会各留 `10 + SEP_W/2 ≈ 14.5px`，
  看起来是"浮在一大块空地中间"（用户反馈：两侧太空）。
  实现上 `computeLayout` 的间距参数从标量扩成**逐对间距数组** `gaps`，
  规则（谁的旁边是分割线）留在 `Dock.tsx`，几何仍归 `magnify.ts`。
- 放大：分割线**不参与鱼眼放大**（`computeLayout` 的 `fixed` 参数）。它仍然参与排布，
  所以邻居该让位照常让位；只是自己不变大 —— 一条竖线放大 1.25 倍只会变粗，不会"变大"。
- 颜色：**浅色半透明细线，没有描边**（`rgba(255,255,255,.42)`）。
  ⚠️ 颜色必须按**渲染后的底色**选，而那个值是动态的（玻璃是半透明的，底色由桌面内容决定）。
  两次失败记在 `backlog.md` BL-5，结论是：跟运行白点用同一套办法，用浅色。
- id：`sep-<毫秒时间戳>-<序号>`，天然不与应用 id 冲突（应用 id 是 exe 全路径小写
  或 `uwp:<AUMID>`）。

#### 系统位置：此电脑 / 回收站

Dock 上也能放**系统位置**。它们和分割线一样，是 `pinned` 里的普通条目，
没有自己的数据结构；区别是它们**能打开**。

关键在于：这些不是 exe，而是 **Shell 命名空间项**（`shell:MyComputerFolder` /
`shell:RecycleBinFolder`）。要能放进 Dock，三条路都得通 —— 三条都已实测：

| 需要 | 走哪条路 | 实测 |
|---|---|---|
| 显示名 | `IShellItem::GetDisplayName(SIGDN_NORMALDISPLAY)` | 中文系统 →「此电脑」「回收站」（**不写死文案**，系统名字由系统给） |
| 图标 | `IShellItemImageFactory::GetImage` —— 它认 shell 路径 | 128×128，透明像素 41~42%（是真图标，不是一块底板） |
| 打开 | `ShellExecuteEx(lpFile="shell:MyComputerFolder", lpVerb="open")` | 真的开出「此电脑 - 文件资源管理器」窗口 |

所以 `apps::launch` **不需要**为它们加分支 —— `ShellExecuteEx` 的 `lpFile`
本来就吃 `shell:` 路径，「打开此电脑」和「启动 Edge」是同一条代码路径。

⚠️ 一个必须区分的坑：`shell:AppsFolder\<AUMID>`（打包应用）**也**以 `shell:` 开头，
但它是**应用**不是位置。判据是 `apps::is_shell_location()`：

| | 打包应用 `shell:AppsFolder\…` | 系统位置 `shell:MyComputerFolder` |
|---|---|---|
| 右键第一项 | 「启动」 | 「**打开**」 |
| 以管理员身份运行 | 保留 | **置灰**（没有"以管理员身份打开此电脑"这回事） |
| 打开文件所在位置 | 置灰（没有文件位置） | 置灰 |

入口在**托盘 →「添加系统位置」**（子菜单由 `apps::SYSTEM_LOCATIONS` 表驱动，
加一个新位置只要往表里加一行）。加进去时落在**最左侧**，之后就是普通条目 ——
可以拖动、可以拖出删除、可以右键移除。「最左」只是加入时的落点，
不是被固定住的区域（与「位置完全由用户决定」这条模型一致）。

#### 临时文件夹：一个**真实目录**，不是命名空间项

Dock 最左侧可以放一个「临时文件夹」（托盘 →「添加临时文件夹」，落在第 0 位）。
它和系统位置**不是一类东西**，实现上也刻意分开：

| | 系统位置（此电脑 / 回收站） | 临时文件夹 |
|---|---|---|
| target | `shell:MyComputerFolder`（Shell 命名空间项） | 一个**真实路径**：`%LOCALAPPDATA%\dev.local.dock\临时文件` |
| 名字 | 问 Shell 要（系统语言） | 我们给的「临时文件夹」 |
| 图标 | Shell 给命名空间项的图标 | Shell 给**目录**的图标 |
| 右键第一项 | 「打开」 | 「打开」（同一个判据，见下） |

**为什么用真实目录**：它就是给用户放临时文件用的 —— 点开要在资源管理器里能直接拖东西进去。
放 `LocalAppData` 而不是"文档"/`%TEMP%`：前者会往用户的资料目录里塞东西，
后者是给**程序**用的（系统随手清、塞满别人的垃圾）。

菜单上的三处细节（都有单测）：
1. **说「打开」不说「启动」** —— 判据是"target 是不是目录"，由 `commands::menu_target`
   量好放进 `Target.is_dir`（`menu::build` 因此保持纯函数，单测不用碰文件系统）；
2. **「以管理员身份运行」置灰** —— 没有"以管理员身份打开一个文件夹"这回事；
3. 「打开文件所在位置」**保留** —— 它是真实路径，定位到它有实际意义。

#### 命名输入框：为什么必须另开一个能拿焦点的窗口

「新建文件夹…」和「重命名…」都要用户敲字，而**浮层窗口是 `WS_EX_NOACTIVATE`** ——
那是"点 Dock 不夺走当前应用焦点"这条核心特性的前提，代价就是**它们打不了字**
（没有焦点就没有键盘输入）。设置窗口能拿焦点，但为了给文件夹起个名去开设置页太绕。

所以有 `prompt_window.rs` + `src/prompt/`（`prompt.html`）：一个**自绘**的小窗口，
材质和浮层完全一致（`glass::apply_acrylic`，圆角仍交给 DWM）。四条规矩：

- **明确抢一次前台 + 把光标放进输入框并全选**（`SetForegroundWindow` + 页面 `focus()`/`select()`）——
  否则用户得先点一下才能打字；
- **同一时刻只允许一个**（`CUR` 里只有一个 `Open`，seq 对不上就丢弃回调）；
- **不能占主线程**：这个框对 Rust 侧是模态的（工作线程 `rx.recv_timeout` 等结果），
  而调用方在 Tauri 事件循环上 —— 和工作线程上的文件对话框同一个套路
  （`store::spawn_create_folder` / `spawn_rename_folder`），顺便 `reveal::pause()`，
  免得用户打字时 Dock 自己收下去；
- **窗口尺寸必须等于内容高度**（`PAD_X/W/H` 与 `prompt.css` 同一组数字）：
  亚克力铺满**整个窗口矩形**，所以窗口不能比内容大一块（会露出一圈更亮的底），
  也不能小（`overflow: hidden` 直接把按钮裁掉）。

**为什么把第一版的原生 Win32 输入框换掉**：用户一句"太丑，而且内容还显示不全"。
两个都是真问题 —— 系统控件在这个应用里本来就是异类（Dock / 浮层 / 设置页全是同一套玻璃），
而"显示不全"是我把 `CreateWindowExW` 的 `nWidth/nHeight` 当成了**客户区**尺寸
（它含标题栏和边框），于是按客户区排的版比实际能画的地方高，底部按钮被裁掉。
自绘之后排版和窗口尺寸由同一组常量算，这类"两处数字不一致"的错就没地方藏了。

自检里也**真的驱动它**：等窗口可见 → 用 `webview.eval` 把值写进输入框（React 受控组件，
必须走原生 setter + `input` 事件）→ 点「确定」→ 断言配置里的名字就是敲进去的那个
（只验"窗口出现了"是没用的，名字有没有落盘才是功能本身）。
⚠️ 别用合成键盘事件驱动它：中文输入法会把按键吞成候选词，自检会假失败。

#### `.url` 快捷方式：Steam / Epic 的游戏

游戏库里的游戏没有"一个 exe 路径"可以填 —— Steam 的启动方式是
`steam://rungameid/<appid>`，而 Steam 的「添加桌面快捷方式」生成的**不是 `.lnk`，
是 `.url`（Internet 快捷方式）**：

```ini
[InternetShortcut]
URL=steam://rungameid/431960
IconFile=D:\Steam\steamapps\common\wallpaper_engine\launcher.exe
IconIndex=0
```

**target 保留快捷方式文件路径本身**，不把里面的 `URL=` 抠出来当 target。三条理由：

1. **图标**：`IconFile=` 指着游戏自己的图标，走文件路径才取得到。
   裸的 `steam://…` 在 Shell 里**取不到任何图标**（实测），
   用户会看到一个首字母占位块；
2. **启动**：`ShellExecuteEx` 打开 `.url` 本来就会执行里面的 URL ——
   协议解析交给系统，我们不用自己维护一张「协议表」（Steam / Epic / Battle.net 各一套）；
3. **「打开文件所在位置」仍然成立**（能定位到这个快捷方式）。

两个必须自己处理的差异：

| | 通用「Internet 快捷方式」图标 | 我们要的 |
|---|---|---|
| 图标 | `SHGetFileInfo` 对 `.url` 返回**蓝地球+箭头**（还叠一个快捷方式小箭头） | 走 `IconFile=`，见 `apps::url_icon_source` |
| 提权 | `runas` 提权的是**协议处理器**（Steam），不是游戏 | 菜单里置灰 |

`IconFile` 的读取用 `GetPrivateProfileStringW` 而不是自己 `read_to_string`：
`.url` 就是 INI，这个 API 自己处理 ANSI / UTF-16 / BOM ——
自己读的话遇到 GBK 写的 `.url`（中文游戏名、中文路径）会直接失败。
路径里的 `%WINDIR%` 这类变量我们自己也展开一次（`apps::expand_env`）。

**实测**（用户机：Steam 在 `D:\Steam`，游戏 Wallpaper Engine / appid 431960）：
`extract_icon(.url)` 与 `extract_icon(launcher.exe)` 的位图哈希**完全相同**
（`C12E1A441BB7C5E5`），截屏确认 Dock 上出现的是 WE 的启动器图标、
**没有**快捷方式小箭头，悬停名字是「Wallpaper」（= 快捷方式的名字）。

> `IShellItemImageFactory` 这条路对 `.url` 其实**不会**叠小箭头（`SHGetFileInfo` 会），
> 也就是说本机上两条路给出的位图一样。保留 `IconFile` 优先是为了不依赖 Shell
> 对 `.url` 的处理策略 —— 它随关联状态变，而 `IconFile` 是快捷方式自己写下的意图。

**不支持的东西（刻意的）**：游戏**没有白点、也不能"切到前台"** ——
Dock 靠"进程 exe 路径 = target"匹配运行态，而 `.url` 不是进程。
要做的话得从 appid 反查 Steam 的 `appmanifest_<id>.acf` → `installdir` →
再拿进程路径做前缀匹配，属于启发式（见 `backlog.md`）。


**核心问题**：macOS 的 Dock 以 App 为单位，Windows 只有三样东西：进程（PID）、窗口（HWND）、AppUserModelID（AUMID）。需要自己合成"应用"概念。

**身份解析优先级**：

```
1. 窗口的 AUMID
   SHGetPropertyStoreForWindow(hwnd, IID_IPropertyStore, &ps)
   ps->GetValue(PKEY_AppUserModel_ID /* {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5 */, &pv)
   → 对 Store/UWP 应用与正确注册的程序有效，最稳定

2. 进程可执行文件路径
   GetWindowThreadProcessId → OpenProcess → QueryFullProcessImageNameW
   → 回退方案，对绝大多数 Win32 程序有效

3. 窗口类名 / 标题
   → 最后兜底，仅用于无法识别的窗口
```

**运行窗口枚举**：

```
EnumWindows(callback)
  ├─ 跳过 !IsWindowVisible
  ├─ 跳过 WS_EX_TOOLWINDOW（工具窗口、托盘气泡）
  ├─ 跳过 GetWindow(hwnd, GW_OWNER) != NULL 且无 WS_EX_APPWINDOW 的窗口（对话框）
  ├─ 跳过 DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED /*14*/) 为真的窗口
  │    ← 关键：UWP 应用被挂起时窗口仍存在但已"cloaked"，
  │      不检查会看到一堆幽灵窗口
  └─ 按上述身份优先级归类到 App
```

**多窗口合并策略**（对齐 macOS）：同一 App 的多个窗口在 Dock 上显示为一个图标；点击时：
- 该 App 无窗口在前台 → 激活其最近使用的窗口；
- 该 App 已在前台 → 在它的窗口间循环切换；
- 悬停时弹出缩略图预览条（P3 可选）。

**图标提取**（决定"清晰度质感"，务必用对 API）：

| API | 分辨率 | 评价 |
|---|---|---|
| `ExtractIconEx` | 16/32 px | **不要用**，在高 DPI 下必糊 |
| `SHGetFileInfo` | 32 px（`SHGFI_LARGEICON`） | 同样偏小 |
| **`IShellItemImageFactory::GetImage`** | 可请求 256×256 | **正确做法**，`SIIGBF_ICONONLY`（`0x4`）纯图标 / `SIIGBF_THUMBNAILONLY`（`0x8`）缩略图 / `SIIGBF_BIGGERSIZEOK`（`0x1`） |

流程：exe 路径 → `SHCreateItemFromParsingName` → `QueryInterface(IShellItemImageFactory)` → `GetImage({256,256}, SIIGBF_ICONONLY|SIIGBF_BIGGERSIZEOK)` → HBITMAP → 转 PNG 落盘缓存。

**图标缓存**：`%APPDATA%\Dock\icons\<sha1(身份)>.png`，附带 `mtime + size` 做失效校验（exe 更新后图标要变）。前端通过 Tauri `asset:` 协议读取，避免 base64 撑爆 IPC。

**启动应用**：

| 类型 | 方式 |
|---|---|
| Win32 exe | `ShellExecuteExW`，带工作目录 |
| **以管理员身份运行** | `ShellExecuteExW` + `lpVerb = L"runas"`（触发 UAC，见 §4.1.1） |
| Store / UWP | `explorer.exe shell:AppsFolder\<AUMID>` |
| 快捷方式 | 解析 `.lnk` 后按上述处理 |
| URL / 协议 | `ShellExecuteExW` 直接传 URI |

**"添加应用"选择器的实用技巧**：枚举**已安装**应用不要自己爬注册表。直接调用

```
Get-StartApps   →  返回 { Name, AppID }  列表
```

一次拿到开始菜单里所有应用（含 UWP，AppID 即 AUMID），用 `child_process` 或直接 `PowerShell` 调用即可，几十毫秒。这是性价比最高的方案。

**窗口操作**：

| 操作 | API |
|---|---|
| 最小化 | `ShowWindow(hwnd, SW_MINIMIZE)` 或 `PostMessage(hwnd, WM_SYSCOMMAND, SC_MINIMIZE, 0)` |
| 关闭 | `PostMessage(hwnd, WM_CLOSE, 0, 0)` |
| 恢复 | `ShowWindow(hwnd, SW_RESTORE)` |
| 强制结束 | `TerminateProcess`（需二次确认，风险高） |
| 前台激活 | 见 §4.1 的激活链路 |

> 上表中所有**写入类**操作对提权窗口均会失败，按 §4.1.1 的降级策略处理。

#### 4.2.1 首次运行预置：全部现查（开源可移植性）

**背景**：这个项目是从"给一台具体机器做的 Dock"长出来的，因此早期版本里有一类隐患 ——
**照着自己机器写死的常量**：`store.rs` 的预置表里写着 Edge 的完整安装路径、
在本机用 `Get-StartApps` 抄下来的 AUMID、以及"终端/记事本/计算器"这些中文显示名。
在开发机上一切正常，换台机器就会：路径不存在（**点不开的空图标**）、英文系统上名字是中文、
没装的应用照样冒出来。要放到 GitHub 上给别人用，这些必须全部换成**探测**。

**规则表 [`DEFAULT_PIN_RULES`](../app/src-tauri/src/store.rs) 只留三类与机器无关的线索**：

| 规则 | 线索 | 换机器为什么仍然成立 |
|---|---|---|
| `SystemExe` | 相对 `%SystemRoot%` | 所有 Windows 都有；**系统盘不一定是 C:** |
| `ShellLocation` | `shell:MyComputerFolder` 这类命名空间 | 与安装位置无关，名字由 Shell 给（跟随系统语言） |
| `Program` | `App Paths` 里的 **exe 文件名** | 微软安装规范要求所有 GUI 程序登记它，值与安装位置无关 |
| `Package` | 打包应用的**包族名前缀** | 发布者哈希由发布者决定（如 `…_8wekyb3d8bbwe`），**所有机器相同** |

四条实现要点（每条对应一个实测踩过的坑）：

1. **`App Paths` 要查四组**：`HKCU`/`HKLM` × 64 位视图/`WOW6432Node`。
   32 位程序（Chrome 常见）只登记在 `WOW6432Node`，64 位进程只读 `SOFTWARE\…` 是读不到的；
2. **显示名分两条路**：`shell:` 目标用 **Shell 显示名**，普通 exe 用**文件版本信息**
   （`display_name_for`）。反过来会得到 `msedge.exe` 这种名字 —— 对 exe 调 Shell 显示名
   只会把**文件名**还给你（实测）；
3. **`WindowsApps\` 下的路径一律不预置**：那个目录的 ACL 只允许经 `AppsFolder` 激活，
   直接 `ShellExecute` 会被拒绝（图标画得出来、点了没反应）。顺带消掉了
   "`wt.exe` 与打包版终端重复出现"——`App Paths` 把 `wt.exe` 解析到的正是 WindowsApps 里的真实路径；
4. **存在性一律现验**（`app_paths_lookup` 内置文件检查、`target_exists` 覆盖 shell/打包应用），
   拿不到就跳过 —— 所以没装的应用**不会**留下空图标。

**单测钉住**：`store::tests::default_pin_rules_are_machine_independent` 断言
"规则里不许出现具体安装路径 / 用户目录 / Program Files"、"系统位置与 `explorer.exe` 必然可解析"、
"exe 的显示名不能退化成文件名"、"WindowsApps 路径必须被判为不可预置"。
（它还抓到了 `notepad.exe` 在 Win11 上已是商店 stub、版本信息不保证存在的问题，
于是样本换成了 `explorer.exe`。）

**实测（全新配置目录，模拟别人第一次装）**：

```
此电脑 · 回收站 · Windows 资源管理器 │ Microsoft Edge · Google Chrome · 终端 · 设置
```

名字跟随系统语言（中文系统是"Windows 资源管理器"，英文系统会是 "Windows Explorer"），
路径来自 `App Paths`（Edge 在 `C:\Program Files (x86)\…`、Chrome 在 `C:\Program Files\Google\…`
—— 两者都是**查出来的**，不是写死的）。已配置过的用户完全不受影响：预置只在
`config.json` 不存在时跑一次。

---

### 4.3 交互层

**鱼眼放大（magnification）** —— 纯数学，无需 Win32。

对每个图标，取光标 x 与其中心的水平距离 `d`：

```
scale(d) = 1 + A · max(0, cos(π · d / (2R)))²     当 d < R，否则 1
```

其中 `R` 为影响半径（约 `4 × 图标宽`），`A` 为最大放大倍率（约 `0.6-0.8`）。

**横向位移同步**：放大后图标会互相重叠，必须同时重算水平位置。做法是对缩放增量做前缀和累积位移：

```
offset_i = Σ_{j<i} (w_j·(scale_j − 1)) / 2
```

`transform-origin: bottom center` 保证图标从底边向上长。整套逻辑约 50 行 TS，20 个图标在 rAF 里跑毫无压力，WebView2 能稳定 60 fps。

**React 实现要点**（避免经典性能陷阱）：

- **不要把鼠标位置放进 React state**——每秒 60 次的 `setState` 会触发整棵组件树重渲染。正确做法：`mousemove` 里直接写 DOM（`el.style.transform = ...`），用 `useRef` 持有节点引用，**完全绕过 React 渲染**。React 只负责渲染图标的存在与顺序。
- 图标列表变化时用稳定的 `key`（应用 `id`），依赖 React 的 diff 复用 DOM 节点，否则图标会被重建、动画中断。
- 缩放不要用 `width` 动画（触发 layout），用 `transform: translateX() scale()` 配合 `will-change: transform` 走合成层；图标绝对定位，位置由 JS 每帧写入 `transform`，完全绕开 flex 布局。

**启动弹跳**：图标启动后播放 2-3 次弹跳（CSS `@keyframes`：`translateY(0 → -18px → 0)` 带 `cubic-bezier` 缓动），直到检测到该 App 的窗口出现则停止（由 `dock://app-launched` 事件驱动）。这个细节对"像 macOS"的贡献远超其实现成本。

**拖拽排序**：用 Pointer Events 自己实现（不用 HTML5 DnD，它的视觉反馈不可控）。拖动时其余图标做弹簧式让位动画。

**从资源管理器拖 .exe 进 Dock 固定**：Tauri v2 提供 `WindowEvent::DragDrop`（`Entered` / `Over` / `Dropped` / `Left`），把 `Dropped` 的路径直接交给固定逻辑即可，无需任何 Win32 代码。**这是 Tauri 方案相对纯原生的一大红利。**
（注意：仅当 Dock 未提权时可用，见 §4.1.1 兜底开关。）

**自动隐藏与贴边触发**：

```
状态机：HIDDEN ──(鼠标进入触发条)──▶ REVEALING ──▶ SHOWN
          ▲                                            │
          └────(鼠标离开 Dock 区域 > 400ms)──── HIDING ◀┘
```

- 轮询周期：隐藏态 30 Hz（省电），显示态 60 Hz；
- 隐藏判定用**延迟 + 边界框**双重条件，避免鼠标快速划过时闪烁；
- 显示/隐藏动画由 Rust 侧推进：插值窗口 y 坐标，`easeOutBack` 用于显示（带轻微过冲，很像 macOS），`easeInQuad` 用于隐藏；
- 若用户正在拖拽图标出 Dock（如拖出到桌面），**必须禁止隐藏**；
- 右键菜单打开时**必须禁止隐藏**。

#### 图标的光学尺寸与「视觉间距」

**布局间距一致 ≠ 看起来间距一致。** 图标画布是固定方的，但**图形自己在画布里留多少白
各不相同** —— 实测：

| 图标 | 图形占画布宽 | 每侧留白（iconSize=38 时） |
|---|---|---|
| 回收站（Windows shell 图标） | 75% | 4.8 逻辑像素 |
| 此电脑（Windows shell 图标） | 88% | 2.4 |
| Edge / Chrome / QQ / 微信 | 100% | 0 |

布局间距全是 10，量出来的**视觉间距**（= 布局间距 + 左边图形的右留白 + 右边图形的左留白）
却是「此电脑↔回收站 17.1、回收站↔资源管理器 14.8、应用之间 10」——
用户反馈的「左侧那几个图标的间隔太大」就是这个，**不是布局排松了**。

因此分两层处理，两层都按量出来的数字算，没有写死的常数：

1. **图形归一化**（`lib/ipc.ts::encodeOptical`）：把图形放大到在画布内尽可能大，
   `k = min(画布宽/图形宽, 画布高/图形高, 1.6)`。
   ⚠️ **不能让图形超出画布**：画布就是边界，超出去的部分会被 `drawImage` 裁掉
   （第一版给了 1.1 倍纵向余量，回收站的顶部就被切了一条）。
   已经填满画布的图标（应用图标）倍率算出来是 1，**一律不动**，不改变现有观感。
2. **光学间距**（`Dock.tsx::gapBetween`）：相邻间距扣掉两者归一化后的留白，
   让**视觉间距**对所有图标一致；有下限 `MIN_GAP = 3`，留白再大也不让图形贴到一起。
   初始布局和拖拽让位预览**必须走同一个函数**，否则松手瞬间会跳几个像素。

结果（页面里量出来的）：`此电脑↔回收站 = 10.0`、`回收站↔资源管理器 = 10.0`、
应用之间 `10.0` —— 全部一致。对应地，布局间距变成 `6.4 / 6.5 / 10.0`（该紧的紧）。

> 量法：自检 `DOCK_ICONBOX=1` 一次打印三层数字 —— 布局间距（页面 `getBoundingClientRect`）、
> 图标的**原始**留白（Rust 侧 alpha 包围盒）、**渲染后**的留白与视觉间距
> （把 `<img>` 的 data URL 解回来在页面里扫 alpha 包围盒）。三层对齐才能定位到底该改哪一层。

#### 大文件夹（程序坞文件夹）

Dock 上可以放**文件夹**：占**一个图标位**，点开后**在原地向上展开**一个图标网格
（像手机桌面的大文件夹），不是弹窗、也不是跳转页面。

**数据模型**：和分割线一样是 `pinned` 里的普通条目（`is_folder: true`），
孩子内嵌在 `children` 里、**不占顶层位置**：

```jsonc
{ "id": "folder-…", "displayName": "常用", "isFolder": true,
  "children": [ { "id": "…", "target": "…" }, … ] }
```

于是它自动获得普通条目的能力：能拖动排序、能拖出 Dock 删除、能被右键移除。
**不支持嵌套**（`add_to_folder` 会拒绝）—— 嵌套会让"点开→再点开"变成
一条没有尽头的路径，收益为零。

**三条交互路径**（都实测过，见自检 `drive_menu_test` 的"大文件夹"段）：

| 动作 | 行为 |
|---|---|
| 右键图标 →「新建文件夹」 | **原位**变成文件夹，该图标自己成为里面第一个（位置不变） |
| 拖动图标到文件夹上 | 放进文件夹（顶层少一个、文件夹里多一个）；拖拽期间文件夹**描蓝边** |
| 右键文件夹 →「解散文件夹」 | 里面的条目回到顶层、放在文件夹原来的位置；文件夹消失 |
| 右键文件夹**里的图标** | 弹**它自己的菜单**：打开 / 以管理员身份运行 / **移出文件夹** / 打开文件所在位置 |
| 「移出文件夹」 | 挪到**顶层**（放在文件夹后面），**不是删掉** —— 删掉是顶层才有的「移出 Dock」 |

**渲染**：

- **运行点只属于应用，不属于文件夹**：文件夹自己**不点**（它不是应用、也没有窗口），
  里面**在跑的那一格**才点（且**永远是白点**，和 Dock 上一致，不随主题变黑）
  —— 展开之后一眼能看出哪个开着。
- Dock 上的文件夹图标 = 一个半透明圆角容器 + 里面**前 4 个**孩子的缩略图（2×2）。
  取前 4 个就够 —— 预览就那么大，多取是白花时间。空文件夹显示空容器。
- 点开后的完整网格复用**浮层窗口**（右键菜单那一套：贴 Dock 上方、以光标为中心、
  不抢焦点、Esc/点别处关闭、显示前先让页面渲染完）。
  尺寸（列数 / 格子边长 / 间距 / 内边距）由 Rust 算好放进 `PanelPayload`，
  页面照用 —— 和菜单同一套"一份真相"的规矩。
- 行数受**屏幕可用高度**约束（最多 5 行），超出由页面滚动：
  面板必须能整个放在 Dock 上方，否则顶部会被屏幕切掉。

**两层叠着：菜单盖在文件夹上**

在文件夹里右键一个图标，菜单要**盖在文件夹上面**，文件夹**不能关**；点外面先关菜单、
再点一次才关文件夹（Esc 同理）。所以是两个窗口、两层状态：

| | 窗口标签 | 页面 | 内容 |
|---|---|---|---|
| 下层 | `panel` | `panel.html` | 文件夹图标网格 |
| 上层 | `menu` | `panel.html`（同一个页面） | 菜单项列表 / **悬停标签**（`Label`，见 §4.1.3） |

两者共用同一个页面（按 payload 的 `kind` 分支渲染）和同一套状态机
（`panel_window.rs` 里的 `Slot` 各开一个实例）：创建、定位、显示时序、看门、暂停自动隐藏。
**悬停标签借用菜单层的窗口**（不新增 WebView），所以页面有 `menu` / `folder` / `label` 三种形态；
菜单真开着的时候不弹标签（同一个窗口，弹了会把菜单顶掉）。

四条必须记住的规矩：1. **整个栈只有一个看门线程**。`GetAsyncKeyState` 的"上次查询之后按过"位是**一次性**的，
   两个线程各查一次会互相偷事件 —— 短于一帧的点击只会被其中一个看到，
   如果被"没轮到关"的那层拿走，这次点击就凭空消失了。
   每 tick 取一次 `top_slot()` 再决定关谁，层数变化下一 tick 自动生效。
2. `hide_all`（Dock 上任何一次按下）关**两层**；`panel_invoke`（选中菜单项）只关**菜单层** ——
   文件夹要留着。选中之后文件夹内容可能变了（移出文件夹），要**原地刷新**它。
3. 浮层窗口**等 Dock 窗口层就绪之后再建**。三个 WebView 一起同步建会把主线程占住几秒，
   而窗口层初始化是投递到主线程的 —— 它会被推迟到页面首次上报**之后**，
   于是"用初值定位"把真实宽度覆盖掉，Dock 永久过宽（本轮实测踩到）。
4. **「同一个图标再点一次 = 关掉」只能拿真菜单比，不能只看 `target_id`。**
   悬停标签和菜单共用菜单层窗口、也共用"当前条目 id"这个字段 ——
   右键一个正显示名字的图标时，如果按 id 判断，就会把这次右键当成"再点一次"，
   于是标签被关掉、菜单永远弹不出来（实测表现：`[FAIL] 右键 → 菜单窗口已显示`，
   而量到的"菜单矩形"其实是 74×29 的标签）。见 `Slot::same_menu_open`。




实现与取舍（2026-09 更新，取代原「原生 `TrackPopupMenu`」方案）：

| | 原生 `TrackPopupMenu` | **独立菜单窗口**（现方案） |
|---|---|---|
| 超出 Dock 窗口边界 | 天生可以 | 天生可以（自己就是窗口） |
| 外观 | Windows 原生，无法定制 | 页面随便画（macOS 观感） |
| 对调用线程 | **自带模态消息循环，卡住线程** | 无阻塞 |

菜单窗口是 Dock 的**第三个 WebView 窗口**（`panel.html`，和文件夹面板共用），启动时创建、保持隐藏
（第一次右键才建的话，WebView2 初始化要几百毫秒，右键会明显卡一下）。

**显示时序**（为什么不能直接显示）：

```
右键 → show() 算出内容与矩形、写好状态
     → eval("__dockMenu(seq)")  ──▶ 页面渲染
     ← invoke("panel_ready", seq) ── 渲染完成
     → SetWindowPos + SWP_SHOWWINDOW
     （另有 250ms 兜底：页面出问题也不能让右键毫无反应）
```

窗口尺寸是 Rust 按菜单项算的，而页面内容是**异步渲染**的。先显示再渲染，会看到窗口里
先是上一次的菜单项、几毫秒后才换成新的（一次闪烁）。所以由页面渲染完回调，
Rust 收到才显示。行高、分隔线高度、格子边长、内边距、宽度全部由 Rust 通过 `PanelPayload` 发给页面，
**CSS 里不再写第二份** —— 两份数值一旦不一致就是「最后一行被裁掉」。

**隐藏逻辑**（四条路径，全部收敛到幂等的 `hide()`）：

| 触发 | 实现 |
|---|---|
| 选了某一项 / 点了文件夹里的图标 | `panel_invoke` / `panel_launch` 先取目标、再 `hide()`、最后执行动作 |
| 按 Esc | 看门线程轮询 `GetAsyncKeyState` |
| 点了菜单与 Dock 以外的地方 | 同上（按下 + 光标位置） |
| 点了 Dock 上的东西 | Dock 页面的 `mousedown` 调 `hide_panel` |

为什么靠**轮询**判断「点了别处」：菜单窗口是 `WS_EX_NOACTIVATE` 的，它**永远不会成为
前台窗口**，因此收不到失活通知 —— 「点外面自动消失」没有消息可等。而这条特性又是必须的
（菜单夺走焦点会让用户正在打字的窗口失焦）。看门线程只在菜单打开期间存在，16 ms 一次。

两个踩过的坑，都写在代码注释里：
1. 判断「刚按下」必须同时看 `GetAsyncKeyState` 的 **0x0001 位**（自上次查询后按过）。
   只看 0x8000（当前按着）会漏掉短于一帧的点击 —— 合成点击（以及手很快的真实点击）
   会在两次轮询之间完成按下与抬起。
2. `hide()` 会连「当前浮层的目标」一起清掉，所以 `panel_invoke` 必须**先取目标再关菜单**。
   反过来写的话菜单正常关闭、动作静默不执行，表现为「点了没反应」。

菜单项与动作全部在 Rust 决定（`menu.rs`）：页面只是渲染器，把点击回传。
这是因为几乎每一项都要调 Win32，而「这个条目现在还在不在运行」前端判断不了 ——
一处判断才不会和真实状态不一致。

---

### 4.4 动画层（P3/P4：最小化效果）

macOS 本身提供两种最小化动画（系统设置 → 桌面与程序坞 → "最小化窗口时使用"）。**本方案按 Q5 决策，P3 做缩放效果，P4 可选做精灵效果。**

| | **缩放效果 Scale Effect**（P3，默认） | **精灵效果 Genie Effect**（P4，可选模块） |
|---|---|---|
| 画面 | 窗口整体按比例缩小，飞向 Dock 图标，边飞边淡出 | 窗口像被吸进神灯：底部收窄成尖，顶部保持宽度，画面被**拉长着**卷进图标 |
| 本质 | 纯几何变换（位移 + 缩放 + 透明度） | **图像的非线性形变**，像素像橡皮膜一样弯曲 |
| 实现 | CSS `transform` 1:1 还原 | 网格形变（mesh warp）+ shader |

> **重要**：缩放效果**不是妥协版**，它本身就是 macOS 的原生选项之一，大量 Mac 用户在用它。P3 交付它即可获得"像 macOS"的完整观感。

#### P3：缩放效果实现

1. **抓窗口快照**：`PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT /*2*/)`
   - 优点：简单、同步、无需 GPU 互操作，对绝大多数窗口有效
   - 缺点：对部分硬件加速 / 受保护内容渲染为黑块
2. **动画**：拿到位图后放到一个覆盖全屏的透明无边框 `TOPMOST` 辅助窗口（或用 Dock 主窗口临时扩容），CSS 通过 `transform: translate() scale()` + `opacity` 把它从窗口原始矩形插值到 Dock 图标矩形；
3. **失败降级**：若快照全黑（采样检测），退化为纯缩放淡出，用户几乎无感；
4. 因为动画只有约 250 ms，**快照质量的瑕疵几乎不可见，性价比极高**。

#### P4：精灵效果实现（独立模块，可失败）

网格形变（mesh warp）原理：

1. 把窗口画面当作一张纹理；
2. 在纹理上覆盖一张 `N×M` 的网格（如 32×32 顶点）；
3. 动画每一帧按进度 `t` 计算每个网格顶点的新位置——越靠近底部的顶点越向 Dock 图标汇聚，并沿曲线路径拉长；
4. GPU 用移动后的顶点**重新采样**纹理并绘制（顶点移动、纹理连续采样，所以画面是弯曲而非裁切）。

工程步骤：
1. 用 `Windows.Graphics.Capture`（Win10 1803+，`windows` crate 有 `Graphics::Capture` 绑定）拿到 D3D11 GPU 纹理；
2. 创建 wgpu/D3D11 的**透明叠加子窗口**，覆盖动画区域；
3. 顶点着色器做网格形变，片段着色器采样原纹理。

**这是一次独立的图形工程，不是"加个动画"**，严格隔离在 `win/capture.rs` + 独立渲染子窗口中，失败也不影响主流程。

---

### 4.5 日志与崩溃取证（`logging.rs`）

**需求**（用户 2026-09 原话）："确保在 dock 栏崩溃闪退、或者出 bug 时可以直接在日志中定位问题"。
这条需求先于"去掉启动时那个终端窗口" —— 顺序不能反：先把输出落到文件，才敢把控制台拿掉。

#### 4.5.1 为什么不是"继续 `println!` + 重定向"

两个独立的理由：

1. **没有控制台可看**：Dock 现在是 GUI 子系统（双击不弹终端窗口）。没有控制台时
   `println!` 往**无效句柄**写会**直接 panic**（Rust 把写失败当致命错误）——
   也就是说"去掉控制台"这件事本身就是 `println!` 的雷。所以输出必须先有一个
   **以文件为权威载体**、写失败可容忍的通道；
2. **崩溃现场要写得出**：崩溃可能发生在**日志锁内部或分配器内部**，
   那时候任何"加锁 + 分配内存"的写法都可能二次崩。取证通道必须能绕过日志模块本身。

#### 4.5.2 两条互不重叠的崩溃路径

| 崩溃类型 | 谁抓得到 | 现场留下什么 |
|---|---|---|
| **Rust panic**（`unwrap` 失败、越界、断言） | `std::panic::set_hook` | 日志里的 `💥 PANIC` + 位置（文件:行:列）+ 线程名/id + **符号化回溯**（debug 构建带 `debug=true`，能直接读到 Rust 源码行） |
| **Win32 未处理异常**（访问违例、非法指令、栈溢出…） | `SetUnhandledExceptionFilter` | `crash-<时间>.txt` + `crash-<时间>.dmp` |

⚠️ **关键认知**：`panic` 钩子**拦不到**访问违例 —— 那不是 Rust 的 panic，
既不会 unwind 也不会走钩子，进程直接被系统带走。用户说的"闪退"多半是这一类
（WebView2/显卡驱动的 bug 经常表现为 `0xC0000005`）。所以两条路径都要有。

#### 4.5.3 崩溃处理器里能做什么、不能做什么

**不能**：加锁、分配堆内存、走 `String`/`format!`、调用可能再触发崩溃的东西。
**做法**：栈上的定长缓冲（`StackBuf`，8KB）+ `fmt::Write` 拼报告 → `CreateFileW`/`WriteFile`
直写。报告内容：

```
异常码   0xC0000005（ACCESS_VIOLATION 访问违例）
异常地址 0x00007FF78300E2ED ← dock-app.exe+0x32E2ED      ← 一眼看出崩在哪个模块
访问违例 写入 地址 0x0000000000000000                     ← 操作类型 + 目标地址
---- 崩溃前的最后 40 行日志 ----                          ← 崩溃前发生了什么
```

"归属模块 + 偏移"用 `GetModuleHandleExW(FROM_ADDRESS)` + `GetModuleFileNameW` 反查 ——
不用符号也能立刻区分"崩在我们自己的代码里"还是"崩在显卡驱动 DLL 里"。
随后用 `MiniDumpWriteDump(MiniDumpWithDataSegs)` 落一份 `.dmp`：VS / WinDbg 打开即可
看到完整调用栈（PDB 就在 exe 旁边，符号直接解析到 Rust 源码行）。

处理完返回 `EXCEPTION_EXECUTE_HANDLER`：**记录归记录，不吞异常** ——
吞掉只会让进程停在半死不活的状态，比崩掉更难查。

#### 4.5.4 其它几条设计

- **级别**：`DOCK_LOG=error|warn|info|debug|trace`（默认 `info`）。
  转换时做过一次分级：产品代码 94 处打印里，**枚举/图标/窗口树这类"每次刷新都刷屏"的降到 `debug`**，
  生命周期与用户动作留在 `info` —— 否则日志被刷成流水账，真出事时翻不到；
- **文件**：按天 `dock-YYYY-MM-DD.log`；超 8MB 轮转 `.1.log`；保留 7 天。
  崩溃报告保留最近 20 份。日志目录：`DOCK_LOG_DIR` > `DOCK_CONFIG_DIR/logs`（自检隔离）
  > `%LOCALAPPDATA%\dev.local.dock\logs`；
- **心跳**：每 5 分钟一行（运行时长 + 工作集）。两个用途：8 小时验收看得到"还活着、
  内存没有一路涨"；**闪退时最后一条心跳的时间就是死亡时刻**；
- **前端通道**：WebView 是独立进程，页面里的未捕获异常/`console.error`/React 渲染报错
  在 Rust 日志里本来一个字都看不到 —— 前端 `src/lib/log.ts` 把
  `window.onerror`、`unhandledrejection`、`console.error/warn` 转发到 `js_log` 命令，
  日志里以 `[前端 dock|panel|settings|prompt]` 标出是哪个窗口；
- **每条 flush**：一条日志 ~2µs，换"崩溃时最后一条一定在盘上"，值得；
- **控制台镜像**：仍然往 stdout 写一份（**写失败忽略**）。自检脚本靠 stdout 解析
  `[PASS]`/`[FAIL]`，所以这层不能省 —— 实测加前缀后门禁的解析规则照样命中。

#### 4.5.5 实测（2026-09）

| 场景 | 结果 |
|---|---|
| 正常启动（GUI 子系统，无终端窗口） | 日志生成，会话头 12 行（版本/pid/参数/配置目录/级别/环境开关）；`[前端 dock] 页面已加载（/）` 证明前端通道通 |
| `DOCK_CRASH_TEST=panic` | 退出码 **101**；日志出现 `💥 PANIC …` + `位置 main.rs:…` + 线程 + **符号化回溯**（`dock_app::…  at .\app\src-tauri\src\main.rs:175`） |
| `DOCK_CRASH_TEST=av` | 退出码 **0xC0000005**；`crash-*.txt` 记录异常码 / 地址 / `dock-app.exe+0x32E2ED` / 写地址 `0x0` / 崩溃前 40 行；同目录 **645 KB** 的 `.dmp` |
| 自检版（`--features selftest`） | 门禁照旧工作：stdout 里 `[PASS]`/`小结`/`构建 ` 仍被脚本命中；自检日志单独落在 `DOCK_CONFIG_DIR/logs/` |

---

## 5. 数据模型与接口

### 5.1 数据结构

```rust
/// Dock 上的一个应用条目
pub struct DockApp {
    pub id: String,             // 稳定标识：AUMID，或 exe 路径的 sha1
    pub display_name: String,
    pub kind: AppKind,          // Win32 | Uwp | Shortcut | Url
    pub target: String,         // exe 路径 / AUMID / URI
    pub args: Option<String>,
    pub icon_path: Option<PathBuf>,  // 缓存 PNG 的项目相对路径
    pub pinned: bool,
    pub pinned_order: Option<u32>,
}

/// 运行态快照（每次枚举后重建）
pub struct RunningState {
    pub app_id: String,
    pub windows: Vec<WindowRef>,   // hwnd 以 u64 传前端（指针不可跨 IPC 语义化）
    pub window_count: usize,
    pub has_foreground: bool,
    pub is_elevated: bool,         // 决定盾牌角标与菜单禁用，见 §4.1.1
}

/// 单个窗口引用
pub struct WindowRef {
    pub hwnd: u64,
    pub title: String,
    pub minimized: bool,
    pub can_control: bool,         // false = 提权窗口，写入类操作将被禁用
}

pub struct Preferences {
    pub edge: Edge,                     // Bottom | Left | Right
    pub monitor: MonitorSelector,       // Primary | Active | Specific(id)
    pub avoid_taskbar: bool,            // 默认 true（Q1 决策）：基于 rcWork 定位
    pub icon_size: u32,                 // px，逻辑像素
    pub magnification: f32,             // 0.0 - 1.0
    pub auto_hide: bool,
    pub hide_delay_ms: u32,             // 默认 400
    pub show_running_indicators: bool,
    pub minimize_effect: MinimizeEffect,// Scale（默认） | Genie（P4）| None
    pub theme: Theme,                   // Light | Dark | System
    pub launch_at_login: bool,
    pub run_self_as_admin: bool,        // 默认 false，兜底开关，见 §4.1.1
}
```

### 5.2 Rust ↔ 前端命令（`invoke`）

| 命令 | 说明 |
|---|---|
| `list_apps()` | 返回 Dock 要显示的 `DockApp[]`。**顺序 = 用户列表的顺序**，运行态只是附带信息（画白点、决定点击是启动还是切换）—— 见下方「应用顺序模型」 |
| `reorder_apps(ids: Vec<String>)` | 保存新顺序（Dock 内拖拽排序结束时调用）。**已实现**：`store::reorder` 要求 id 集合不重不漏，否则整条拒绝、不写盘 |
| `remove_dock_app(id)` | 从 Dock 移除（拖出 Dock 松手 / 右键菜单） |
| `add_separator(after_id)` | 在某个条目**后面**插一条分割线；返回新条目的 id |
| `add_system_location(target)` | 把「此电脑 / 回收站」这类**系统位置**放到 Dock **最左侧**（表在 `apps::SYSTEM_LOCATIONS`） |
| `show_panel_menu(owner_hwnd, app_id, anchor_x)` | 弹出右键菜单（浮层窗口）。返回「调用后浮层是不是打开的」——同一个图标再右键一次会关掉 |
| `show_icon_label(owner_hwnd, app_id, anchor_x)` / `hide_icon_label()` | 悬停名字（浮层窗口的 `Label` 形态，**图标正上方**，不是原生 tooltip；见 §4.1.3） |
| `show_panel_folder(owner_hwnd, folder_id, anchor_x)` | 展开**文件夹**（浮层窗口，和菜单共用） |
| `hide_panel()` | 关掉浮层（幂等） |
| `panel_state()` / `panel_ready(seq)` | 浮层页面读内容 / 报告渲染完成（见 §4.3 的显示时序） |
| `panel_invoke(item_id)` / `panel_launch(item_id)` | 执行菜单项 / 点开文件夹里的图标（先关浮层、再取目标、最后执行并刷新 Dock） |
| `create_folder(app_id)` / `add_app_to_folder(folder_id, app_id)` / `dissolve_folder(folder_id)` / `move_out_of_folder(app_id)` | 新建文件夹 / 放进去 / 解散 / **移出文件夹**（挪到顶层，不删；见 §4.2 大文件夹） |
| `launch_app(id)` | 启动（含 UWP 分支） |
| `run_as_admin(id)` | **以管理员身份运行**（`lpVerb="runas"`）；用户取消 UAC 返回 `Cancelled`，不算错误 |
| `restart_as_admin(id)` | 可选（P2）：关闭后提权重启，**需前端二次确认** |
| `activate_app(id)` | 激活 / 在窗口间循环 |
| `minimize_app(id)` / `close_app(id)` / `quit_app(id)` | 窗口与进程操作；对提权窗口返回 `Err(NeedsAdmin)` |
| `get_icon_png(id)` | 返回可直接用于 `<img>` 的 asset URL |
| `get_preferences()` / `set_preferences(p)` | 配置读写 |
| `list_installed_apps()` | 供"添加应用"选择器（包装 `Get-StartApps`） |
| `set_dock_rect(w, h)` | 前端布局完成后回报尺寸，Rust 据此调整窗口与触发条 |
| `set_ignore_mouse(bool)` | 显示 / 隐藏时切换鼠标穿透 |

**错误约定**：所有写操作返回 `Result<T, DockError>`，`DockError` 枚举至少包含 `NeedsAdmin`、`ForegroundDenied`、`TargetGone`、`UiAccessDenied`，前端据此给出**具体且可操作**的提示（而非笼统的"操作失败"）。

### 5.3 Rust → 前端事件（`emit`）

| 事件 | 载荷 | 触发时机 |
|---|---|---|
| `dock://apps-changed` | `DockApp[]` + 运行态 | `SetWinEventHook` 防抖后 |
| `dock://visibility-changed` | `{ visible: bool }` | 显示 / 隐藏动画开始 |
| `dock://geometry-changed` | `{ x, y, width, height, scale_factor }` | 显示器切换 / DPI 变化 |
| `dock://app-launched` | `{ id }` | 用于停止启动弹跳 |
| `dock://error` | `{ code, message }` | 权限不足、激活失败、UAC 被取消等，前端可提示 |

**注意**：`apps-changed` 可能在窗口频繁开关时高频触发，前端必须做 diff（按 `id` 复用 React 元素），否则图标会反复重建、动画被打断。

---

## 6. 多显示器与 DPI

### 6.1 要点

1. `windows/app.manifest` 中声明 `dpiAwareness = PerMonitorV2`，否则在混合 DPI 的多屏环境下整个 Dock 会糊。
2. `EnumDisplayMonitors` + `GetMonitorInfoW` 拿每块屏的 `rcMonitor`（物理边界）与 **`rcWork`（工作区，已排除任务栏）**。
   **Q1 决策：默认基于 `rcWork` 贴底**（`avoid_taskbar = true`），Dock 显示在任务栏正上方，两者并存、互不遮挡。若用户想覆盖任务栏区域，可在设置中切换为 `rcMonitor`。
   > **P0 环境实测（重要）**：开发机上的任务栏是**自动隐藏**的，此时 `rcWork == rcMonitor`（实测任务栏高度 0 px）。**`rcWork` 规则在"任务栏自动隐藏"时不再提供任何避让**——必须额外处理"鼠标触底 → 任务栏弹出并盖住 Dock"的冲突（风险 R18），建议检测到任务栏弹出时让 Dock 临时上移。
   >
   > **P1 落地**：`Shell_TrayWnd` 始终存在，直接量它的矩形就能同时拿到高度与状态，
   > 于是自动隐藏时手动扣掉一个任务栏高度（见 `win_layer::dock_rect_for_size`）。
   >
   > **⚠️ 2026-09 修正（这个"量矩形"的写法本身是错的，真踩到了）**：用「窗口顶边是否贴着屏幕底」
   > 判断"是不是自动隐藏"会漏掉**任务栏正在弹出的那一瞬间** —— 那时它的顶边回到 1020，
   > 看着和常显一样，判断成"不用留空"，可 `rcWork` 仍是整屏（因为配置是自动隐藏）。
   > 结果是 Dock 底边贴到 1080，**永久压在任务栏 1020-1080 的弹出区上**：
   > 鼠标移到 Dock 图标上 → 触发任务栏弹出 → 任务栏（`TOPMOST`）盖住 Dock。
   > 而且它随"启动那一刻任务栏弹没弹出来"而变，属于随机复现。
   >
   > 正确写法（两个值都与弹出/收起无关）：
   > - 是不是自动隐藏 → `SHAppBarMessage(ABM_GETSTATE)` 的 `ABS_AUTOHIDE` 位；
   > - 要留多高 → `SHAppBarMessage(ABM_GETTASKBARPOS)` 的 `rc`（**吸附矩形**，恒定 60px），
   >   并检查 `uEdge == ABE_BOTTOM`（只有吸附在底边才从底边扣）。
   >
   > 实测（用户机：1920×1080、任务栏自动隐藏 60px）：修前 Dock `(453,1010)-(1466,1080)`，
   > 图标行中心最上层是 `Shell_TrayWnd`；修后 Dock `(453,950)-(1466,1020)`，
   > 任务栏弹出时图标行中心最上层仍是 `Dock`，任务栏区内一点是 `Shell_TrayWnd`，**零重叠**。
3. **`bottomOffset`（2026-09 新增）**：Dock 底边距「基准底边」多少**逻辑像素**，由用户决定，   0 = 贴底。基准底边就是上面第 2 条算出来的那个（任务栏常显时是 `rcWork` 底，
   自动隐藏时再扣掉任务栏高度）。用户想在 Dock 与屏幕底边之间留多少空间就看它 ——
   设置页里有滑块，改完**立刻生效**（重算 y → `reveal::set_target`，不等重启）。
   实测：距底部 120 逻辑像素 → 窗口上移 150 物理像素（125% DPI），启动路径与实时路径都验过。
4. 配置 `monitor: Active` 时，用 `MonitorFromPoint(GetCursorPos())` 跟随鼠标所在屏，跨屏移动时迁移 Dock 窗口。迁移过程要做淡出 → 移动 → 淡入，避免瞬移的突兀感。
5. Rust 侧一律用**物理像素**计算窗口位置；前端用逻辑像素布局，由 Tauri 在 IPC 层转换。涉及"前端布局结果 → 窗口物理尺寸"的换算，必须显式乘 `scale_factor`，**这里是高 DPI bug 的高发区**。
6. 图标固定按**逻辑像素**（如 48/56/64）设置，物理像素 = 逻辑 × scale，避免 4K 屏上图标变得巨大。
7. **任务栏位置变化**（用户把任务栏挪到顶部/左侧，或改了任务栏高度）会导致 `rcWork` 改变，需要监听 `WM_SETTINGCHANGE` / `WM_DISPLAYCHANGE` 并重新计算 Dock 位置。

### 6.2 测试矩阵

| 场景 | 预期 |
|---|---|
| 100% 单屏，任务栏在底部 | 基线：Dock 紧贴任务栏上方 |
| 150% 单屏 | 图标清晰、无 1px 模糊、位置正确 |
| 100% + 200% 双屏，跨屏拖动鼠标 | Dock 正确迁移、尺寸按目标屏重算 |
| **任务栏改到顶部 / 左侧** | **Dock 跟随重算，不与之重叠（Q1 决策的关键用例）** |
| **任务栏高度调整 / 自动隐藏** | Dock 位置正确更新，不出现空隙漂移 |
| 竖屏（旋转 90°） | 贴底方向正确 |
| 分辨率热切换（接投影 / 拔线） | 重新计算，不残留旧坐标 |

---

## 7. 工程结构

```
Dock/
├─ docs/
│  └─ dock-technical-design.md
├─ src-tauri/
│  ├─ Cargo.toml
│  ├─ tauri.conf.json
│  ├─ windows/
│  │  └─ app.manifest            # PerMonitorV2 DPI
│  ├─ icons/
│  └─ src/
│     ├─ main.rs                 # 入口、插件注册、单实例
│     ├─ commands.rs             # #[tauri::command] 全部集中于此
│     ├─ error.rs                # DockError 枚举（前端提示的依据）
│     ├─ model.rs                # 纯数据结构（可单测）
│     ├─ store.rs                # 配置持久化与迁移
│     ├─ apps.rs                 # 应用模型聚合、固定/运行合并（纯逻辑，可单测）
│     └─ win/
│        ├─ mod.rs
│        ├─ window.rs            # 窗口创建、样式、置顶、鼠标穿透
│        ├─ glass.rs             # 毛玻璃两条路线
│        ├─ monitor.rs           # 显示器枚举、DPI、rcWork 工作区
│        ├─ trigger.rs           # 触发条窗口 + 贴边检测状态机
│        ├─ reveal.rs            # 显示/隐藏动画推进
│        ├─ enum_windows.rs      # 窗口枚举 + cloaked 过滤
│        ├─ identity.rs          # AUMID / 进程路径 / 身份解析
│        ├─ privilege.rs         # 提权状态检测（TokenElevation）
│        ├─ icons.rs             # IShellItemImageFactory + 缓存
│        ├─ shell.rs             # 启动（含 runas）/ 激活 / 最小化 / 关闭
│        ├─ events.rs            # SetWinEventHook + 防抖
│        └─ capture.rs           # 缩略图快照（P3）/ 网格形变（P4）
├─ src/                          # 前端（React）
│  ├─ main.tsx
│  ├─ App.tsx                    # 容器 + 全局状态
│  ├─ components/
│  │  ├─ Dock.tsx                # 图标行布局
│  │  ├─ AppIcon.tsx             # 单图标（弹跳、指示点、盾牌角标）
│  │  ├─ ContextMenu.tsx         # 右键菜单（含以管理员身份运行）
│  │  ├─ Settings.tsx
│  │  └─ Preview.tsx             # 窗口缩略图预览条（P3）
│  ├─ hooks/
│  │  ├─ useDockApps.ts          # 订阅 dock://apps-changed + diff
│  │  └─ useDragReorder.ts
│  ├─ lib/
│  │  ├─ magnify.ts              # 鱼眼数学（纯函数，可单测）
│  │  └─ ipc.ts                  # invoke/emit 的类型化封装
│  ├─ types.ts                   # 与 Rust model.rs 对齐的 TS 类型
│  └─ styles/glass.css
├─ package.json
├─ vite.config.ts
└─ README.md
```

**可单测的纯逻辑**（不碰 Win32，值得写测试）：
`magnify.ts`（衰减退化到边界值）、`apps.rs`（固定/运行合并、去重、顺序稳定性）、`store.rs`（配置默认值与版本迁移）。

**类型同步建议**：`types.ts` 与 `model.rs` 手工保持对齐容易漂移。可用 `ts-rs` 从 Rust 结构体自动导出 TS 类型，几十行配置一次性解决。

---

## 8. 路线图

### P0 · 可行性验证（1-2 天）

**唯一目标：把三个不确定点变成确定点。**

1. 毛玻璃两条路线各出一个最小 demo，按 §4.1 的四项清单打勾选定；
2. `WS_EX_NOACTIVATE` 生效验证：点击 Dock 时当前编辑器/浏览器**不失焦**；
3. 触发条 + 主窗口的双窗口自动隐藏在 120 Hz 下无掉帧。

> **决策门**：若 1 全部不达标，先确认降级方案（CSS 伪玻璃）的观感是否可接受，再决定是否继续。

### P1 · 能用（3-7 天）

- 应用模型 + 配置持久化（先支持手工/配置文件添加）
- 图标提取与缓存
- 点击启动、激活、最小化，运行指示点
- 右键菜单：打开 / 最小化 / 取消固定 / 退出 / 结束进程 / **以管理员身份运行**
- **提权状态检测 + 盾牌角标 + 菜单禁用降级**（§4.1.1）
- 鱼眼放大
- 自动隐藏 + 贴边弹出
- 基于 `rcWork` 的定位（Q1）

**验收**：
1. 能替代任务栏完成日常"启动和切换常用应用"；
2. 连续运行 8 小时不崩、无明显内存增长；
3. **对提权程序，没有任何一次"点了没反应"**——要么成功，要么有明确提示。

### P2 · 好用（1-2 周）

- `SetWinEventHook` 自动发现运行窗口，替换轮询
- 拖拽排序、从资源管理器拖入固定、"添加应用"选择器（`Get-StartApps`）
- 多显示器与 PerMonitorV2 DPI 全矩阵通过（含任务栏换位用例）
- 启动弹跳、激活失败降级提示
- 「以管理员身份重新启动」（带二次确认）
- 托盘菜单、开机自启、设置界面
- NSIS 安装包

**验收**：§6.2 测试矩阵全绿；连续使用一周无需重启。

### P3 · 像（2-4 周）

- **最小化缩放效果动画**（`PrintWindow` 快照 + 位移缩放淡出，见 §4.4）
- 悬停窗口缩略图预览条
- 角标 / 通知数、文件夹（堆栈）
- 主题与自定义外观、设置界面打磨
- 代码签名 + 安装引导（仅为了消除 SmartScreen 警告，**不涉及 uiAccess**）

### P4 · 极致（按需，独立模块）

- **精灵形变 shader**（独立 wgpu 渲染子窗口 + `Windows.Graphics.Capture`）
- 多种玻璃材质与动态壁纸取样
- Dock 左右侧贴边模式
- 事件驱动的低功耗模式（`SetWinEventHook` + 无操作时降到 10 Hz 轮询）

---

## 9. 风险登记册

| # | 风险 | 等级 | 影响 | 应对 |
|---|---|---|---|---|
| ~~R1~~ | ~~`DWMWA_SYSTEMBACKDROP_TYPE` 与透明/分层窗口不兼容~~ | ~~高~~ | — | **P0 实测销案**：`plain` 与 `WS_EX_NOREDIRECTIONBITMAP`（Tauri 所用）下均正常（76 px / 对比度 92）；仅 `WS_EX_LAYERED` 会削弱到 66 px → 不使用该样式即可 |
| R2 | `SetForegroundWindow` 前台锁定导致点击无反应 | **高** | 核心功能主观失效 | §4.1 的四级激活链路 + 失败降级（闪烁任务栏）+ 明确错误提示。**P0 已实测验证**：后台进程直接调用会被**静默忽略**，先 `AttachThreadInput` 附加到前台线程后调用有效 |
| ~~R5~~ | ~~亚克力 + 高频窗口移动导致掉帧 / 输入延迟~~ | ~~中~~ | — | **P0 实测销案**：单次 `SetWindowPos` 313 µs（有亚克力）vs 317 µs（无玻璃），最坏占 60 Hz 帧预算 **12.4%**，120 Hz 预算下仍有 4 倍余量 |
| R3 | 提权窗口无法被关闭 / 最小化 | 中低 | 部分操作失效 | **Q2 决策已把风险从架构层移走**：改为提供「以管理员身份运行」；对提权窗口走盾牌标注 + 菜单禁用 + 明确提示（§4.1.1）。剩余风险是"用户仍觉得不够用"，可用 `run_self_as_admin` 兜底开关覆盖 |
| R4 | `ShowWindow` / `SetForegroundWindow` 对提权窗口的实际行为未定 | 中 | 降级策略可能过严或过松 | P1 实测四个组合，据结果调整 `can_control` 判定 |
| R6 | 应用身份识别不准，图标分组混乱 | 中 | 观感"不像 macOS" | 三级身份解析 + 可手动"合并/分离"条目 + 用户可覆盖 |
| R7 | `PrintWindow` 对硬件加速窗口返回黑图 | 中 | 最小化动画偶发黑块 | 检测到全黑则退化为纯缩放淡出；P4 换 `Windows.Graphics.Capture` |
| R8 | 图标在高 DPI 下模糊 | 中 | 质感差 | 强制走 `IShellItemImageFactory` 取 256 px，禁用 `ExtractIconEx` |
| R9 | `explorer.exe` 崩溃重启导致 shell API 短暂失效 | 低 | 短暂异常 | 每次调用做结果校验 + 定时重建；不要缓存 COM 对象跨会话 |
| R10 | 杀软 / 反作弊对常驻置顶程序的误报 | 低 | 被拦截 | 明确不使用 `WH_MOUSE_LL`；不注入任何进程；不做键盘监听 |
| R11 | 独占全屏游戏无法覆盖 | 低（已知限制） | 游戏内看不到 Dock | 已在 §1.2 声明；检测到独占全屏时自动让位 |
| R12 | 常驻内存随时间增长 | 中 | 长期体验差 | 图标缓存上限（LRU，如 200 项）；窗口枚举结果不做无界累积；P1 起加内存基线回归测试 |
| R13 | 任务栏位置/高度变化后 Dock 位置漂移 | 中低 | 视觉错位 | 监听 `WM_SETTINGCHANGE` / `WM_DISPLAYCHANGE` 重算 `rcWork` |
| R14 | WebView2 运行时缺失（极旧的 Win11 镜像） | 低 | 无法启动 | 安装包内置 WebView2 bootstrapper |
| **R15** | **Tauri/tao 未设置 `WS_EX_NOACTIVATE`；同进程点击会夺焦点** | **中低** | 同进程下 Dock 可能抢走焦点 | **P1 已查清（完整焦点矩阵见 `docs/p1-window-layer-findings.md` §6.14）**：① 手动补 `WS_EX_NOACTIVATE`；② **跨进程（真实工况）点击 Dock 不夺焦点，已在真实 Tauri 窗口上自动化验证** ✅；③ 同进程仍会夺焦点，且 `WM_MOUSEACTIVATE` 钩子**真实点击时走不到**（消息落到无法子类化的深层 Chromium 窗口，是 comctl32 线程局部的硬限制）。**实际防护改为：Dock 不创建任何会获得焦点的自有窗口**（将来的设置界面必须加 `WS_EX_NOACTIVATE`） |
| R16 | DWM 材质跟随系统主题，浅色下是一层不透明浅底 | 中 | 深色 Dock 观感不符 | 采用"OS 模糊 + CSS 染色"叠加；或试 `DWMWA_USE_IMMERSIVE_DARK_MODE /*20*/` |
| R17 | 裸 Win32 窗口的 P0 结论未必完全适用于 Tauri 窗口 | 中 | 毛玻璃/焦点结论可能失效 | **已应验并已处理**：P1 复验发现**毛玻璃路线结论确实失效**（A 在 Tauri 上不模糊），已改选路线 B；焦点结论经受控实验确认有效 |
| ~~R18~~ | ~~任务栏自动隐藏时 `rcWork == rcMonitor`，Dock 会被弹出的任务栏盖住~~ | ~~中~~ | — | **P1 已解决**（**2026-09 又修了一次**）：预留高度改为问 `ABM_GETSTATE` + `ABM_GETTASKBARPOS`，与任务栏此刻弹没弹出来无关。实测弹出后 Dock `950..1020`、任务栏 `1020..1080`，**零重叠**。详见 §6.1 第 2 条的修正说明与 `docs/p1-window-layer-findings.md` §6.16 |

---

## 10. 开发环境准备

当前机器已具备：**Node 24、Rust 1.95、git、Windows 11 build 26200**。还需补齐：

```powershell
# 1. Rust target（MSVC toolchain）
rustup target list --installed

# 2. Tauri CLI
cargo install tauri-cli --version "^2"

# 3. 前端脚手架（Q3：React + TS + Vite）
npm create vite@latest . -- --template react-ts

# 4. WebView2 Runtime（Win11 通常已预装，缺失则装 Evergreen Bootstrapper）

# 5. 初始化 Tauri（在 C:\Program1\Projects\Dock 下）
cargo tauri init
```

**开发调试注意**：透明窗口 + 亚克力的效果**无法在浏览器里预览**，每次调整都要 `cargo tauri dev`。建议把玻璃参数抽到配置文件里，配合 Tauri 的热重载减少重启次数。

---

## 11. 决策记录

### 11.1 已决策（Q1-Q5）

| # | 问题 | 决策 | 对方案的影响 |
|---|---|---|---|
| **Q1** | Dock 与任务栏的关系 | **并存**，Dock 位于任务栏上方 | 定位基准由 `rcMonitor` 改为 **`rcWork`**；新增 `avoid_taskbar` 配置与任务栏换位测试用例（§6） |
| **Q2** | 是否需要操作提权窗口 | **不需要**。改为右键菜单提供「以管理员身份运行」，Dock 自身不提权 | 移除 `uiAccess` + 代码签名的架构诉求；新增 §4.1.1 提权启动与 UI 降级设计；风险 R3 由"高成本"降为"中低" |
| **Q3** | 前端框架 | **React 19** + Vite，不引入 UI 组件库 | §2/§7 更新；补充 §4.3 的 React 性能要点（鼠标位置不进 state） |
| **Q4** | 多语言与自动更新 | **暂不做** | 不引入 `tauri-plugin-updater`；文案抽常量即可，不做 i18n 框架 |
| **Q5** | 最小化动画 | **P3 做缩放效果**（macOS 原生选项之一）；**精灵效果降为 P4 可选模块** | §4.4 重写；`minimize_effect` 进配置；P3/P4 边界清晰 |

### 11.2 剩余待定

| # | 问题 | 何时需要决定 | 影响 |
|---|---|---|---|
| ~~O1~~ | ~~毛玻璃走路线 A 还是 B~~ | **已定** | **P1 在真实 Tauri 窗口上复验后推翻 P0 结论：改选路线 B**。路线 A 在裸 Win32 窗口可用，但在 WebView2 子窗口覆盖客户端的拓扑下完全不产生模糊。详见 §4.1 与 `docs/p1-window-layer-findings.md` §6.6 |
| O2 | 是否实现「以管理员身份重新启动」（关闭后提权重启） | P2 开始前 | 需处理"未保存数据丢失"的二次确认交互 |
| O3 | 图标尺寸与影响半径的默认值 | P1 调参期 | 纯观感，无需提前定 |
| O4 | 是否需要 Dock 左右侧贴边模式 | P4 前 | 会小幅增加 §3.2 与 §4.3 的通用性要求 |

---

## 12. 附录：Win32 API 速查

### 常量

| 名称 | 值 | 用途 |
|---|---|---|
| `WS_EX_NOACTIVATE` | `0x08000000` | 不抢焦点 |
| `WS_EX_TOOLWINDOW` | `0x00000080` | 隐藏于 Alt+Tab |
| `WS_EX_TRANSPARENT` | `0x00000020` | 整窗点击穿透（本方案少用） |
| `WS_EX_LAYERED` | `0x00080000` | 分层 / 透明窗口 |
| `DWMWA_USE_IMMERSIVE_DARK_MODE` | `20` | 深色标题栏 |
| `DWMWA_WINDOW_CORNER_PREFERENCE` | `33` | 圆角偏好（`DWMWCP_DONOTROUND = 1`） |
| `DWMWA_SYSTEMBACKDROP_TYPE` | `38` | 系统背景（`2`=Mica，`3`=Acrylic，`4`=Mica Alt） |
| `DWMWA_CLOAKED` | `14` | 判断 UWP 幽灵窗口 |
| `ACCENT_ENABLE_ACRYLICBLURBEHIND` | `4` | 传统亚克力 |
| `PW_RENDERFULLCONTENT` | `0x00000002` | `PrintWindow` 抓 D3D 内容 |
| `SEE_MASK_NOCLOSEPROCESS` / `SEE_MASK_NOASYNC` | `0x40` / `0x100` | `ShellExecuteEx` 选项 |
| `ERROR_CANCELLED` | `1223` | 用户取消 UAC，**不作为错误上报** |
| `SIIGBF_ICONONLY` / `THUMBNAILONLY` / `BIGGERSIZEOK` | `0x4` / `0x8` / `0x1` | 图标提取选项 |
| `PKEY_AppUserModel_ID` | `{9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5` | 窗口 AUMID |
| `TokenElevation` | `20` | 提权状态查询类型 |
| `HTTRANSPARENT` | `-1` | `WM_NCHITTEST` 穿透返回值 |

### 主要函数

**窗口 / 合成**：`SetWindowLongPtrW`、`SetWindowPos`、`DwmSetWindowAttribute`、`DwmGetWindowAttribute`、`SetWindowCompositionAttribute`、`PrintWindow`

**窗口枚举 / 事件**：`EnumWindows`、`IsWindowVisible`、`GetWindow`、`GetWindowThreadProcessId`、`QueryFullProcessImageNameW`、`SetWinEventHook`、`UnhookWinEvent`

**Shell / 启动**：`ShellExecuteExW`（`lpVerb="runas"` 提权）、`SHGetPropertyStoreForWindow`、`SHCreateItemFromParsingName`、`IShellItemImageFactory::GetImage`、`SHGetKnownFolderPath`

**权限**：`OpenProcess`（`PROCESS_QUERY_LIMITED_INFORMATION`）、`OpenProcessToken`、`GetTokenInformation`（`TokenElevation`）

**显示器 / DPI**：`EnumDisplayMonitors`、`GetMonitorInfoW`（`rcWork`）、`MonitorFromPoint`、`GetDpiForWindow`、`GetCursorPos`

**窗口控制**：`ShowWindow`、`SetForegroundWindow`、`AttachThreadInput`、`PostMessageW`、`SwitchToThisWindow`

**消息 / 事件常量**：`WM_SETTINGCHANGE`、`WM_DISPLAYCHANGE`、`ERROR_CANCELLED`

**建议 crate 特性**：`windows` crate 按需启用 feature，只开 `Win32_UI_WindowsAndMessaging`、`Win32_UI_Shell`、`Win32_Graphics_Dwm`、`Win32_Graphics_Gdi`、`Win32_System_Com`、`Win32_System_Threading`、`Win32_Security`（提权检测）、`Win32_UI_HiDpi`、`Win32_Foundation`，避免编译时间失控。

---


> 这一节是从 README 搬过来的**完整版**（README 只留一句话结论的表）。
> 每条都写清了原因与代价，免得后来者把它们当成 bug 重新查一遍。

## 13. 已知限制（完整版）

| 限制 | 原因 |
|---|---|
| **同进程点击 Dock 会夺焦点** | `WM_MOUSEACTIVATE` 落到无法子类化的深层 Chromium 窗口（comctl32 线程局部，硬限制）。实际防护是「**Dock 窗口**不创建任何会获得焦点的东西」。⚠️ 自检里 `[A]`/`[B]` 两相报红就是这个已知限制，**验收判据是 `[C]` 跨进程相**（真实工况）。注意这条只针对 Dock 窗口 —— **设置窗口是普通可聚焦窗口**，那是应该的（要打字、拖滑块），见 `settings_window.rs` |
| 独占全屏游戏内看不到 Dock | Windows 机制限制，`TOPMOST` 压不过 D3D 独占全屏 |
| **自动隐藏的任务栏**：留空必须问「配置」而不是看「此刻的位置」 | 任务栏自动隐藏时 `rcWork` **恒等于整屏**，但任务栏一触底就会弹出来盖住 Dock。旧写法靠"任务栏窗口顶边是否贴着屏幕底"判断它是不是自动隐藏 —— 任务栏**弹出来的那一瞬间**顶边回到 1020，看着和常显一样，于是判断成"不用留空"，Dock 就永久压在弹出区上（实测 Dock 底 1080、任务栏 1020-1080，鼠标移到 Dock 上等于把任务栏叫出来盖住自己）。现在用 `ABM_GETSTATE` 问「是不是配置成自动隐藏」、用 `ABM_GETTASKBARPOS` 拿吸附矩形的高度 —— 这两个值与弹出/收起无关。修后 Dock 底 1020、任务栏 1020-1080，**零重叠** |
| **Dock 本体不置顶，最大化的窗口会盖住它** | 刻意的：需求是"Dock 只出现在桌面上，其他页面都在它之上"。所以 Dock 是普通窗口（`alwaysOnTop: false`，`layout_dock` / `move_to` 都带 `SWP_NOZORDER`），只有浮层（菜单 / 大文件夹 / 悬停标签）是 `TOPMOST`。实测：普通窗口盖住 Dock 中心点时最上层是那个窗口，关掉后又是 Dock。**若哪天要改**，正确做法是只对最大化窗口特判，而不是整体恢复置顶 |
| **外部程序能把 Dock 设成"总在最前"**（会浮在游戏上面） | `WS_EX_TOPMOST` 是**窗口自己的属性**，任何程序都能用 `SetWindowPos(HWND_TOPMOST)` 或直接改扩展样式给它设上（自动化脚本 / 录屏 / 游戏加速器 / "总在最前"小工具），而且**设上就不会自己掉** —— 表现为"我打开游戏，Dock 却盖在游戏上面"。2026-09 用户实测撞到过一次（来源是自检脚本为了点得到 Dock 临时置顶）。现在在 `dock_subclass_proc` 的 **`WM_WINDOWPOSCHANGING`（补 `SWP_NOZORDER`）/ `WM_STYLECHANGING`（抹掉该位）**里**于生效之前**拦掉：实测外部置顶后 40ms 内确认样式位从未变过（轮询版会暴露最多一秒，游戏里那一眼就够看见）；`reveal::clear_topmost_if_set` 降级为每 5 秒的兜底，正常永不触发。自检 `drive_layering_test` 6 项钉住它。附带影响：**Dock 从此不接受任何 Z 序变更** —— 想用"把它顶起来"的办法跑合成点击自检已失效，跑自检请 `Win+D` 让 Dock 真的露出来。见设计文档 §4.1.2 |
| ~~**debug 版启动会多带一个终端窗口**~~ | **已修**（2026-09）：现在**一直**是 GUI 子系统（`#![windows_subsystem = "windows"]`），双击启动不再有终端窗口。之所以以前要留着控制台，是因为 `println!` 在没有控制台时会**因为写失败而 panic**；现在输出统一走 `logging`（先落文件，控制台镜像写失败一律忽略），所以可以放心去掉。日志位置与崩溃取证见「环境变量 → 日志与崩溃取证」 |
| **做不到"把任务栏背景改透明"（需要注入 explorer）** | Win11 的任务栏背景是 Explorer 里的 **XAML 岛**画的，`SetWindowCompositionAttribute` 只能往上加色 → **只会更实**；DWM 背板那条路也排除了（`DWMWA_SYSTEMBACKDROP_TYPE` 设成 NONE 逐像素无变化）。TranslucentTB 在 Win11 是靠 `SetWindowsHookEx` + `InitializeXamlDiagnosticsEx` **注入 explorer** 改 XAML 画笔（GPL、10 文件 ~25KB C++），与本项目 R10「不注入任何进程」冲突。**要完全透明就用 TranslucentTB / Windhawk**；证据见 `docs/dock-technical-design.md` §4.1.3 |
| 无法操作以管理员身份运行的窗口 | UIPI；改为提供「以管理员身份运行」入口 |
| 自动隐藏用**单窗口**而非双窗口 | 把窗口整个移出屏幕即可不挡点击，不需要第二个触发条窗口 |
| 菜单是**独立窗口**（不是原生菜单） | 原生 `TrackPopupMenu` 外观完全无法定制，且自带模态消息循环卡住调用线程。代价：菜单窗口的**投影会被窗口边界裁掉**（亚克力按整个窗口矩形铺，留不出画阴影的边距），只用内描边区分边界 —— 见 `panel_window.rs` |
| 菜单的「点别处自动消失」靠**轮询**（16ms） | 菜单窗口是 `WS_EX_NOACTIVATE`（必须的：夺焦点会让用户正在打字的窗口失焦），因此收不到失活通知，没有消息可等。看门线程只在菜单打开期间存在 |
| 大文件夹**不支持嵌套**，也不能把图标从文件夹里**拖出来** | 嵌套会让"点开→再点开"变成一条没有尽头的路径；拖出来的落点语义（放回第几位？）没有好答案。要拿出来就用右键「解散文件夹」，或右键里面的图标「移出文件夹」 |
| 「移出文件夹」是**挪到顶层**，不是删掉 | 文案写的是"移出"，那就该是挪出去。想从 Dock 上删掉，移出来之后再右键「移出 Dock」——两步，但不含歧义 |
| 文件夹的「移出 Dock」会**连里面的应用一起删** | 里面的条目不在顶层列表里，只能跟着文件夹走。所以文案写明了项数（「移出 Dock（含里面 3 项）」），不做静默的数据损失 |
| 文件夹的图标预览只画**前 4 个** | 预览就那么大（2×2），多取是白花时间。里面超过 4 个时预览不代表全部 |
| Dock 上的分割线是**纯视觉**分组 | 它不参与「显示哪些应用」的任何逻辑，也不做智能排序（不会自动把同类应用分到一组）—— 位置完全由用户拖放决定 |
| 回收站的图标**不会**随空 / 满自动变化 | 图标带缓存（同一个 target 只取一次）。要跟随状态变化得定期查 `SHQueryRecycleBinW` 的 `dwNumItems` 并作废缓存，见 backlog |
| 手工按 stub 路径固定会「对不上」 | `System32\notepad.exe` 是应用执行别名，真实进程在 `WindowsApps` 下；**通过右键固定不会踩这个坑** |
| 少数商店应用快捷方式解析不出目标 | 有些 MSIX 别名的 `.lnk` 既没有文件路径、IDList 也拿不到可解析的名字；此时会**明确提示**改用托盘「添加应用…」 |
| 拖拽时被拖的图标超出面板会被窗口裁掉 | 窗口只有 72 逻辑像素高。macOS 能让图标浮在 Dock 外，我们做不到 —— 除非拖拽期间加高窗口，而那会让透明区域**吞掉桌面点击**（窗口透明 ≠ 点击穿透）。被裁掉这件事本身也成了「要删了」的视觉反馈 |
