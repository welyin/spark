/**
 * 统一深链携域切域（problem D9）回归：
 * - payload 携带 space 时，openPluginDeepLink 先切到目标域再派发 spark:open-plugin 事件
 *   （监听器与上下文条读取到的都是切域后的状态）；
 * - 已在目标域时不重复切换；不带 space 时维持现状（当前域打开）。
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { OPEN_PLUGIN_DEEPLINK_EVENT, openPluginDeepLink } from '../../services/deep-link';
import { currentSpace, switchToOrg, switchToPersonal } from '../../stores/current-space';

function listenOnce() {
  const seen: Array<{ detail: unknown; spaceAtEvent: unknown }> = [];
  const handler = (event: Event) => {
    seen.push({
      detail: (event as CustomEvent).detail,
      // 断言事件到达监听器时 currentSpace 已是目标域
      spaceAtEvent: currentSpace.value
    });
  };
  window.addEventListener(OPEN_PLUGIN_DEEPLINK_EVENT, handler);
  return { seen, off: () => window.removeEventListener(OPEN_PLUGIN_DEEPLINK_EVENT, handler) };
}

beforeEach(() => {
  localStorage.clear();
  switchToPersonal();
});

describe('openPluginDeepLink · D9 携域切域', () => {
  it('携带 org 空间：先切到该域再派发事件（监听器读到新域）', () => {
    const { seen, off } = listenOnce();
    openPluginDeepLink({ pluginId: 'spark-affairs', space: { type: 'org', orgId: 'org-a' } });
    off();
    expect(currentSpace.value).toEqual({ type: 'org', orgId: 'org-a' });
    expect(seen).toHaveLength(1);
    expect(seen[0].spaceAtEvent).toEqual({ type: 'org', orgId: 'org-a' });
    expect((seen[0].detail as { pluginId: string }).pluginId).toBe('spark-affairs');
  });

  it('携带 personal：从组织空间切回个人空间', () => {
    switchToOrg('org-a');
    const { seen, off } = listenOnce();
    openPluginDeepLink({ pluginId: 'spark-affairs', space: { type: 'personal' } });
    off();
    expect(currentSpace.value).toEqual({ type: 'personal' });
    expect(seen[0].spaceAtEvent).toEqual({ type: 'personal' });
  });

  it('已在目标域：不重复切换（localStorage 仅写一次语义），事件照常派发', () => {
    switchToOrg('org-a');
    const setSpy = vi.spyOn(Storage.prototype, 'setItem');
    const { seen, off } = listenOnce();
    openPluginDeepLink({ pluginId: 'p1', space: { type: 'org', orgId: 'org-a' } });
    off();
    expect(seen).toHaveLength(1);
    expect(setSpy).not.toHaveBeenCalledWith('spark:current-space', expect.anything());
    setSpy.mockRestore();
  });

  it('不带 space：不切域，在当前域派发', () => {
    switchToOrg('org-b');
    const { seen, off } = listenOnce();
    openPluginDeepLink({ pluginId: 'p2' });
    off();
    expect(currentSpace.value).toEqual({ type: 'org', orgId: 'org-b' });
    expect(seen).toHaveLength(1);
  });
});
