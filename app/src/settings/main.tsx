import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import Settings from "./Settings";

import { installFrontendLogging } from "../lib/log";

// 前端错误/警告一律转进 Dock 的日志文件（WebView 是独立进程，否则这些信息没人看得到）
installFrontendLogging("settings");

// 设置窗口的入口。与 Dock 本体（src/main.tsx）是两个独立页面，
// 由 vite.config.ts 的 rollupOptions.input 分别打包。
const root = document.getElementById("root");
if (!root) {
  throw new Error("#root 不存在");
}

createRoot(root).render(
  <StrictMode>
    <Settings />
  </StrictMode>,
);
