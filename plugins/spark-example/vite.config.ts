/**
 * 示例插件（spark-example）产物构建配置（插件 iframe 沙箱化阶段 A）。
 *
 * vite lib 模式多入口产出 ESM bundle：
 * - dist/views/main.js       主入口（当前壳层 srcdoc 固定加载它，内部按
 *   window.__sparkPluginView 分发主视图/卡片视图——这是现实约束下的
 *   workaround，设计方向是壳层按 view 直载各视图 bundle，届时主入口的
 *   分发职责即退役）；
 * - dist/views/post-card.js  消息卡片视图 bundle（设计文档「包形态」
 *   多视图约定：views/<viewId>.js；当前经主入口动态 import 加载，
 *   作为独立入口产出是为壳层按 view 直载预留的产物形态）；
 * - dist/chunks/*.js         两入口共享代码（vue/element-plus/SDK）自动
 *   切出的共享 chunk——多入口下无法 inlineDynamicImports，共享 chunk
 *   随 dist 一起分发，CSP script-src 插件源 origin 已覆盖；
 * - @spark/plugin-sdk 为零依赖源码包，经相对路径引入直接打进 bundle；
 * - vue / element-plus 同样打进 bundle（框架自包含原则：插件间版本互不干扰，
 *   CSP 禁止远程 script-src，不允许运行时外引任何代码），无 external；
 * - manifest.json 与静态资源由 scripts/copy-example-dist.mjs 在构建后拷贝。
 *
 * plugins 目录不持有 node_modules（不新增 npm 依赖）：vite 本体由
 * plugins/package.json 的 build:example 脚本经 node 直接调起
 * （code/app/node_modules/vite），vue/element-plus/@vitejs/plugin-vue 的
 * 依赖副本经绝对路径锚定到 code/app/node_modules（与 app/vite.config.ts
 * 的 alias 同一策略）。
 */
import vue from '../../app/node_modules/@vitejs/plugin-vue/dist/index.mjs';
import { fileURLToPath } from 'node:url';

const here = (path) => fileURLToPath(new URL(path, import.meta.url));

export default {
  root: here('./'),
  plugins: [vue()],
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
        'post-card': here('./post-card.ts')
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
