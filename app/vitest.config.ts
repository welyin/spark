import { defineConfig } from 'vitest/config';
import vue from '@vitejs/plugin-vue';

// 对齐旧工程 desktop/vitest.config.ts（TS 版）；被测的插件源码在工程根之外
// （code/plugins），经 server.fs.allow 放开上级目录（与 vite.config.ts 一致）。
export default defineConfig({
  plugins: [vue()],
  server: {
    fs: {
      allow: ['..']
    }
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['src/test-setup.ts'],
    include: ['src/**/*.test.ts', '../plugins/**/*.test.ts']
  }
});
