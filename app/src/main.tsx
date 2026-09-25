import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";

import { installFrontendLogging } from "./lib/log";

// 前端错误/警告一律转进 Dock 的日志文件（WebView 是独立进程，否则这些信息没人看得到）
installFrontendLogging("dock");

const root = document.getElementById("root");
if (!root) {
  throw new Error("#root 不存在");
}

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
