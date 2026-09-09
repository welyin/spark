import { describe, expect, it, vi } from 'vitest';
vi.mock('./app-registry', async () => {
  const { ref } = await import('vue');
  return { desktopSpaceId: ref('personal') };
});
import { desktopSpaceId } from './app-registry';
import { closeWindow, openNewWindow, restorePersistedWindows, windowGroups, windows } from './window-manager';
import type { Ref } from 'vue';

describe('desktop window space isolation', () => {
  it('keeps instances across spaces and closes a window in its owning space', () => {
    const first = openNewWindow('app-a');
    const second = openNewWindow('app-a');
    (desktopSpaceId as Ref<string>).value = 'org-preview';
    const orgWindow = openNewWindow('app-a');
    expect(windows.value.map((item) => item.key)).toEqual([orgWindow]);
    expect(windowGroups.value.flatMap((group) => group.windows)).toHaveLength(3);
    closeWindow(first);
    expect(windows.value).toHaveLength(1);
    (desktopSpaceId as Ref<string>).value = 'personal';
    restorePersistedWindows();
    expect(windows.value.map((item) => item.key)).toEqual([second]);
  });
});