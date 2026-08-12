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
        const [rawPluginId, ...rest] = segments;
        // repo id（plugin-dist §1）经 encodeURIComponent 收成单段传输，此处解码还原；
        // 解码后仍须拒 `..` 与反斜杠（编码可绕过上方逐段校验，双保险）
        let pluginId = rawPluginId;
        try {
          pluginId = decodeURIComponent(rawPluginId);
        } catch {
          res.statusCode = 400;
          res.end('bad plugin id encoding');
          return;
        }
        if (pluginId.includes('..') || pluginId.includes('\\')) {
          res.statusCode = 400;
          res.end('bad plugin path');
          return;
        }
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

type DevPluginInfo = { id: string; name: string; version: string; icon?: string };

/**
 * dev 插件自动发现（plugin_decoupling.md §5）：扫描 code/plugins 下各子目录的
 * manifest.json，把每个子目录作为一个「开发插件」注入 dev 目录（id / name / version /
 * 资源基地址指向 vite 中间件）。支持 `VITE_PLUGIN_DEV=ai-chat,spark-moments`
 * 指定子集，未设置则自动扫描全部。
 *
 * 链路归属：纯 vite 侧（Node 端有 fs 可扫描插件目录），经 `define` 注入
 * `__DEV_PLUGINS__`。**生产构建注入空数组** + 运行时 `import.meta.env.DEV` 门控
 * （见 src/mock/dev-plugins.ts），确保 dev 逻辑与插件清单不打进生产 bundle（§9）。
 */
function devPluginDiscovery(): Plugin {
  return {
    name: 'spark-dev-plugin-discovery',
    config: (_config, env) => {
      // 双重确认：仅 dev serve（command=serve 且 mode != production）注入真实 dev 插件；
      // 任何 build（含 `vite build --mode development` 等分阶段构建）一律注入空数组，
      // 避免真实插件清单（id/name）残留进生产 bundle（安全评审 🟡）。
      // 运行时另有 import.meta.env.DEV 门控（src/mock/dev-plugins.ts）兜底。
      const isProductionBuild =
        process.env.NODE_ENV === 'production' ||
        env.mode === 'production' ||
        env.command === 'build';
      const devPlugins: DevPluginInfo[] = isProductionBuild ? [] : scanDevPlugins();
      return {
        define: {
          __DEV_PLUGINS__: JSON.stringify(devPlugins)
        }
      };
    }
  };
}

/** 扫描 code/plugins 下各子目录的 manifest.json；VITE_PLUGIN_DEV 指定子集（未设置 = 全部）。 */
function scanDevPlugins(): DevPluginInfo[] {
  const pluginsRoot = fileURLToPath(new URL('../plugins', import.meta.url));
  const subset = process.env.VITE_PLUGIN_DEV
    ? process.env.VITE_PLUGIN_DEV.split(',').map((id) => id.trim()).filter(Boolean)
    : null;
  const plugins: DevPluginInfo[] = [];
  for (const entry of fs.readdirSync(pluginsRoot, { withFileTypes: true })) {
    if (!entry.isDirectory()) {
      continue;
    }
    if (subset && !subset.includes(entry.name)) {
      continue;
    }
    const manifestPath = path.join(pluginsRoot, entry.name, 'manifest.json');
    if (!fs.existsSync(manifestPath)) {
      continue;
    }
    try {
      const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8')) as {
        id?: string;
        name?: string;
        version?: string;
      };
      plugins.push({
        id: manifest.id ?? entry.name,
        name: manifest.name ?? entry.name,
        version: manifest.version ?? '0.0.0'
      });
    } catch {
      // 单个坏 manifest 静默跳过，不阻断其他插件
    }
  }
  return plugins;
}

// Tauri 2 前端约定（https://v2.tauri.app/start/frontend/vite/）：
// - 固定 dev 端口 1420，与 src-tauri/tauri.conf.json 的 devUrl 对齐
// - 禁止清屏以便看到 rust 侧输出
// - 只暴露 VITE_/TAURI_ 前缀的环境变量
export default defineConfig({
  plugins: [vue(), pluginSourceMiddleware(), devPluginDiscovery()],
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
    // 移动端（Android/iOS）真机调试：tauri CLI 检测到设备后把 devUrl host 替换为
    // 局域网 IP（如 192.168.31.99），并把该地址写入 TAURI_DEV_HOST——dev server 须
    // 监听该地址（0.0.0.0）才能被真机访问；桌面端保持默认 localhost 绑定。
    host: process.env.TAURI_DEV_HOST ?? false,
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
