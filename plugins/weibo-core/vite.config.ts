/**
 * weibo-core 插件产物构建配置（插件 iframe 沙箱化阶段 A）。
 *
 * vite lib 模式产出 ESM 单文件 dist/views/main.js：
 * - @spark/plugin-sdk 为零依赖源码包，经相对路径引入直接打进 bundle；
 * - vue / element-plus 同样打进 bundle（框架自包含原则：插件间版本互不干扰，
 *   CSP 禁止远程 script-src，不允许运行时外引任何代码），无 external；
 * - manifest.json 与静态资源由 scripts/copy-weibo-dist.mjs 在构建后拷贝。
 *
 * plugins 目录不持有 node_modules（不新增 npm 依赖）：vite 本体由
 * plugins/package.json 的 build:weibo 脚本经 node 直接调起
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
      entry: here('./index.ts'),
      formats: ['es'],
      // 单文件 ESM：dist/views/main.js
      fileName: () => 'views/main.js'
    },
    rollupOptions: {
      output: {
        inlineDynamicImports: true
      }
    }
  }
};
