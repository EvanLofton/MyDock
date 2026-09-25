import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 侧 build.frontendDist 指向 ../dist，即本目录下的 dist
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // WebView2 基于较新的 Chromium，可以放心用现代语法
    target: "chrome110",
    sourcemap: false,
    rollupOptions: {
      // 四个独立页面，对应四个窗口：
      //   index.html    —— Dock 本体（无边框、透明、不抢焦点）
      //   settings.html —— 设置窗口（普通窗口，见 src-tauri/src/settings_window.rs）
      //   panel.html    —— 浮层（右键菜单 + 文件夹面板 + 悬停标签，见 src-tauri/src/panel_window.rs）
      //   prompt.html   —— 命名输入框（给文件夹起名 / 改名，见 src-tauri/src/prompt_window.rs）
      // 路径相对 root（本目录），不要用 __dirname —— package.json 是 "type": "module"。
      input: {
        main: "index.html",
        settings: "settings.html",
        panel: "panel.html",
        prompt: "prompt.html",
      },
    },
  },
  server: {
    port: 1420,
    strictPort: true,
  },
});
