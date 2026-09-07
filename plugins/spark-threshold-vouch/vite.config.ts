/**
 * 担保链门槛示例（spark-threshold-vouch）产物构建配置。
 *
 * 对齐 spark-example/vite.config.ts（插件体系统一构建约定）：vite lib 模式
 * 产出 dist/views/main.js（单视图插件，无 message-card 入口）；vue /
 * element-plus / @spark/plugin-sdk 全部打进 bundle（框架自包含，无 external）；
 * 依赖副本经绝对路径锚定到 code/app/node_modules。
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
        main: here('./index.ts')
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
