/**
 * 最小聊天插件（spark-minichat）产物构建配置（对齐 spark-chat lib 模式；
 * 无 element-plus 依赖——原生 CSS 最小示例，bundle 只含 vue + SDK）。
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
    alias: [{ find: 'vue', replacement: here('../../app/node_modules/vue') }]
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
