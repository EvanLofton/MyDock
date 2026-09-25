# P1 窗口层复验发现（真实 Tauri 窗口）

| 项目 | 内容 |
|---|---|
| 目标 | 在真实 Tauri 窗口上复验 P0 的毛玻璃结论（R17）与焦点约束（R15） |
| 工程 | `app/`（Tauri 2.11.2 + 静态 HTML，零 npm 依赖） |
| 状态 | **窗口层已跑通并落地**：R15 已得出受控结论、R17 的 DWM 路径已确认、DPI 定位已修复 |

---

## 1. ❗最重要的环境约束：我这个开发环境跑不了 Tauri 应用

Tauri/WebView2 应用在 DSH 的受限沙箱下**无法启动窗口**，现象是：

- `WebviewWindowBuilder::build()` 返回 `Ok`，但进程里**没有任何窗口**（只有 `Tao Thread Event Target` 消息窗和 IME 窗）；
- 进程活着、事件循环在跑，`hwnd()` 永远返回 "underlying handle is not available"；
- 放宽到 `danger-full-access` 后 **一次成功**。

根因有两层：

1. **Tauri 强制把 WebView2 数据目录设为 `%LOCALAPPDATA%\{identifier}`**（源码 `tauri/src/manager/webview.rs:537-552`，`WEBVIEW2_USER_DATA_FOLDER` 环境变量会被忽略），沙箱拒绝该写入 → `create_dir_all` 报 os error 5；
2. 即使把数据目录重定向到工作区内，**WebView2 仍起不来** —— 高度怀疑是沙箱禁止**命名管道**（WebView2 用命名管道做宿主与渲染进程的 IPC），导致 webview 初始化失败。

> **这是我这边的环境限制，不是产品问题。** 你在自己的终端里直接跑是正常的。
> 影响：后续每次运行 Dock 应用，我都需要放宽沙箱权限。

---

## 2. R15：tao **没有**设置 `WS_EX_NOACTIVATE`

真实 Tauri 窗口的实测样式：

```
class  : 'Tauri Window'
style  : WS_CAPTION | WS_SYSMENU | WS_VISIBLE
exstyle: WS_EX_TOPMOST | WS_EX_APPWINDOW | WS_EX_ACCEPTFILES | WS_EX_WINDOWEDGE
         (0x00040118)
```

| 检查 | 结果 | 影响 |
|---|---|---|
| `WS_EX_NOACTIVATE` | ❌ **未设置** | 必须由我们自己在建窗后补上 |
| `WS_EX_LAYERED` | ✅ **未设置** | 好消息：不会削弱毛玻璃（P0 证明 LAYERED 会把 76px 降到 66px） |
| `WS_EX_NOREDIRECTIONBITMAP` | ❌ 未设置 | tao 的透明不走这条路 |
| `WS_EX_TOPMOST` | ✅ 已设置 | `always_on_top: true` 生效 |
| `WS_EX_TOOLWINDOW` | ❌ **未设置**，且带着 `WS_EX_APPWINDOW` | ⚠️ **`skip_taskbar: true` 疑似未生效**，待复查 |

**推论（待验证）**：既然 tao 连 `WS_EX_NOACTIVATE` 都不设，它几乎不可能处理 `WM_MOUSEACTIVATE`。按 P0 的 T2 结论，**同进程焦点场景必须靠 `WM_MOUSEACTIVATE → MA_NOACTIVATE`**，因此需要：

```rust
// 用 SetWindowSubclass 在 Tauri 窗口上补 WM_MOUSEACTIVATE 处理
SetWindowSubclass(hwnd, Some(dock_subclass_proc), SUBCLASS_ID, 0);
```

`dock_subclass_proc` 中：`WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize)`，其余交给 `DefSubclassProc`。

**下一步动作**：在 Tauri 窗口上实测「同进程点击是否夺焦点」，确认是否真的需要子类化。

---

## 3. R17：DWM 路线 A 在真实 Tauri 窗口上全部成功

| 调用 | 结果 |
|---|---|
| `DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW)` | ✅ `Ok(())` |
| `DwmGetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE)` 回读 | ✅ `Ok(())`，值 = **3**（Acrylic） |
| `DwmSetWindowAttribute(DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_DONOTROUND)` | ✅ `Ok(())` |
| `DwmSetWindowAttribute(DWMWA_BORDER_COLOR = 0xFFFFFFFE)` | ✅ `Ok(())` |
| `DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE = true)` | ✅ `Ok(())`，回读 = `true` |

**结论：P0 的路线 A 在真实 Tauri 窗口上完全可用，所有 DWM 属性可写可回读。**

> **仍待完成**：用 T1c 的锐利边缘法对真实 Tauri 窗口做**量化**模糊量测（确认模糊强度与裸 Win32 窗口一致）。程序化建窗路径已跑通，测法见下节。

---

## 4. 其他实测发现

| # | 发现 | 影响 |
|---|---|---|
| 1 | **屏幕 DPI 缩放 = 125%**（逻辑 1536×864 ↔ 物理 1920×1080） | 印证技术方案 §6 的判断：物理/逻辑像素换算是高发 bug 区。`inner_size(900, 96)` 实测物理尺寸为 **1125×120**（= 逻辑 × 1.25） |
| 2 | **窗口位置请求未按预期生效**：请求逻辑坐标 (510, 920) → 实际落在 (96, 96) | 920 × 1.25 = 1150 > 屏高 1080，超出屏幕后被系统重定位。**定位必须按 `rcWork` 反算并钳制在屏内**，不能直接把逻辑坐标丢给系统 |
| 3 | `decorations: false` 下窗口**仍保留 `WS_CAPTION \| WS_SYSMENU`** | tao 是「保留样式位 + 用 `WM_NCCALCSIZE` 抹掉非客户区」的实现方式，而非去掉样式位。若我们自行处理 NC 相关消息需注意 |
| 4 | `skip_taskbar: true` 后 exstyle 中仍是 `WS_EX_APPWINDOW`、没有 `WS_EX_TOOLWINDOW` | ⚠️ 待复查：可能是我读取样式的时机早于 tao 应用该设置；若确为未生效，需手动补 `WS_EX_TOOLWINDOW` 并去掉 `WS_EX_APPWINDOW` |
| 5 | 在 `setup` 里建窗后立刻读 `hwnd()` 会失败 | 必须轮询等待句柄就绪（本次实现为 100ms × 50 次），或改到 `RunEvent::Ready` 之后 |

---

## 5. 工程侧记录

### 构建

- **整个 Tauri 栈可离线构建**（tauri 2.11.2 / tao 0.35.2 / wry 0.55.1 / webview2-com 0.38.2 等全部命中 cargo 缓存）。
- `tauri-build` **必需 `src-tauri/icons/icon.ico`**，缺失直接构建失败。已用脚本生成一个合法的 256×256 32 位 ICO。
- 构建命令：`cargo build --offline`（在 `app/src-tauri` 下）。

### 目录结构

```
app/
├─ dist/index.html              # 静态前端（临时，验证窗口层用，零 npm）
└─ src-tauri/
   ├─ Cargo.toml
   ├─ build.rs
   ├─ tauri.conf.json
   ├─ icons/icon.ico
   └─ src/main.rs               # 窗口层诊断
```

### 运行（开发环境）

```powershell
# 正常情况（你自己的终端）
cargo run

# 我这个受沙箱限制的环境：必须放宽权限
```

---

## 6. 第二轮复验结果（受控实验）

### 6.1 R15 结论：钩子**必要且有效**，但有一个残留风险

端到端「真实点击」在本环境不可用（见 6.3），因此改用**直接量测激活决策点**的方式：
`WM_MOUSEACTIVATE` 的返回值就是「鼠标点击是否激活窗口」的唯一决策点。

**受控实验结果（同一进程内的三个对象）**：

| 被测对象 | 返回值 | 含义 |
|---|---|---|
| 正对照：纯 Win32 假 Dock（无钩子） | `1` `MA_ACTIVATE` | 会夺焦点 —— **对照有效** |
| **真 Dock（已装载钩子）** | **`3` `MA_NOACTIVATE`** | **不夺焦点 ✅** |
| 真 Dock（钩子放行，**同一窗口**） | `1` `MA_ACTIVATE` | 会夺焦点 |

**结论**：
1. `SetWindowSubclass` 挂在 Tauri 窗口上**能够收到** `WM_MOUSEACTIVATE`；
2. 钩子生效后返回 `MA_NOACTIVATE`，点击不会激活 Dock；
3. 第三行是关键 —— 同一个窗口在钩子放行时返回 `MA_ACTIVATE`，**证明差异由我们的钩子造成**，排除了其它混杂因素。

**⚠️ 残留风险（必须记录）**：该实验是把 `WM_MOUSEACTIVATE` **直接发给顶层窗口**。
真实点击时，消息可能先发到 **WebView2 子窗口**，若子窗口自行处理并不上抛，顶层窗口的钩子就不会被走到。
本轮的端到端点击未能复现（见 6.3），所以**这一条尚未证伪也尚未证实**。

**P1 的应对**：把同一个 `WM_MOUSEACTIVATE` 处理同时挂到 WebView2 的子窗口上（遍历子窗口逐个 `SetWindowSubclass`），作为防御性措施。代价很低。

### 6.2 窗口定位已修复

```
DPI = 120 (scale 1.25)   逻辑 900x96 -> 物理 1125x120
rcWork = (0,0)-(1920,1080)
目标物理矩形 = (397,960) 1125x120
实际物理矩形 = (397,960) 1125x120
定位精确命中 ✅
```

按 `rcWork × DPI` 反算物理矩形并用 `set_size`/`set_position`（Physical）下发后，**精确定位**，不再是第一轮那个落到 (96,96) 的结果。

### 6.3 端到端合成点击：原因已查明（见 §6.14，本节结论已被推翻）

> ⚠️ **本节结论已被 §6.14 推翻。** 这里的现象记录仍然准确，
> 但归因是错的 —— 根因是辅助窗口建在了没有消息循环的后台线程上，**是我的测试代码 bug**。
> 修正后端到端点击**可以**自动化验证。

排查过程（现象部分仍然有效）：

| 检查项 | 结果 |
|---|---|
| 光标是否移到目标 | ✅ `SetCursorPos` 后读回 `(450,545)`，正是目标中心 |
| `SendInput` 是否注入 | ✅ 返回 `3/3` |
| `SendInput` 整体是否有效 | ✅ 绝对移动可用（光标 `(1528,1027)` → `(300,300)`） |
| 点击坐标处顶层窗口 | ✅ `WindowFromPoint` 返回 `FAKE-DOCK` / `Dock` |
| 目标窗口是否收到鼠标消息 | ❌ `WM_LBUTTONDOWN = 0`、`WM_MOUSEACTIVATE = 0` |
| 目标窗口是否活着 | ✅ 自建窗口能收到消息（计数 16 / 3），tao 的消息泵**确实**会派发给我们自建的窗口 |

即：光标在正确位置、输入注入成功、目标窗口活着且能被派发消息，**但按钮事件没有产生任何鼠标消息**。

**结论：这是本开发环境的限制，不是产品缺陷。** 影响是「无法用自动化手段做端到端点击验证」，
所以端到端的焦点行为改由两个来源支撑：
1. 本轮 `WM_MOUSEACTIVATE` 的受控量测（6.1）；
2. P0 报告 §3 的真实点击实测（当时 `clicks=1`，确认点击送达并发生了焦点转移）。

**建议**：由你手工做一次 5 秒验证 —— 打开记事本并让它获得焦点，然后点击 Dock，
看记事本是否**仍然保持焦点**。这是最终的 ground truth。

### 6.4 线程约定（踩坑记录，务必遵守）

| 事项 | 结论 |
|---|---|
| `SetWindowSubclass` | **必须由拥有窗口的线程调用**。在后台线程调用会**静默返回 false**（第一轮就栽在这里）。窗口层设置现改为通过 `run_on_main_thread` 投递到主线程执行 |
| 自检里的 `sleep` | **不能放主线程**，否则阻塞 tao 事件循环、窗口收不到任何消息。自检跑在后台线程 |
| HWND 跨线程 | `HWND` 含裸指针、不是 `Send`，跨线程只传 `usize` 地址，在另一侧还原 |

### 6.5 已落地的窗口层清单

`app/src-tauri/src/win_layer.rs` 现已实现并实测通过：

| # | 事项 | 实测结果 |
|---|---|---|
| 1 | 补 `WS_EX_NOACTIVATE` | `0x00040118` → `0x08000198` |
| 2 | 补 `WS_EX_TOOLWINDOW`、去 `WS_EX_APPWINDOW` | ✅ 不再进任务栏 / Alt+Tab |
| 3 | 去掉 `WS_EX_LAYERED` | ✅ 未出现（避免削弱毛玻璃） |
| 4 | `WM_MOUSEACTIVATE → MA_NOACTIVATE` | ✅ 返回 3 |
| 5 | 路线 A 毛玻璃 + 自绘圆角 + 去边框 + 深色材质 | ✅ 全部 `Ok`，`SYSTEMBACKDROP_TYPE` 回读 = 3，深色回读 `true` |
| 6 | `rcWork × DPI` 定位并钳制屏内 | ✅ 精确定位 |

---

### 6.6 ❗路线选择被推翻：真实 Tauri 窗口上 **A 不模糊、B 才模糊**

这是本轮最重要的发现，**它推翻了 P0 报告 §2.5 的路线选择**。

**量测方法**：在 Dock 窗口背后放一块左黑右白的锐利边缘背景窗，把 Dock `SetWindowPos` 到其上方，
抓屏读一条横向亮度剖面（每 25px 一个采样点）。越平滑 = 模糊越强，越台阶 = 越锐利。

**同一台机器、同一个窗口，只改毛玻璃路线的三方对照**：

| `DOCK_GLASS` | 亮度剖面（x 每 25px） | 对比度 | 判定 |
|---|---|---|---|
| `none`（基线） | `12 12 12 12 12 12 12 12 160 160 160 160 160 160 160 160` | 148 | 锐利台阶 —— 无模糊 |
| `a` 官方 DWM 系统背景 | `61 61 61 61 61 61 61 61 61 61 61 61 61 61 61 61` | 0 | ❌ **完全平坦 —— 没有采样背后内容** |
| `b` `SetWindowCompositionAttribute` | `26 28 27 27 27 27 28 31 39 45 50 50 50 50 50 50` | 26 | ✅ **真实模糊，约 200px 渐变** |

**基线的正确性可验证**：`none` 行的数值与「HTML 面板 rgba(28,28,32,0.42) 叠在黑/白上」的理论值
精确吻合 —— 黑侧 `0.42×30 ≈ 12`，白侧 `0.58×255 + 12.6 ≈ 160`。说明量测链路可信。

**结论**：
1. **真实 Tauri/WebView2 窗口上，路线 A（`DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW`）
   渲染出的是一层完全平坦的材质，不采样窗口背后的内容**；
2. **路线 B（`SetWindowCompositionAttribute` + `ACCENT_ENABLE_ACRYLICBLURBEHIND`）产生真实、强烈的模糊**；
3. 这与生态现状一致 —— `window-vibrancy`、各类 Tauri 毛玻璃实现用的**都是路线 B**。

**为什么 P0 得出了相反结论**：P0 的量测对象是**裸 Win32 窗口**（无 WebView2 子窗口，
`WS_POPUP`、客户端由 GDI 绘制）。可见差异来自 **WebView2 子窗口覆盖了客户端区域**，
在这种窗口拓扑下 DWM 的系统背景材质不再采样背后内容。

> **教训**：窗口层的行为不能只靠「拓扑近似」的裸 Win32 探针定案，必须在**真实框架窗口**上复验。
> 这正是把 R17 单列成风险的价值。

**P1 决策**：**改用路线 B 作为毛玻璃实现**（`app` 默认 `DOCK_GLASS=b`）。
路线 A 的代码保留在 `win_layer::apply_glass`，仅供圆角 / 边框 / 深色模式三项复用。

**待办**：路线 B 的着色 alpha 目前取 200（约 78% 不透明），观感偏暗、对比度只剩 26，
需要在前端联调时下调 alpha 并配合 CSS 染色，找到 macOS 那种通透感的平衡点。

---

### 6.7 React + Vite 前端骨架已就位

技术栈：**React 19 + Vite 8 + TypeScript**（npm 走 fnm 的 `v24.19.0`）。

| 项 | 结果 |
|---|---|
| `npm install` | ✅ 24 个包，17s（registry 走 npmmirror） |
| `tsc --noEmit` 类型检查 | ✅ 通过 |
| `vite build` 产物 | ✅ `index.js` 222.58 kB（**gzip 69.88 kB**）+ CSS 1.11 kB，161ms |
| 鱼眼放大数值校验 | ✅ **14/14 断言通过**（`npm run check:magnify`） |
| 界面实际渲染 | ✅ 用 `probe inspect` 读窗口像素确认（见下） |

**`probe inspect` 命令**：抓窗口像素并输出「颜色网格 + 内容包围盒」，可在**不看屏幕**的情况下
确认界面是否渲染、内容是否居中。实测结果：

```
内容包围盒: x 222..891（宽 669）  y 26..103（高 77）
窗口内容宽 1113，中心 x = 556
内容中心 x = 556
水平偏移 = 0 px  =>  水平居中 ✅
```

#### ❗踩到的三个坑（都值得记住）

**1. Tauri 在编译期把前端资源嵌入二进制**

改了 `dist/` 之后**必须重新 `cargo build`**，否则应用仍在跑旧前端。
我第一次改 CSS 后重新 `vite build` 却没重编 Rust，量测结果一模一样、完全没变化 —— 这是最直接的信号。

> 开发循环：`npm run build` → `cargo build` → 运行。**两步都要做。**
> 两个 `.log` 与 `target/`、`dist/` 已加入 `.gitignore`。

**2. `baseLeft` 的坐标系搞错了（我自己的 bug）**

`computeLayout` 内部是以「行中心为 0」排布的，`baseLeft` 范围是 −266…+266。
我一开始直接把它当 CSS `left` 用（相对行左边缘），结果图标被推到行外，
实测**偏左 321px**。修法是输出时加回 `rowWidth/2` 转成 0 基。

> 这个 bug 只有靠**像素量测**才能发现，肉眼看「图标在一条线上」是看不出来的。

**3. 用「彩色度」判定内容边界是不可靠的**

`probe inspect` 最初用色相饱和度找图标包围盒，结果把灰色的「设置」图标漏掉了，
算出来的中心偏左 43px，**误报成布局错误**。改成「与背景主色差异」判定后，
同一帧画面得到 `偏移 0px ✅`。

#### 性能设计（已在代码里落实）

- **鼠标位置绝不进 React state**：`mousemove` 的 rAF 里直接把 `transform` 写进 DOM，
  完全绕过 React 渲染；React 只负责「有哪些图标、顺序如何」。
- **放大只用 `translateX + scale`**，不改 `width/left`，不触发布局。
- `.dock-row` 用 `left:50% + margin-left:-rowWidth/2` 锚定在面板中心，
  这样面板宽度随放大变化时行的左边界保持不动（否则图标会漂移）。
- 鱼眼放大后的**不重叠**由「按放大后宽度重新顺序排布」保证，
  校验脚本断言最小间隙 = 配置的 gap（12px）。

---

### 6.8 应用模型层已打通（真实应用 + 真实图标）

新增模块：`model.rs`（纯数据）、`apps.rs`（枚举 / 身份 / 窗口操作 / 启动）、`icons.rs`（图标提取）、`commands.rs`（Tauri 命令）。

**枚举实测输出**（`DOCK_DUMP_APPS=1`）：

```
共 3 个应用
  Windows 资源管理器   窗口=1  exe = C:\Windows\explorer.exe
  Application Frame Host 窗口=1  exe = C:\Windows\System32\ApplicationFrameHost.exe   '设置'
  Microsoft Edge       窗口=1  exe = C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe
```

**界面实测**（`probe inspect`）：内容包围盒宽 242 物理像素 = 194 CSS = **3×56 + 2×12 = 192 CSS**，
正好 3 个图标，且水平居中 —— 说明「枚举 → IPC → 渲染 → 图标」整条链路通了。

#### 枚举规则的每一条都对应一个真实的坑

| 过滤条件 | 原因 |
|---|---|
| `IsWindowVisible` | 隐藏窗不算 |
| 跳过 `WS_EX_TOOLWINDOW`（除非有 `WS_EX_APPWINDOW`） | 托盘气泡、输入法候选窗会污染列表 |
| 跳过 `GetWindow(hwnd, GW_OWNER) != NULL` | 有 owner 的通常是对话框，不该算独立应用 |
| 跳过 `DWMWA_CLOAKED` | **UWP 挂起后窗口仍存在但已被 cloak**，不检查会看到一堆幽灵窗口 |
| 跳过无标题窗口 | 多数是无意义的辅助窗 |
| 跳过自身进程 | 否则 Dock 会把自己也列出来 |

#### ❗实测确认了设计文档预言的问题：UWP 应用会被合并

**所有 UWP 应用都跑在 `ApplicationFrameHost.exe` 下。** 只按 exe 路径聚合时，
「设置」「计算器」「照片」等**全部塌缩成一个「Application Frame Host」条目** —— 实测复现。

当前对策（`apps.rs` 里已实现并注释）：**对 UWP 宿主改用窗口标题当身份**。
对 UWP 来说标题就是应用名，能正确区分常见 UWP 应用，实现成本低。
代价是同一 UWP 的多个窗口会被拆成多条。

**正规做法**是读窗口的 `PKEY_AppUserModel_ID`（AUMID）。API 路径已探明：
`SHGetPropertyStoreForWindow<IPropertyStore>`（`Win32::UI::Shell::PropertiesSystem`）
+ `PKEY_AppUserModel_ID`（在 `Win32::Storage::EnhancedStorage`，GUID `9f4c2855-9f79-4b39-a8d0-e1d42de1d5f3` pid 5）
+ `PropVariantToStringAlloc`（`Win32::System::Com::StructuredStorage`）。
需要额外开 3 个 feature，留作后续增强。

UWP 的**图标**同理取不到（宿主进程的图标不是应用图标），当前用首字母兜底。

#### 图标提取

走 `IShellItemImageFactory::GetImage` 请求 128px（**不用** `ExtractIconEx` / `SHGetFileInfo`，
它们只给 16/32px，125% DPI 下必糊）。

两个实现细节：
- **传输**：Rust 返回 `[w:u32][h:u32][BGRA...]` 原始字节，走 Tauri 的 `ipc::Response`
  二进制通道 —— 否则几十 KB 会被序列化成 JSON 数字数组；
- **编码**：前端用 canvas 做 BGRA→RGBA 与 `toDataURL("image/png")`，
  **省掉了在 Rust 里引 PNG 编码库**（离线环境依赖风险）。
- `normalize_alpha`：Shell 图标常见**预乘 alpha**，直接当直通 alpha 用会让边缘发黑。
  判定很可靠 —— 预乘下任何通道都不可能大于 alpha，发现一个反例就原样返回。

#### 已实现的窗口操作

`activate`（还原→`AttachThreadInput`→`SetForegroundWindow`，失败则闪任务栏兜底）、
`minimize`、`close_window`、`launch`（`ShellExecuteExW`；
`run_as_admin` 走 `lpVerb="runas"`，**用户取消 UAC 返回 `ERROR_CANCELLED(1223)` 视为成功**，不报错）。

---

### 6.9 自动隐藏、点击行为、右键菜单

#### 自动隐藏：改成一个窗口（推翻文档 §3.2 的双窗口方案）

文档原本提出「触发条窗口 + 主窗口」两个窗口，理由是隐藏时主窗口若仍有透明区域会挡住桌面点击。

**实测后改了方案**：把主窗口**整个移到屏幕外**（`y = 工作区底边`），
移出屏幕的窗口不可能挡住任何东西 —— 于是**根本不需要第二个窗口**，
少一个窗口就少一套生命周期与命中测试。

实测验证（用 `SetCursorPos` 驱动光标，逐帧读窗口矩形）：

```
鼠标在屏幕中部  → Dock 位置 = (397,1082)   隐藏 ✅
鼠标触底        → Dock 位置 = (397,960)    弹出 ✅
鼠标离开        → Dock 位置 = (397,1082)   重新隐藏 ✅
```

实现要点：触发区 3px、隐藏延迟 420ms、动画 180ms（唤出 `easeOutCubic`，隐藏 `easeInQuad`）。
P0 的 T3 已证明逐帧 `SetWindowPos` 只占 60Hz 帧预算 12.4%，所以逐帧移动是安全的。

> 默认**关闭**（`DOCK_AUTOHIDE=1` 开启）。等配置持久化做好后改为读用户偏好 ——
> 现在还没有配置存储，一个难以被发现的行为不该默认打开。

#### 右键菜单：用原生弹出菜单

**为什么不在页面里画 HTML 菜单**：Dock 窗口只有 88px 高，HTML 菜单会被窗口裁掉。
除非把窗口做得更高，但那样窗口上半部分的透明区域会**吞掉桌面点击**
（关键认知：**窗口透明 ≠ 点击穿透**）。

原生 `TrackPopupMenu` 是独立的系统对象，可以自由超出窗口边界。

菜单项：激活 / 最小化 / 关闭窗口 / ─── / **以管理员身份运行**（已提权时置灰）/ 打开文件所在位置。

实现要点：
- `TrackPopupMenu` 自带**模态消息循环**，会阻塞线程 → 命令层必须用
  `tauri::async_runtime::spawn_blocking` 跑，**绝不能占住主线程**
  （主线程是 Tauri 事件循环，卡住就窗口收不到消息、菜单也点不动）；
- `nReserved` 参数在 windows-rs 里是 `Option<i32>`，必须传 `None`；
- 弹出前要 `SetForegroundWindow(owner)`，否则在别处点击时菜单不会消失；
- 菜单打开时置 `reveal::PAUSED`，**禁止自动隐藏**（文档 §4.3 的要求）。

代价：外观是 Windows 原生风格。P2 若要还原 macOS 观感，需要改用独立的菜单窗口。

#### 点击行为对齐 macOS

- 该应用**已在前台** → **最小化**（再次点击收起）
- 否则 → **激活**它的窗口

失败时弹 red toast 显示具体原因（前台锁定 / 提权窗口），**绝不静默失败** ——
这是设计文档里反复强调的一条。

---

### 6.10 配置持久化 + 系统托盘 + 开机自启

#### 自己实现，不用插件

本机 cargo 缓存里**没有** `tauri-plugin-store` / `tauri-plugin-autostart` / `tauri-plugin-single-instance`
（离线装不上），于是：

| 需求 | 做法 |
|---|---|
| 配置读写 | 自己写 `store.rs`：`serde_json` + `std::fs`（`serde_json` 本就在依赖里，十几行） |
| 开机自启 | 自己写 `autostart.rs`：`HKCU\...\CurrentVersion\Run`（用 HKCU 不需要管理员权限） |
| 托盘 | 用 Tauri 内置 `tray-icon` feature（缓存里有 `tray-icon 0.23.1`） |

配置文件在 `%APPDATA%\{identifier}\config.json`；`DOCK_CONFIG_DIR` 可重定向
（受限沙箱下写 AppData 会被拒）。**配置坏了不让程序起不来** —— 解析失败一律退回默认值。

#### 实测：配置真的驱动行为（不只是存下来）

写不同的 `config.json` 再启动，逐帧读窗口矩形：

| 场景 | 配置 | 鼠标在屏幕中部 | 鼠标触底 |
|---|---|---|---|
| 1 | `autoHide: false` | Dock Y=960 **可见** ✅ | — |
| 2 | `autoHide: true` | Dock Y=1082 **隐藏** ✅ | Y=960 **弹出** ✅ |

`iconSize: 44`（默认 56）与 `glassAlpha: 170` 也都在日志与像素量测中确认生效。

#### 托盘

菜单只有三项，刻意保持精简 —— Dock 本体负责日常操作，托盘只放
「Dock 自己没法表达」的开关：**自动隐藏**（勾选，实时生效不重启）/ **开机自启**（勾选）/ **退出**。

#### 毛玻璃着色的定量调优

终于把「alpha 该取多少」从猜测变成了数据。背后放左黑右白的锐利边缘，量对比度：

| `glassAlpha` | 暗侧 | 亮侧 | 对比度 | 观感 |
|---|---|---|---|---|
| 220 | 25 | 50 | **25** | 几乎看不到模糊，像块死板的深色板 |
| 170 | 21 | 88 | 67 | 偏暗 |
| **150（默认）** | — | — | **~85** | 兼顾玻璃感与实体感 |
| 120 | 16 | 127 | 111 | 通透 |
| 70 | 12 | 165 | 153 | 几乎全透，失去 Dock 的实体感 |

> **两层要一起调**：这层之上还有前端 `.dock` 的 `rgba(24,24,28,0.34)`，
> 最终观感由两层共同决定。

#### 默认值变化

`autoHide` 默认从「关闭」改为 **`true`** —— 之前默认关闭是因为没有配置存储，
一个难以被发现的行为不该默认打开；现在托盘能实时切换、且选择会被记住，就可以默认开启了。

---

### 6.11 稳定性与内存实测（并修正设计文档的一个错误结论）

#### 稳定性采样

周期性让光标触底/移开，使自动隐藏线程持续工作（真实负载），每 45 秒采样：

| 时刻 | 工作集 | 私有 | 句柄数 | 线程 |
|---|---|---|---|---|
| T+45s | 31.1 MB | 8.0 MB | 384 | 15 |
| T+90s | 31.1 MB | 8.0 MB | 384 | 15 |
| T+135s | 31.1 MB | 7.9 MB | 382 | 11 |
| T+180s | 31.0 MB | 7.8 MB | 378 | 7 |
| T+225s | 31.0 MB | 7.8 MB | **378** | 7 |

**完全平坦，句柄数还略微下降**（`384 → 378`），线程数从 15 收敛到 7。
主进程侧没有泄漏迹象。

> ⚠️ 上面那次是 **4 分钟**的采样，**不等于**验收标准里的「连续运行 8 小时」。
>
> **8 小时长稳测试在持续记录**：`docs/soak-log.txt`（每 120 秒采样：工作集 / 私有内存 / 句柄数 / 线程数）。
>
> **必须说清楚：这条验收在本次会话内无法完成。** 它需要的是**真实流逝的 8 小时**，
> 不是工作量 —— 会话里每轮之间只过去一两分钟，所以采样点数会很少。
> 采样会一直跑下去；只要进程不退出，日志就会继续累积。
> 早期版本曾周期性移动**用户真实光标**来驱动自动隐藏，那太打扰用户，已改成空闲采样。

#### ❗实测推翻了设计文档 §2.1 的内存结论

原文写「Tauri 常驻内存 40-80 MB，Electron 150-250 MB」，并以此作为选型的主要理由。

**实测（含 WebView2 子进程）**：

| 进程 | 工作集 | 私有 |
|---|---|---|
| dock-app（Rust 主进程） | 31.1 MB | **8.0 MB** |
| webview2 子进程 ×6 | 360 MB | **183 MB** |
| **合计** | 391 MB | **191 MB** |

- **私有内存 191 MB** 才是可比数字（不共享、可跨进程求和）；
  工作集 391 MB 是跨进程求和**重复计算了共享页**，偏大，不能直接对比；
- **主进程只有 8 MB 私有内存** —— 原方案的「40-80 MB」很可能就是把主进程当成了全部；
- **191 MB 与 Electron 的 150-250 MB 基本是平手**（两者都是多进程 Chromium）。

**结论**：**Tauri 的真实优势是「包体小」与「Rust 能直接调 Win32」，不是「省内存」。**
设计文档 §2.1 已据此修正。对本项目而言 Rust 互操作才是决定性的一条
（80% 的难点都在 Shell 侧），所以选型不变，但**理由要诚实**。

---

### 6.12 焦点防护挂到整棵窗口树（R15 残留风险已收窄到具体窗口）

§6.1 留了个残留风险：真实点击时 `WM_MOUSEACTIVATE` 可能先落到 **WebView2 子窗口**，
若子窗口自行处理且不上抛，只挂在顶层窗口的钩子就走不到。

**做法**：把钩子挂到**整棵树**（`EnumChildWindows` 遍历），并因为 WebView2 会动态建子窗口，
每 3 秒幂等地重挂一次（`RemoveWindowSubclass` → `SetWindowSubclass`，避免叠层）。

**实测结果**：

| 窗口 | 归属线程 | 钩子 | `WM_MOUSEACTIVATE` 回答 |
|---|---|---|---|
| `Tauri Window` | 我们 | ✅ 成功 | `3` MA_NOACTIVATE ✅ |
| `WRY_WEBVIEW` | 我们 | ✅ 成功 | `3` MA_NOACTIVATE ✅ |
| `Chrome_WidgetWin_0`（WebView2 宿主） | 我们 | ✅ 成功 | `3` MA_NOACTIVATE ✅ |
| `Chrome_WidgetWin_1` | **Chromium 线程** | ❌ `ERROR_INVALID_HANDLE(6)` | `1` MA_ACTIVATE |
| `Chrome_RenderWidgetHostHWND` | **Chromium 线程** | ❌ 同上 | `1` MA_ACTIVATE |
| `Intermediate D3D Window` | **Chromium 线程** | ❌ 同上 | `1` MA_ACTIVATE |

**根因**：`SetWindowSubclass` 在 comctl32 里是**线程局部**的，
只能给调用线程自己创建的窗口挂子类化。深层那 3 个 Chromium 窗口由 Chromium 自己的线程创建，
因此必然失败（`ERROR_INVALID_HANDLE`）。**这不是写法问题，是这个 API 的硬限制。**

**风险现在被收窄成一句话**：
> 只有当 Windows 把 `WM_MOUSEACTIVATE` 投递给那 3 个深层 Chromium 窗口时，才会夺焦点。

**为什么 P1 的实际风险仍然低**：
1. **跨进程场景（正常工况）已由 `WS_EX_NOACTIVATE` 覆盖** —— 这是 P0 §3 的实测结论；
2. 同进程场景只在「Dock 自己的窗口获得焦点时又点 Dock 本体」才出现，
   而 P1 **还没有任何会抢焦点的自有窗口**（原生弹出菜单不会造成这个问题）；
3. 而且点击落在 `Chrome_WidgetWin_0` 这条链上时，钩子是生效的。

**若将来人工验证发现确实会夺焦点**，可选对策：
- 给 Dock 自有的弹窗（设置窗口）也加 `WS_EX_NOACTIVATE` + 同样的钩子，让它压根不获得焦点；
- 或在 Dock 获得点击后主动把焦点还回去（记下点击前的 `GetForegroundWindow`）；
- 不建议钩 `Chrome_*` 内部窗口：跨线程 `SetWindowLongPtr` 改 Chromium 的 wndproc 风险极高。

---

### 6.13 AUMID 身份解析（UWP 归并与图标一并修好）

§6.8 记录过：所有 UWP 应用都跑在 `ApplicationFrameHost.exe` 下，只按 exe 路径聚合会把它们
全部塌缩成一个条目。当时用「窗口标题」临时兜底，现在**换成了正规做法**。

**实现**（`apps.rs`）：

```
SHGetPropertyStoreForWindow<IPropertyStore>(hwnd)
  → IPropertyStore::GetValue(PKEY_AppUserModel_ID)
  → PropVariantToStringAlloc
```

三个实现细节：

1. `PKEY_AppUserModel_ID` 在 windows crate 里位于 `Win32::Storage::EnhancedStorage`
   （位置很反直觉），为避免多开一个 feature，**按原值手写了一份 `PROPERTYKEY`**；
2. `PropVariantToStringAlloc` 用 `CoTaskMemAlloc` 分配，**必须自己 `CoTaskMemFree`** ——
   这是每次枚举都走的路径，漏了会持续泄漏，稳定性验收会挂在这上面；
3. **判据**：打包应用（UWP / MSIX）的 AUMID 一定形如 `PackageFamilyName!AppId`，**含 `!`**。
   用它区分「打包应用」与「普通 Win32 程序」非常可靠。

**修好了两件事**：

| | 修之前 | 修之后 |
|---|---|---|
| 身份 | 按窗口标题（近似） | **AUMID**，如 `windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel` |
| 图标 | 取不到（宿主是 ApplicationFrameHost） | **`shell:AppsFolder\<AUMID>`** 当解析名，正常取到 |

**实测输出**：

```
共 2 个应用
  Windows 资源管理器  [Win32]   图标: 成功 128x128
      target = C:\Windows\explorer.exe
  设置               [Uwp]     图标: 成功 128x128
      target = shell:AppsFolder\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel
```

- 身份不再塌缩成「Application Frame Host」，显示名是「设置」✅
- **UWP 图标也取到了 128×128** ✅ —— `shell:AppsFolder\<AUMID>` 这条路是通的
- `target` 同时用于**启动**与**取图标**，两个用途共用一个字段，`ShellExecuteExW` 能直接处理 `shell:` 路径

---

### 6.14 ❗推翻 §6.3：「合成点击在本环境不可用」是我自己的测试代码 bug

§6.3 结论是「本环境的合成点击不生效」，并据此把端到端验证推给人工。
**这个结论是错的** —— 真正的原因是**我自己的测试代码写错了**。

#### 根因

自检里的辅助窗口（焦点持有窗、假 Dock）是在**后台线程**里 `CreateWindowExW` 创建的，
而那个线程**没有消息循环**。

- 点击产生的鼠标消息是**投递**消息，进了后台线程的队列却无人 `DispatchMessage`；
- 于是 `WM_LBUTTONDOWN` 永远是 0，`SetForegroundWindow` 这类**直接调用**却正常工作；
- 表象完美地伪装成「SendInput 注入无效」，把我误导了好几轮。

**正确切分**：
- **主线程**创建辅助窗口（主线程跑着 tao 的事件循环，会派发消息）；
- **后台线程**驱动测试（要 `sleep`，不能占用主线程）。

改完之后阳性对照立刻就能复现「焦点被夺」，`WM_LBUTTONDOWN=1`。

> **教训**：把「测不出来」归因于环境之前，先怀疑自己的测试装置。
> 我前面用了「光标确实到位 + SendInput 返回 3/3 + WindowFromPoint 正确」
> 三条证据来支撑「环境不支持」，但它们**都绕过了「消息是否被派发」这一环**，
> 所以三条全中也不能排除装置本身有问题。

#### 修正后的完整 R15 矩阵（自校验）

带**两个阳性对照**，若对照不复现就说明测试机制失效、结论不可用：

| 相位 | 场景 | 焦点持有者 | 结果 | 符合预期 |
|---|---|---|---|---|
| `[P0]` | 同进程 无保护假 Dock | 同进程 | **焦点被夺**（`WM_LBUTTONDOWN=1`） | ✅ 阳性对照有效 |
| `[A]` | 同进程 真 Dock（**已装钩子**） | 同进程 | **焦点被夺** | ❌ |
| `[B]` | 同进程 真 Dock（未装钩子） | 同进程 | **焦点被夺** | ❌ |
| **`[C]`** | **跨进程 真 Dock（真实工况）** | 资源管理器 | **焦点未被夺** | ✅ |
| `[D]` | 跨进程 无保护假 Dock | 资源管理器 | **焦点被夺**（`WM_LBUTTONDOWN=1`） | ✅ 阳性对照有效 |
| `[E]` | 跨进程 带 `NOACTIVATE` 假 Dock | 资源管理器 | 焦点未被夺 | ✅ |

#### 结论

1. **跨进程（真实工况）：点击 Dock 不夺焦点 ✅**
   —— 这正是验收关心的场景，现在**在真实 Tauri 窗口上直接验证过了**（[C]），
   且与 [E]、P0 §3 的结论一致；
2. **同进程：点击 Dock 会夺焦点，而且钩子没有起作用**
   —— [A]（装钩子）与 [B]（未装钩子）结果**完全相同**，
   且 `WM_MOUSEACTIVATE` 计数在**所有已挂窗口上都是 0**。
   说明真实点击时这条消息落到了 §6.12 里那几个**无法子类化的深层 Chromium 窗口**上。

#### 这推翻了我之前的什么说法

| 之前说过的 | 实际情况 |
|---|---|
| 「合成点击在本环境不生效，端到端只能人工验证」 | ❌ 我的测试代码 bug，**端到端可以自动化验证** |
| 「`WM_MOUSEACTIVATE` 钩子提供了同进程防护」 | ❌ 钩子**装得上、单独量测也有效**，但**真实点击时根本走不到** |
| 「同进程风险低，因为 P1 没有会抢焦点的自有窗口」 | ✅ 这条仍然成立，且现在是**唯一**的实际防护 |

#### 同进程问题的正确处理方式

钩子这条路已经被证明走不通（深层 Chromium 窗口不可子类化，是 API 硬限制）。
可行的是**从设计上避免**：

1. **Dock 不要创建任何会获得焦点的自有窗口** —— 原生弹出菜单不会造成这个问题，
   将来的设置界面应加 `WS_EX_NOACTIVATE`；
2. 真需要时，可在处理点击后把焦点还给「点击前的 `GetForegroundWindow`」，
   但会有闪烁，作为兜底而非首选；
3. **不要**去 hook `Chrome_*` 内部窗口：跨线程 `SetWindowLongPtr` 改 Chromium 的 wndproc
   风险极高，得不偿失。

---

### 6.15 点击链路已端到端自动化验证（验收核心行为）

修好测试装置（§6.14）之后，「点击图标 → 激活/最小化」这条验收主线终于可以自动化验证了。

为了不依赖任何外部程序，先给 Dock 自己加了一个 `holdwin` 子进程模式：
`dock-app.exe holdwin` 只创建一个普通窗口并跑消息循环，充当「另一个进程的应用」。

#### A. 应用操作原语（`apps::`）

| 断言 | 结果 |
|---|---|
| 前置：焦点在别处 | PASS（`foreground=SETTINGS-HOLDER`） |
| `activate()` 把目标带到前台 | PASS（`foreground=DOCK-OPPONENT`） |
| `minimize()` 让目标最小化 | PASS（`IsIconic=true`） |
| `activate()` 能把最小化窗口还原并带到前台 | PASS（`IsIconic=false`，`foreground=DOCK-OPPONENT`） |

**4 项全通过。** 目标窗口属于**另一个进程**，与真实场景一致。

#### B. IPC 链路（JS → invoke → Rust 命令 → Win32）

这是「点击图标」路上最后一段没验证过的环节：前端到底能不能调到命令。

做法：用 `webview.eval` 在前端注入调用，前端走**和真实点击完全相同的** `activateApp`，
结果再通过一条回执命令写回 Rust（**不依赖 window title 之类的间接通道**）。

```
回执            : activate_app:ok
目标是否到前台  : true  (foreground=DOCK-OPPONENT)
-> 链路完整 ✅（前端成功调用命令且窗口行为正确）
```

#### 至此验收主线「能替代任务栏完成日常启动与切换常用应用」的自动化证据链

| 环节 | 验证方式 | 结果 |
|---|---|---|
| 界面渲染真实应用与图标 | 像素量测（§6.8、§6.13） | ✅ |
| 点击 Dock 不夺走当前应用焦点 | 焦点矩阵双向阳性对照（§6.14 [C]） | ✅ |
| 前端能调用到命令 | IPC 回执（本节 B） | ✅ |
| 激活 / 最小化 / 还原行为正确 | 跨进程断言（本节 A） | ✅ |

**仍然需要人工的**：UAC 弹窗（提权启动）、右键菜单的**视觉**确认、以及长时间观感。

---

### 6.16 R18 解决：给自动隐藏的任务栏预留空间（Q1「与任务栏并存」的正确落地）

**问题**（§6 曾以 R18 记录）：Dock 的触发区是屏幕底部 3px，**与自动隐藏的任务栏抢同一条边**。
用户的任务栏正是自动隐藏的，所以鼠标一触底，任务栏和 Dock 会同时弹出并**互相压住**。

**先量数据**（`Shell_TrayWnd`）：

| 状态 | 任务栏矩形 | 含义 |
|---|---|---|
| 自动隐藏（鼠标不在底部） | `(0,1078) 1920x60` | 底部只露 2px |
| **弹出**（鼠标触底） | `(0,1020) 1920x60` | 占 `1020..1080` |

而我们原来的 Dock 在 `960..1080` —— **任务栏弹出时会压住 Dock 的下半部分**。

**关键认知**：任务栏自动隐藏时 **`rcWork` 等于整屏**，
`SPI_GETWORKAREA` 完全不会告诉我们要给任务栏留空间（P0 就发现过：实测任务栏高度 0px）。
但 `Shell_TrayWnd` 窗口**始终存在**，隐藏态只是顶边贴到屏幕最底下，
所以**能直接量出它的高度和当前状态** —— 这比查注册表里的 Settings blob 干净得多。

**做法**：`win_layer::taskbar_info()` 量出任务栏高度与是否自动隐藏；
若自动隐藏且 `reserveTaskbar`（默认 true），底边再上移一个任务栏高度。

**实测结果**：

```
任务栏 = 高 60px，自动隐藏   => 预留空间：true

鼠标在中部（都隐藏）
  Dock   rect=(397,1082) 1125x120   ← 完全移出屏幕
  任务栏 rect=(0,1078)  1920x60

鼠标触底（两者都出现）
  Dock   rect=(397,900)  1125x120   ← 占 900..1020
  任务栏 rect=(0,1020)   1920x60    ← 占 1020..1080
```

**严丝合缝，零重叠。** 这正是 Q1「与任务栏并存」应有的样子。

新增配置项 `reserveTaskbar`（默认 `true`）；缺省字段由 `serde(default)` 补上默认值，
实测不写该字段时日志仍显示 `预留空间：true` ✅。

---

### 6.17 固定项与启动（补上「日常启动应用」这个功能缺口）

**发现的功能缺口**：到 §6.15 为止，Dock 只会显示**正在运行**的应用 ——
能切换，但**没法启动任何东西**。而验收标准是「能替代任务栏完成日常**启动**与切换常用应用」。
一个只会显示已运行窗口的 Dock 替代不了任务栏。这一轮补上。

**实现**：

| 部分 | 做法 |
|---|---|
| 配置 | `Preferences.pinned: Vec<PinnedApp>`（id / displayName / target） |
| 合并 | `apps::enumerate_with_pinned()`：**固定项在前（按固定顺序），未固定的运行项在后**，对齐 macOS ⚠️ **此模型已于 2026-09 废弃** —— 改为「顺序只由用户列表决定，运行状态不参与」，见下 |
| 未运行的固定项 | `running = false`、`windows` 为空 —— 前端据此决定点击是「启动」还是「切换」（**这一条仍然成立**） |
| 右键菜单 | 新增「固定到 Dock / 从 Dock 移除」；**未运行的应用把窗口类操作全部置灰**，只留「启动」（⚠️ 2026-09 后只剩「从 Dock 移除」，因为能看到的图标必然已在列表里） |
| 托盘/配置 | 固定关系持久化到 `config.json` |

> ⚠️ **2026-09 模型修正（本节下面的内容已是历史）**：两段式「固定项 ∪ 运行项」会让
> 没添加过的程序一跑就自己冒出来、退出又消失，且**同一程序在运行/未运行两种状态下
> 位置可能不同**。现已改为**单列表**：Dock 上的图标与顺序完全由 `pinned` 决定，
> 运行只影响白点与点击语义。`enumerate_with_pinned()` 已更名为
> `enumerate_dock_apps()`，`discovered` 字段已删除。
> 权威说明见 `dock-technical-design.md` §4.2「应用顺序模型」。

**点击行为**（对齐 macOS）：

```
固定但未运行 → 启动
已在前台     → 最小化（再点收起）
其余         → 激活窗口
```

**实测**：

```
[PASS] launch() 后运行中的应用数增加  运行中应用数 3 -> 4  result=Ok(())
小结：5 项通过，0 项失败
```

界面像素量测：内容宽 `357` 物理像素（之前 3 个图标时是 242），固定项已成功显示。

#### ❗踩到的坑：应用执行别名（stub）会让固定项“对不上”

第一版断言是「启动后 target 是否等于传入路径」，结果 **FAIL**：传入
`C:\Windows\System32\notepad.exe`，启动后枚举到的却是
`C:\Program Files\WindowsApps\Microsoft.WindowsNotepad_...\Notepad.exe`。

原因：**`System32\notepad.exe` 是「应用执行别名」**，一个重定向到商店版记事本的小程序，
真实进程路径在 `WindowsApps` 下。**启动本身是成功的**（dump 里 Notepad 确实出现了、
还成了前台窗口），只是按传入路径比对必然匹配不上。

**对产品的影响**：如果用户**手工在 config.json 里**按 stub 路径固定一个应用，
它会和运行时枚举出的真实条目**对不上**，表现为 Dock 上出现「一个固定图标 + 一个运行图标」两个。
不过**通过右键菜单固定不会踩这个坑** —— 菜单固定的是**正在运行条目的真实 id/target**。

修法：断言改成「运行中的应用数是否增加」，这才是真正要验的东西。

---

### 6.18 「添加应用…」入口（补齐固定的另一半）

§6.17 做了固定项，但只能固定**正在运行**的应用。
而用户想放上 Dock 的，往往正是那些**还没启动**的常用应用 —— 少了这个入口，
「日常启动」这条验收只成立一半。

**实现**：`picker.rs` 用 `IFileOpenDialog`（Vista+ 的通用文件对话框，
而不是已过时的 `GetOpenFileName`）：

- 两个实现要点：`spawn_blocking` 的线程**默认没初始化 COM**，必须自己 `CoInitializeEx`；
  `GetDisplayName(SIGDN_FILESYSPATH)` 的返回值用 `CoTaskMemAlloc` 分配，**必须自己 `CoTaskMemFree`**；
- 入口放在**托盘菜单**「添加应用…」（文件对话框是模态的，不能占住主线程，
  托盘回调里另起线程跑）；选择期间置 `reveal::PAUSED`，避免 Dock 中途隐藏；
- 选中后取版本信息的 FileDescription 当展示名，再走已有的 `store::pin`。

**验证状况**：编译通过、应用启动正常、托盘创建成功、界面与定位无回归（实测
`显示 y=900`，任务栏空间仍正确预留）。**但文件对话框本身需要人工点一次** ——
它是交互式模态窗口，自动化打开会挂住进程，不能脚本验证。

**已知取舍**：入口只在托盘里，Dock 本体上没有「+」按钮。
在 Dock 上加「+」需要往图标列表里塞一个非应用条目，会把「图标索引 ↔ 应用」的对应关系搞乱，
P2 做拖拽排序时再一并处理更合适。

---

### 6.19 修复用户反馈：「灰色背景上才是 Dock」

**现象（用户反馈）**：Dock 周围有一圈灰色背景，Dock 浮在灰色背景上。

#### 根因

窗口尺寸是**写死**的 900×96 逻辑像素（物理 1125×120），
而 `.dock` 面板宽度是**跟着图标行算的**（`rowWidth + padding`）：

| 元素 | 宽度（逻辑） |
|---|---|
| 窗口（亚克力覆盖范围） | **900** |
| 面板（只开 2-3 个应用时） | ~220 |

而路线 B 的亚克力是**按整个窗口矩形**铺的，不是按 HTML 内容铺的 ——
面板之外那 75% 的空玻璃就成了一块灰底。

> 这个问题的苗头其实早就在像素量测里出现过：窗口边缘采样值是 `404043`、`414144`
> 这类灰色，我当时当成"模糊后的桌面"解释过去了。**量到了数据不等于解释对了。**

#### 修法：窗口尺寸跟随面板

1. **前端上报尺寸**：新增 `set_dock_size(width, height, radius)` 命令，
   前端算好面板尺寸后上报；
2. **面板宽度按「放大到最大时」定死**（新增 `maxMagnifiedWidth()`）：
   这样悬停时窗口不需要改尺寸（否则每帧改窗口会闪烁 + 开销）；
3. **`reveal` 线程改为每帧读共享的 TARGET 矩形**，尺寸变化立刻跟上，无需重启线程；
4. **顺带修掉一个潜在裁切**：放大倍率改为按几何上限收敛
   （图标最高只能到 `面板高 88 - 底部间距 8 - 顶部余量 2` = 78，
   56px 图标 → 上限约 1.39 倍），默认增量从 0.7 调到 0.35。
   否则放大后的图标顶部会被窗口切平。

**实测**：窗口宽度 **1125 → 328 物理像素**，内容包围盒占满 **100%**（x 0..316 / 317），水平居中。

#### ❗圆角：`SetWindowRgn` 实测**无效**，`DWMWCP_ROUND` 才有效

窗口缩到面板大小后，四角仍会露出方形灰底（CSS 的 `border-radius` 管不到 OS 材质）。

**先试的错路**：`SetWindowRgn` 把窗口裁成圆角矩形。做了 A/B 对照
（设区域 vs 显式 `SetWindowRgn(hwnd, None, true)` 清除区域），
读同一个角点像素 —— **结果逐字节相同**。结论：**GDI 的窗口区域约束不了 DWM 的 accent 材质。**

> 第一次做这个 A/B 时两组结果也"相同"，但那是**对照本身失效**：
> `main.rs` 在启动时设过区域，而"不裁剪"分支只是改了尺寸、没清除区域。
> 补上清除之后才得到可信结论。**对照没生效时的"相同"毫无意义。**

**真正有效的做法**：把原本设的 `DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_DONOTROUND`
（当初是为了自绘圆角）改回 **`DWMWCP_ROUND`**，让 DWM 裁窗户本身 ——
它是唯一能让材质跟着形状走的途径。

**单像素 A/B 验证**：

| 设置 | 左上角像素 | 判定 |
|---|---|---|
| `DWMWCP_ROUND` | `#E4E4E4`（浅色 = 背景） | 材质被裁 ✅ |
| `DWMWCP_DONOTROUND` | `#67676A`（灰色 = 亚克力） | 材质铺到角上 ❌ |

**代价**：圆角半径由系统决定（约 8px），不是设计稿的 24px。
要还原 24px 圆角，只能改用**独立的自绘渲染窗口**（P2 方向）。
为此移除了 `SetWindowRgn` 的调用（它对本项目没有任何效果，只多一个 GDI 对象）。

---

### 6.20 修复用户反馈：「图标也有背景图」

**现象（用户反馈）**：灰底修好之后，图标看起来仍然各自带一块方形底板。

#### 排查过程（先量，不猜）

**第一步：图标本身是不是不透明方块？** 加 alpha 统计：

| 应用 | alpha 范围 | 全透明占比 | 全不透明占比 |
|---|---|---|---|
| Windows 资源管理器 | 0..255 | 18.3% | 81.2% |
| Microsoft Edge | 0..255 | 30.3% | 65.9% |
| 设置 | 0..255 | 36.6% | 60.5% |

**图标确实有透明区**，不是提取成了不透明方块。又打了 alpha 轮廓图确认形状：

```
................................
.##########:....................     ← 文件夹的标签
############....................
################################     ← 文件夹主体本来就满宽
################################
```

是**真正的文件夹形状**。所以问题不在图标本身。

**第二步：看像素。** 高分辨率网格显示 —— 每个图标**正下方**有一条与图标等宽的暗带
（`#414144` / `#3C3C40`），而**图标之间**是正常面板色（`#4A4A4D`）。
暗带宽度恰好等于图标宽度，这指向**阴影**。

#### 根因

```css
.dock-icon.has-icon { box-shadow: 0 2px 6px rgba(0, 0, 0, 0.35); }
```

**`box-shadow` 画的是「按钮矩形」的阴影，不是图标形状的阴影。**
图标是透明 PNG，所以按钮矩形那块就成了一片方形暗色底板 —— 正是"图标带背景图"的观感。

#### 修法

换成 `drop-shadow`（按元素渲染后的 alpha 形状投影）会好一些，
**但实测仍不够**：像资源管理器这种**文件夹图标本身就近乎满方**，
投影依然是接近方形的暗带，看起来还是像底板。

所以最终**直接不加任何阴影**：

```css
.dock-icon.has-icon { box-shadow: none; background: transparent !important; filter: none; }
```

**实测**：图标下方那一行从 `#3C3C40` 变回面板色 `#48494C`，暗带消失。

> **教训**：`box-shadow` 与 `drop-shadow` 的区别值得记住 ——
> 前者是**盒子**的阴影，后者是**形状**的阴影。
> 凡是元素本身是透明图形（PNG / SVG / 文字），一律应该用 `drop-shadow`。

---

## 7. 下一步

| 优先级 | 任务 |
|---|---|
| P1 | **人工确认**：UAC 弹窗、右键菜单视觉、「添加应用…」文件对话框（见下） |
| P1 | **长稳测试**：8 小时 —— **需真实流逝时间，本次会话内无法完成**（见 §6.11） |
| P2 | 固定项拖拽排序；Dock 本体上的「+」入口 |
| P2 | 设置界面（目前只能改 JSON 或走托盘的四个开关） |
| P2 | 用 `SetWinEventHook` 推送替换前端轮询（⚠️ 本行原写「实测 CPU 仅 0.08%，非必需」——**该数字已作废**。后续实测与归因：1.5 秒轮询下空闲 2.34%，但**轮询只占其中 0.15%**；2.1% 的真凶是 `reveal.rs` 每 16ms 无条件 `SetWindowPos`，已修复，修复后空闲 **0.42~0.47%**。因此本项的 **CPU 理由不成立**，只剩「亚秒级延迟」与「为窗口预览 / 每窗口白点铺路」两条。详见 `backlog.md` BL-1 / BL-2） |
| P2 | 菜单换成独立窗口以还原 macOS 观感（当前是 Windows 原生菜单） |
| P2 | 内存优化：WebView2 6 个子进程共 183 MB（见 §6.11） |

### 仍然需要人工确认的

前面几轮把能自动化的都自动化了（焦点矩阵、应用操作、IPC 链路、启动、自动隐藏、任务栏避让、
配置驱动行为）。真正剩下的是**自动化做不了**的两件：

1. **UAC 弹窗**：右键图标 → 「以管理员身份运行」应弹出 UAC（弹窗在安全桌面上，无法脚本确认）；
2. **右键菜单的视觉效果**：菜单项是否齐全、置灰状态是否正确（原生菜单的外观需要人眼看）。

```powershell
cd C:\Program1\Projects\Dock\app\src-tauri
cargo run
```

- 自动隐藏默认开启：鼠标移到屏幕**最底边**唤出（会自动停在任务栏上方，不会与任务栏重叠）；
- 右键任意图标 → 菜单里可以「固定到 Dock」；固定后即使该应用没运行也会留在 Dock 上，点击即启动；
- 托盘图标右键 → 「自动隐藏」「开机自启」「退出 Dock」。

### 开发循环备忘

```powershell
# 前端（改用 fnm 的 node 绝对路径，因为 DSH 自带的 node 没有 npm）
$nodeDir = "C:\Program1\fnm-window\node-versions\v24.19.0\installation"
$env:PATH = "$nodeDir;$env:PATH"
$env:npm_config_cache = "C:\Program1\Projects\Dock\.npm-cache"   # 沙箱要求缓存落在工作区内

cd app
npm run check:magnify      # 鱼眼数学数值校验
npm run build              # 类型检查 + 打包到 app/dist

cd src-tauri
cargo build --offline      # ← 必须！Tauri 在编译期嵌入 dist
```

> 在受 DSH 沙箱限制的环境里，`vite build` 需要放宽权限（Vite 用管道 stdio 解析真实路径，
> 受限模式会 `spawn EPERM`）；运行 Dock 应用也需要放宽（WebView2 需要 `%LOCALAPPDATA%` 与命名管道）。

> 端到端手工验证（6.3 建议的那 5 秒）建议尽早做，它是焦点行为唯一的最终依据。
