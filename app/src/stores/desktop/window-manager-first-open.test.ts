import { expect, it, vi } from 'vitest';
import { nextTick, watch } from 'vue';

it('first rendered empty desktop reacts to its first opened window', async () => {
  vi.resetModules();
  const manager = await import('./window-manager');
  const counts: number[] = [];
  const stop = watch(manager.windows, (items) => counts.push(items.length), { immediate: true });
  expect(counts).toEqual([0]);
  manager.openWindow('spark:market');
  await nextTick();
  expect(counts).toEqual([0, 1]);
  stop();
});