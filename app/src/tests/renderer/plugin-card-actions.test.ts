/**
 * 卡片按钮回调路由（plugin-card-actions）单测：
 * - 归属登记/卸载：cardId 必须是壳层签发且当前在架的；
 * - 主视图实例缺失返回 false（action 丢弃，设计允许）；
 * - 同插件同空间换实例：unregister 的 === host 守卫不误删新实例；
 * - 空间精确匹配：cardId 编入 spaceKey，跨空间同插件卡片互不串投。
 */
import { describe, expect, it, vi } from 'vitest';
import type { BridgeHost } from '../../../../packages/plugin-sdk/src/bridge/host';
import {
  pluginSpaceKey,
  registerCard,
  registerMainViewInstance,
  routeCardAction,
  unregisterCard,
  unregisterMainViewInstance
} from '../../plugin-card-actions';

/** BridgeHost 桩：路由链路只用到 pushAction */
function makeHost(): BridgeHost & { pushAction: ReturnType<typeof vi.fn> } {
  return { pushAction: vi.fn() } as unknown as BridgeHost & { pushAction: ReturnType<typeof vi.fn> };
}

describe('pluginSpaceKey（与 mock SpaceKey/桥注入 boundSpaceKey 同口径）', () => {
  it('个人空间 personal；组织空间 org:<orgId>', () => {
    expect(pluginSpaceKey({ type: 'personal', id: 'personal' })).toBe('personal');
    expect(pluginSpaceKey({ type: 'org', id: 'org_1' })).toBe('org:org_1');
  });
});

describe('卡片归属登记', () => {
  it('登记后可路由送达；卸载后归属失效返回 false', () => {
    const host = makeHost();
    registerMainViewInstance('weibo-core', 'personal', host);
    registerCard('weibo-core:personal:m1', 'weibo-core', 'personal');

    expect(routeCardAction('weibo-core', 'personal', 'weibo-core:personal:m1', 'like')).toBe(true);
    expect(host.pushAction).toHaveBeenCalledWith('weibo-core:personal:m1', 'like', undefined);

    unregisterCard('weibo-core:personal:m1');
    expect(routeCardAction('weibo-core', 'personal', 'weibo-core:personal:m1', 'like')).toBe(false);
    unregisterMainViewInstance('weibo-core', 'personal', host);
  });

  it('归属不符（他插件/他空间冒用在架 cardId）返回 false', () => {
    const host = makeHost();
    registerMainViewInstance('weibo-core', 'personal', host);
    registerCard('weibo-core:personal:m2', 'weibo-core', 'personal');

    expect(routeCardAction('evil', 'personal', 'weibo-core:personal:m2', 'x')).toBe(false);
    expect(routeCardAction('weibo-core', 'org:org_1', 'weibo-core:personal:m2', 'x')).toBe(false);
    expect(host.pushAction).not.toHaveBeenCalled();

    unregisterCard('weibo-core:personal:m2');
    unregisterMainViewInstance('weibo-core', 'personal', host);
  });
});

describe('主视图实例路由', () => {
  it('主实例未运行：action 丢弃返回 false', () => {
    registerCard('weibo-core:personal:m3', 'weibo-core', 'personal');
    expect(routeCardAction('weibo-core', 'personal', 'weibo-core:personal:m3', 'like', { v: 1 })).toBe(false);
    unregisterCard('weibo-core:personal:m3');
  });

  it('同插件同空间换实例：后登记覆盖先登记，旧实例迟到的 unregister（=== host 守卫）不误删新实例', () => {
    const oldHost = makeHost();
    const newHost = makeHost();
    registerMainViewInstance('weibo-core', 'personal', oldHost);
    registerMainViewInstance('weibo-core', 'personal', newHost);
    unregisterMainViewInstance('weibo-core', 'personal', oldHost); // 旧实例卸载晚到

    registerCard('weibo-core:personal:m4', 'weibo-core', 'personal');
    expect(routeCardAction('weibo-core', 'personal', 'weibo-core:personal:m4', 'like')).toBe(true);
    expect(newHost.pushAction).toHaveBeenCalledTimes(1);
    expect(oldHost.pushAction).not.toHaveBeenCalled();

    unregisterCard('weibo-core:personal:m4');
    unregisterMainViewInstance('weibo-core', 'personal', newHost);
  });

  it('空间精确匹配：同插件跨空间各自路由，同 messageId 的 cardId 跨空间不撞', () => {
    const personalHost = makeHost();
    const orgHost = makeHost();
    registerMainViewInstance('weibo-core', 'personal', personalHost);
    registerMainViewInstance('weibo-core', 'org:org_1', orgHost);
    registerCard('weibo-core:personal:m5', 'weibo-core', 'personal');
    registerCard('weibo-core:org:org_1:m5', 'weibo-core', 'org:org_1');

    expect(routeCardAction('weibo-core', 'personal', 'weibo-core:personal:m5', 'like')).toBe(true);
    expect(personalHost.pushAction).toHaveBeenCalledTimes(1);
    expect(orgHost.pushAction).not.toHaveBeenCalled();

    expect(routeCardAction('weibo-core', 'org:org_1', 'weibo-core:org:org_1:m5', 'like')).toBe(true);
    expect(orgHost.pushAction).toHaveBeenCalledTimes(1);

    // 空间边界：org 卡片不投给 personal 实例（owner.spaceKey 不符）
    expect(routeCardAction('weibo-core', 'personal', 'weibo-core:org:org_1:m5', 'like')).toBe(false);
    expect(personalHost.pushAction).toHaveBeenCalledTimes(1);

    unregisterCard('weibo-core:personal:m5');
    unregisterCard('weibo-core:org:org_1:m5');
    unregisterMainViewInstance('weibo-core', 'personal', personalHost);
    unregisterMainViewInstance('weibo-core', 'org:org_1', orgHost);
  });
});
