import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import Prompt from "./Prompt";
import "./prompt.css";

import { installFrontendLogging } from "../lib/log";

// 前端错误/警告一律转进 Dock 的日志文件（WebView 是独立进程，否则这些信息没人看得到）
installFrontendLogging("prompt");

// 命名输入框窗口的入口（给文件夹起名 / 改名）。
// 与 Dock 本体、设置页、浮层是四个独立页面，由 vite.config.ts 的
// rollupOptions.input 分别打包。
//
// 为什么它是**自己的窗口**而不是浮层的一部分：浮层窗口是 `WS_EX_NOACTIVATE`
// （"点 Dock 不夺焦点"的前提），**打不了字**。输入必须落在一个真正能拿焦点的窗口上。
const root = document.getElementById("root");
if (!root) {
  throw new Error("#root 不存在");
}

createRoot(root).render(
  <StrictMode>
    <Prompt />
  </StrictMode>,
);
