import { defineConfig, type Plugin } from 'vite';
import vue from '@vitejs/plugin-vue';
import { fileURLToPath } from 'node:url';
import fs from 'node:fs';
import path from 'node:path';

// 插件源服务 CSP（与 src-tauri/src/plugin_src.rs 的 PLUGIN_CSP 一致，双重施加的一重）
const PLUGIN_CSP =
  "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:";

const MIME_BY_EXT: Record<string, string> = {
  js: 'text/javascript; charset=utf-8',
  mjs: 'text/javascript; charset=utf-8',
  css: 'text/css; charset=utf-8',
  json: 'application/json; charset=utf-8',
  map: 'application/json; charset=utf-8',
  html: 'text/html; charset=utf-8',
  svg: 'image/svg+xml',
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  gif: 'image/gif',
  webp: 'image/webp',
  ico: 'image/x-icon',
  woff: 'font/woff',
  woff2: 'font/woff2'
};

/**
 * dev 插件源中间件：与生产 plugin:// 协议（src-tauri/src/plugin_src.rs）同形，
 * 提供 http://localhost:1420/plugin/<pluginId>/<path>，从内置插件 dist 读文件。
 *
 * 与生产的差异（dev 下同 origin，无 OOPIF）：
 * - 生产 iframe 资源走独立 scheme（Windows 为 http://plugin.localhost），
 *   dev 浏览器没有自定义 scheme，资源与壳层同 origin；沙箱 iframe
 *   （sandbox="allow-scripts"，无 allow-same-origin）仍会拿到 opaque origin，
 *   桥握手 expectedOrigin 因此两端一致（'null'）；
 * - 只读内置 dist（code/plugins/<id>/dist），不解析已安装 .spkg——dev 链路
 *   面向插件开发，市场安装包的源服务以生产 plugin:// 为准。
 */
function pluginSourceMiddleware(): Plugin {
  const pluginsRoot = fileURLToPath(new URL('../plugins', import.meta.url));
  return {
    name: 'spark-plugin-source',
    configureServer(server) {
      server.middlewares.use('/plugin', (req, res) => {
        const raw = (req.url ?? '').split('?')[0].replace(/^\/+/, '');
        const segments = raw.split('/').filter((segment) => segment.length > 0 && segment !== '.');
        // 路径穿越防护：拒绝 .. 与反斜杠；至少需要 <pluginId>/<path>
        if (
          segments.length < 2 ||
          segments.some((segment) => segment === '..' || segment.includes('\\'))
        ) {
          res.statusCode = 400;
          res.end('bad plugin path');
          return;
        }
        const [pluginId, ...rest] = segments;
        const relPath = rest.join('/');
        const filePath = path.join(pluginsRoot, pluginId, 'dist', relPath);
        // join 后仍须落在 dist 根内（双保险，段校验已拒 ..）
        const distRoot = path.join(pluginsRoot, pluginId, 'dist');
        if (!filePath.startsWith(distRoot) || !fs.existsSync(filePath) || !fs.statSync(filePath).isFile()) {
          res.statusCode = 404;
          res.end('plugin resource not found');
          return;
        }
        const ext = relPath.split('.').pop()?.toLowerCase() ?? '';
        res.setHeader('Content-Type', MIME_BY_EXT[ext] ?? 'application/octet-stream');
        res.setHeader('Content-Security-Policy', PLUGIN_CSP);
        // 沙箱 iframe 为 opaque origin，module script 走 CORS：显式放行
        res.setHeader('Access-Control-Allow-Origin', '*');
        res.end(fs.readFileSync(filePath));
      });
    }
  };
}

// Tauri 2 前端约定（https://v2.tauri.app/start/frontend/vite/）：
// - 固定 dev 端口 1420，与 src-tauri/tauri.conf.json 的 devUrl 对齐
// - 禁止清屏以便看到 rust 侧输出
// - 只暴露 VITE_/TAURI_ 前缀的环境变量
export default defineConfig({
  plugins: [vue(), pluginSourceMiddleware()],
  clearScreen: false,
  envPrefix: ['VITE_', 'TAURI_'],
  resolve: {
    alias: [
      // 插件源码在工程根之外（code/plugins，dev 中间件与插件源服务指向它）：
      // bare import 沿插件目录向上解析不到本工程 node_modules，显式锚定依赖副本
      // （src 内同包导入解析到同一目标，兼起 dedupe 作用，不会双实例）。
      { find: 'vue', replacement: fileURLToPath(new URL('./node_modules/vue', import.meta.url)) },
      { find: 'element-plus', replacement: fileURLToPath(new URL('./node_modules/element-plus', import.meta.url)) }
    ]
  },
  server: {
    port: 1420,
    strictPort: true,
    // 插件目录在工程根之外（code/plugins，dev 中间件直接从这里出插件源）；
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
