import { defineConfig } from 'vitest/config';
import vue from '@vitejs/plugin-vue';
import { fileURLToPath } from 'node:url';

const here = (path: string): string => fileURLToPath(new URL(path, import.meta.url));

// 对齐旧工程 desktop/vitest.config.ts（TS 版）；被测的插件源码与 SDK 包在工程根之外
// （code/plugins、code/packages），经 server.fs.allow 放开上级目录（与 vite.config.ts 一致）。
// resolve.alias：plugins 目录不持有 node_modules（插件组件测试从 code/plugins 引用
// vue/element-plus 时按本目录锚定解析，与各插件 vite.config 的产物构建同口径）。
export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: [
      // plugins 目录不持有 node_modules：插件组件测试从 code/plugins 引用
      // element-plus 时按本目录锚定解析（与各插件 vite.config 产物构建同口径；
      // vue 经 vitest 预打包缓存本就可解析，别名 vue 会让全量跑的 worker 崩溃）
      { find: 'element-plus', replacement: here('./node_modules/element-plus') },
      { find: '@element-plus/icons-vue', replacement: here('./node_modules/@element-plus/icons-vue') }
    ]
  },
  server: {
    fs: {
      allow: ['..']
    }
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['src/test-setup.ts'],
    include: ['src/**/*.test.ts', '../plugins/**/*.test.ts', '../packages/**/*.test.ts']
  }
});
