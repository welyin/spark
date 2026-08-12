/**
 * 朋友圈插件（spark-moments）产物构建配置（对齐 spark-example / ai-chat lib 模式）。
 *
 * vite lib 模式多入口产出 ESM bundle：
 * - dist/views/main.js        主入口（壳层 srcdoc 固定加载，内部按
 *   window.__sparkPluginView 分发主视图/notify-card 卡片视图——多视图分发
 *   职责见 index.ts，与 spark-example 同构）；
 * - dist/views/notify-card.js 互动通知卡片视图 bundle（message-card 富渲染，
 *   「X 赞了你的动态」/「X 评论了你的动态」+ 动态摘要，独立入口经主入口
 *   动态 import 加载）；
 * - dist/views/background.js  后台入口（内核 QuickJS 沙箱）：零依赖纯脚本，
 *   宿主直接 eval（manifest.background 指向它）；
 * - dist/chunks/*.js          多入口共享代码（vue/element-plus/SDK）切出的 chunk。
 *
 * @spark/plugin-sdk 为零依赖源码包，经相对路径引入直接打进 bundle；
 * vue / element-plus 同样打进 bundle（框架自包含），无 external。
 * plugins 目录不持有 node_modules：vite 本体与 vue/element-plus 的依赖副本
 * 经绝对路径锚定到 code/app/node_modules（与 app/vite.config.ts alias 同策略）。
 * manifest.json 与静态资源由 scripts/copy-moments-dist.mjs 在构建后拷贝。
 */
import vue from '../../app/node_modules/@vitejs/plugin-vue/dist/index.mjs';
import { fileURLToPath } from 'node:url';

const here = (path) => fileURLToPath(new URL(path, import.meta.url));

export default {
  root: here('./'),
  plugins: [vue()],
  // lib 模式不自动替换 process.env.NODE_ENV，不替换则 vue/element-plus 源码里的
  // process.env.NODE_ENV 判断会原样进 bundle，WebView 中无 process 即抛 ReferenceError
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
        'notify-card': here('./notify-card.ts'),
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
