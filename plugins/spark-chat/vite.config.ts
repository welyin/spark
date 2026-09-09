/**
 * 聊天应用插件（spark-chat）产物构建配置（对齐 spark-moments lib 模式）。
 *
 * vite lib 模式产出 ESM bundle：dist/views/main.js 主入口（壳层 srcdoc 固定
 * 加载），dist/chunks/*.js 共享代码 chunk。@spark/plugin-sdk 与
 * vue/element-plus 全部打进 bundle（框架自包含），无 external；
 * plugins 目录不持有 node_modules，依赖副本锚定 code/app/node_modules。
 */
import vue from '../../app/node_modules/@vitejs/plugin-vue/dist/index.mjs';
import { fileURLToPath } from 'node:url';

const here = (path) => fileURLToPath(new URL(path, import.meta.url));

export default {
  root: here('./'),
  plugins: [vue()],
  define: {
    'process.env.NODE_ENV': JSON.stringify('production')
  },
  resolve: {
    alias: [
      { find: 'vue', replacement: here('../../app/node_modules/vue') },
      { find: 'element-plus', replacement: here('../../app/node_modules/element-plus') },
      { find: '@element-plus/icons-vue', replacement: here('../../app/node_modules/@element-plus/icons-vue') }
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
