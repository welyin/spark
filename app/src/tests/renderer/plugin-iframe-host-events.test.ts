/**
 * PluginIframeHost 事件转发（A18，communication §4.1）：
 * - ChatReceived：绑定 space 匹配 + messages:read 授权 → 桥事件（否则不推）；
 * - ContactsSynced：contacts:read 授权 → 桥事件（否则不推）；
 * - FeedReceived：topic 前缀匹配 + feed:read 授权 → 桥事件（A18 前免权限，现门控）；
 * - PluginDataChanged：pluginId 匹配 → 桥事件（既有行为回归）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h, nextTick, type App } from 'vue';
import { ElButton, ElIcon } from 'element-plus';
import PluginIframeHost from '../../components/plugin/PluginIframeHost.vue';
import { pluginInstanceKey } from '../../plugin/disabled';

type FakeHost = {
  ready: Promise<unknown>;
  pushEvent: ReturnType<typeof vi.fn>;
  ping: ReturnType<typeof vi.fn>;
  destroy: ReturnType<typeof vi.fn>;
  resolveReady: () => void;
};

// vi.mock 工厂提升执行，托管数组须经 vi.hoisted 声明
const createdHosts = vi.hoisted(() => [] as FakeHost[]);
/** listenP2pEvents 捕获的 p2p-event 处理器（init 注册，测试触发） */
const p2pHandlers = vi.hoisted(() => [] as Array<(event: { kind: string; data: unknown }) => void>);

vi.mock('../../../../packages/plugin-sdk/src/bridge/host', () => ({
  createBridgeHost: vi.fn(() => {
    let resolveReady!: (value: unknown) => void;
    let rejectReady!: (reason?: unknown) => void;
    const ready = new Promise((resolve, reject) => {
      resolveReady = resolve;
      rejectReady = reject;
    });
    const host: FakeHost = {
      ready,
      pushEvent: vi.fn(),
      ping: vi.fn(() => Promise.resolve()),
      destroy: vi.fn(() => rejectReady(new Error('Plugin bridge destroyed'))),
      resolveReady: () => resolveReady({})
    };
    createdHosts.push(host);
    return host;
  })
}));

vi.mock('../../plugin/source', () => ({
  buildPluginHostSrcdoc: vi.fn(() => '<!doctype html><html><body></body></html>'),
  fetchPluginManifest: vi.fn(async () => null)
}));

vi.mock('../../plugin/bridge-dispatcher', () => ({
  createPluginBridgeDispatcher: vi.fn(async () => async () => null),
  setBridgeEventPump: vi.fn()
}));

// listenP2pEvents 捕获（其余 api 导出保持真实：plugin/disabled、stores 等依赖原样）
vi.mock('../../api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../api')>();
  return {
    ...actual,
    listenP2pEvents: vi.fn(async (handler: (event: { kind: string; data: unknown }) => void) => {
      p2pHandlers.push(handler);
      return () => {};
    })
  };
});

const SPACE = { type: 'personal', id: 'personal' } as const;

function mockGranted(permissions: string[], pluginId = 'spark-example'): void {
  (window as any).electronAPI = {
    ...(window as any).electronAPI,
    pluginMarket: {
      list: async () => [{ id: pluginId, grantedPermissions: permissions }]
    }
  };
}

async function flush(): Promise<void> {
  await nextTick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await nextTick();
}

function mountHost(pluginId = 'spark-example'): { el: HTMLElement; app: App } {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp({
    render: () => h(PluginIframeHost, { pluginId, viewId: 'default', space: { ...SPACE } })
  });
  app.component('el-button', ElButton);
  app.component('el-icon', ElIcon);
  app.mount(el);
  return { el, app };
}

function fire(event: { kind: string; data: unknown }): void {
  for (const handler of [...p2pHandlers]) {
    handler(event);
  }
}

function lastHost(): FakeHost {
  return createdHosts[createdHosts.length - 1];
}

beforeEach(() => {
  localStorage.clear();
  createdHosts.length = 0;
  p2pHandlers.length = 0;
  vi.clearAllMocks();
});

afterEach(() => {
  document.body.innerHTML = '';
});

describe('PluginIframeHost 事件转发（A18）', () => {
  it('ChatReceived：messages:read 授权 + space 匹配 → 推送；space 不匹配/未授权 → 不推', async () => {
    mockGranted(['messages:read']);
    mountHost();
    await flush();
    lastHost().resolveReady();
    await flush();
    const push = lastHost().pushEvent;
    const chat = (spaceKey: string) => ({
      kind: 'ChatReceived',
      data: { spaceKey, conversation: { id: 'c1' }, message: { id: 'm1', content: 'hi' } }
    });
    fire(chat('personal'));
    expect(push).toHaveBeenCalledWith('ChatReceived', chat('personal').data);
    push.mockClear();
    // space 不匹配（本实例绑定 personal）
    fire(chat('org:org_1'));
    expect(push).not.toHaveBeenCalled();
    // 无关事件不推
    fire({ kind: 'KeepaliveTick', data: {} });
    expect(push).not.toHaveBeenCalled();
    // A19 事件面正例：ChatStatus（space 匹配）→ 推送
    const status = { kind: 'ChatStatus', data: { spaceKey: 'personal', convId: 'c1', messageId: 'm1', status: 'read' } };
    fire(status);
    expect(push).toHaveBeenCalledWith('ChatStatus', status.data);
    push.mockClear();
    // ChatStatus space 不匹配 → 不推
    fire({ kind: 'ChatStatus', data: { spaceKey: 'org:org_1', convId: 'c1' } });
    expect(push).not.toHaveBeenCalled();
    // ConversationsSynced / PeerConnected（同 messages:read 门控）→ 推送
    fire({ kind: 'ConversationsSynced', data: { applied: 1 } });
    expect(push).toHaveBeenCalledWith('ConversationsSynced', { applied: 1 });
    fire({ kind: 'PeerConnected', data: { peerId: 'p1' } });
    expect(push).toHaveBeenCalledWith('PeerConnected', { peerId: 'p1' });
  });

  it('ChatReceived：无 messages:read 授权 → 不推', async () => {
    mockGranted([]);
    mountHost();
    await flush();
    lastHost().resolveReady();
    await flush();
    fire({ kind: 'ChatReceived', data: { spaceKey: 'personal', conversation: {}, message: {} } });
    expect(lastHost().pushEvent).not.toHaveBeenCalled();
  });

  it('ContactsSynced：contacts:read 授权 → 推送；未授权 → 不推', async () => {
    mockGranted(['contacts:read']);
    mountHost();
    await flush();
    lastHost().resolveReady();
    await flush();
    fire({ kind: 'ContactsSynced', data: { applied: 2 } });
    expect(lastHost().pushEvent).toHaveBeenCalledWith('ContactsSynced', { applied: 2 });

    // 未授权场景：重挂一个独立插件
    document.body.innerHTML = '';
    createdHosts.length = 0;
    p2pHandlers.length = 0;
    mockGranted([], 'spark-other');
    mountHost('spark-other');
    await flush();
    lastHost().resolveReady();
    await flush();
    fire({ kind: 'ContactsSynced', data: { applied: 1 } });
    expect(lastHost().pushEvent).not.toHaveBeenCalled();
  });

  it('FeedReceived：topic 前缀匹配 + feed:read 授权 → 推送；未授权 feed:read → 不推', async () => {
    mockGranted(['feed:read']);
    mountHost();
    await flush();
    lastHost().resolveReady();
    await flush();
    const push = lastHost().pushEvent;
    const feed = { kind: 'FeedReceived', data: { topic: 'spark-example:posts', feedId: 'f1', from: 'bob', payload: {}, ts: 1 } };
    fire(feed);
    expect(push).toHaveBeenCalledWith('FeedReceived', feed.data);
    push.mockClear();
    // topic 前缀 != 插件 id → 不推
    fire({ kind: 'FeedReceived', data: { topic: 'other:posts', feedId: 'f2' } });
    expect(push).not.toHaveBeenCalled();
  });

  it('FeedReceived：无 feed:read 授权 → 不推（A18 门控）', async () => {
    mockGranted([]);
    mountHost();
    await flush();
    lastHost().resolveReady();
    await flush();
    fire({ kind: 'FeedReceived', data: { topic: 'spark-example:posts', feedId: 'f1' } });
    expect(lastHost().pushEvent).not.toHaveBeenCalled();
  });

  it('PluginDataChanged：pluginId 匹配 → 推送（既有行为回归）', async () => {
    mockGranted([]);
    mountHost();
    await flush();
    lastHost().resolveReady();
    await flush();
    const data = { pluginId: 'spark-example', name: 'spark-example:posts', keys: ['k1'] };
    fire({ kind: 'PluginDataChanged', data });
    expect(lastHost().pushEvent).toHaveBeenCalledWith('PluginDataChanged', data);
  });

  it('实例键不参与事件判定（disabled 状态机不受影响）', () => {
    // 仅保证 pluginInstanceKey 可用（防止 mock 破坏 disabled 真实模块）
    expect(pluginInstanceKey('spark-example', SPACE)).toContain('spark-example');
  });
});
