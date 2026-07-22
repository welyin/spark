import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';
import { fileURLToPath } from 'node:url';

// Tauri 2 前端约定（https://v2.tauri.app/start/frontend/vite/）：
// - 固定 dev 端口 1420，与 src-tauri/tauri.conf.json 的 devUrl 对齐
// - 禁止清屏以便看到 rust 侧输出
// - 只暴露 VITE_/TAURI_ 前缀的环境变量
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  envPrefix: ['VITE_', 'TAURI_'],
  resolve: {
    alias: [
      // 插件源码在工程根之外（code/plugins，见 plugin-loader.ts）：bare import
      // 沿插件目录向上解析不到本工程 node_modules，显式锚定依赖副本
      // （src 内同包导入解析到同一目标，兼起 dedupe 作用，不会双实例）。
      { find: 'vue', replacement: fileURLToPath(new URL('./node_modules/vue', import.meta.url)) },
      { find: 'element-plus', replacement: fileURLToPath(new URL('./node_modules/element-plus', import.meta.url)) }
    ]
  },
  server: {
    port: 1420,
    strictPort: true,
    // 插件目录在工程根之外（code/plugins，plugin-loader.ts 的 glob 指向它）；
    // dev server 默认只允许 serve workspace 根（code/app），需显式放开上级。
    fs: {
      allow: ['..']
    },
    watch: {
      // rust 代码变动不应触发前端 reload
      ignored: ['**/src-tauri/**']
    }
  },
  build: {
    // Tauri 桌面 WebView：Windows=Chromium，macOS/iOS=WKWebView
    target: process.env.TAURI_ENV_PLATFORM === 'windows' ? 'chrome105' : 'safari13',
    minify: process.env.TAURI_ENV_DEBUG ? false : 'esbuild',
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    outDir: 'dist',
    emptyOutDir: true
  }
});
