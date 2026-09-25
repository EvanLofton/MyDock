/**
 * 与 Rust 侧类型对齐（Rust 用 #[serde(rename_all = "camelCase")]）
 */

export type AppKind = "win32" | "uwp" | "unknown";

export interface WindowRef {
  hwnd: number;
  title: string;
  minimized: boolean;
}

export interface AppEntry {
  /** 稳定标识：Win32 为 exe 全路径小写；UWP 为 uwp:<AUMID> */
  id: string;
  displayName: string;
  kind: AppKind;
  /** 启动目标：Win32 是 exe 全路径；打包应用是 shell:AppsFolder\<AUMID> */
  target: string;
  isElevated: boolean;
  hasForeground: boolean;
  /** 当前是否有窗口在运行。false = 固定但未运行，点击应「启动」 */
  running: boolean;
  windows: WindowRef[];
  /**
   * 这一条是**分割线**（纯视觉分组），不是应用。
   *
   * 它在列表里就是一个普通条目：能拖动排序、能拖出去删除、能被右键移除 ——
   * 所以没有单独的「分割线位置」需要维护，`pinned` 的顺序就是全部真相。
   */
  separator: boolean;
  /**
   * 这一条是**文件夹**（Dock 大文件夹）：占一个图标位，点开显示 `children`。
   * `running` 表示"里面有东西在运行"。
   */
  isFolder: boolean;
  /** 文件夹里的条目（非文件夹恒为空）。UI 只拿前 4 个画 2×2 预览 */
  children: AppEntry[];
}
