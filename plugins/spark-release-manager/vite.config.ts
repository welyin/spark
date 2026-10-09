/**
 * 发布管理插件（spark-release-manager）产物构建配置。
 *
 * 对齐 spark-kanban/vite.config.ts（插件体系统一构建约定）：
 * - vite lib 模式多入口 ESM：dist/views/main.js（主入口，内部按
 *   __sparkPluginView 分发）+ dist/views/release-card.js（message-card 卡片，
 *   为壳层按 view 直载预留）+ dist/views/lib.js（库包入口——「项目」等组合者
 *   构建期依赖本件时的引入面，纯导出、不握手不挂载，release-management.md §5）；
 * - vue / element-plus / @spark/plugin-sdk 全部打进 bundle（框架自包含，
 *   无 external），共享代码自动切 dist/chunks/*.js；
 * - plugins 目录不持有 node_modules：vite 本体与框架依赖副本经绝对路径
 *   锚定到 code/app/node_modules。
 */
import vue from '../../app/node_modules/@vitejs/plugin-vue/dist/index.mjs';
import { fileURLToPath } from 'node:url';

const here = (path) => fileURLToPath(new URL(path, import.meta.url));

export default {
  root: here('./'),
  plugins: [vue()],
  // lib 模式不替换 process.env.NODE_ENV：WebView 中无 process 对象会抛错
  define: {
    'process.env.NODE_ENV': JSON.stringify('production')
  },
  resolve: {
    alias: [
      { find: 'vue', replacement: here('../../app/node_modules/vue') },
      { find: 'element-plus', replacement: here('../../app/node_modules/element-plus') }
    ]
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    minify: 'esbuild',
    lib: {
      entry: {
        main: here('./index.ts'),
        'release-card': here('./release-card.ts'),
        lib: here('./lib.ts')
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
