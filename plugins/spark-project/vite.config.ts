/**
 * 项目（议题）插件（spark-project）产物构建配置。
 *
 * 对齐 spark-kanban/vite.config.ts（插件体系统一构建约定）：
 * - vite lib 模式多入口 ESM：dist/views/main.js（主入口，内部按
 *   __sparkPluginView.viewId 分发）+ dist/views/affair-card.js（项目动态卡片）
 *   + dist/views/release-card.js（发布卡片——组合 release-manager 库件推送的
 *   卡片在本插件 manifest 下按同名 viewId 渲染）；
 * - 库依赖（档二-3）：vendor/ 下锚定的 spark-kanban / spark-release-manager
 *   库包源码在构建期全部打进 bundle（dist 自洽扫描不允许残留 vendor 引用，
 *   spark-plugin-cli build 强制）；
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
        'affair-card': here('./affair-card.ts'),
        'release-card': here('./release-card.ts')
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
