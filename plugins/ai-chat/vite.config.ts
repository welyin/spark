/**
 * ai-chat 插件产物构建配置（与 spark-example 对齐 lib 模式）。
 *
 * - dist/views/main.js  壳层 srcdoc 固定加载的主入口
 * - dist/chunks/*.js    共享代码自动切出的 chunk
 * - dist/assets/main.css 提取的 scoped 样式（lib 模式产出）
 * - manifest.json 由 scripts/copy-ai-chat-dist.mjs 在构建后拷贝
 *
 * plugins 目录不持有 node_modules（不新增 npm 依赖）：vite 本体由
 * plugins/package.json 脚本经 node 直接调起（code/app/node_modules/vite），
 * vue/@vitejs/plugin-vue 的依赖副本经绝对路径锚定到 code/app/node_modules。
 */
import vue from '../../app/node_modules/@vitejs/plugin-vue/dist/index.mjs';
import { fileURLToPath } from 'node:url';
import { resolve } from 'path';

const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));

export default {
  root: here('./'),
  plugins: [vue()],
  // WebView 无 process 对象，必须手动替换 process.env.NODE_ENV
  define: {
    'process.env.NODE_ENV': JSON.stringify('production')
  },
  resolve: {
    alias: [
      { find: 'vue', replacement: here('../../app/node_modules/vue') }
    ]
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    minify: 'esbuild',
    lib: {
      entry: {
        main: here('./index.ts'),
        // 后台入口（内核 QuickJS 沙箱）：零依赖纯脚本，宿主直接 eval
        background: here('./background.ts')
      },
      formats: ['es']
    },
    rollupOptions: {
      output: {
        entryFileNames: 'views/[name].js',
        chunkFileNames: 'chunks/[name]-[hash].js'
      }
    }
  }
};

