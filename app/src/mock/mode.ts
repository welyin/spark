/**
 * 全局 mock 模式开关：仅 `VITE_MOCK=1`（即 `npm run tauri:mock`）时为 true。
 * 默认（`npm run tauri dev`）走真实内核数据。
 */
export function mockMode(): boolean {
  return import.meta.env.VITE_MOCK === '1';
}
