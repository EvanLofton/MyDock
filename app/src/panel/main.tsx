import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import Panel from "./Panel";
import "./panel.css";

import { installFrontendLogging } from "../lib/log";

// 前端错误/警告一律转进 Dock 的日志文件（WebView 是独立进程，否则这些信息没人看得到）
installFrontendLogging("panel");

// 浮层窗口的入口（右键菜单 + 文件夹面板）。
// 与 Dock 本体（src/main.tsx）、设置页（src/settings/main.tsx）
// 是三个独立页面，由 vite.config.ts 的 rollupOptions.input 分别打包。
const root = document.getElementById("root");
if (!root) {
  throw new Error("#root 不存在");
}

createRoot(root).render(
  <StrictMode>
    <Panel />
  </StrictMode>,
);
