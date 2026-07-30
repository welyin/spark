/**
 * 桥协议测试（postMessage 双端模拟）。
 *
 * jsdom 的 window.postMessage 不填充 MessageEvent.origin/source（jsdom 19），
 * 而 origin/source 校验正是本桥的安全核心，因此这里用一对交叉链接的伪造
 * 端点模拟宿主/插件两个窗口：postMessage 经 setTimeout 异步派发 MessageEvent
 * （对齐真实 postMessage 语义），并保留 inject 入口供伪造 origin/source 的
 * 拒绝用例直接注入消息。
 */

import { connectPluginBridge, type WindowMessageEndpoint } from '../src/bridge/client';
import { createBridgeHost, type BridgeHost, type BridgeHostHandler } from '../src/bridge/host';
import {
  BRIDGE_ERROR_SDK_VERSION_INCOMPATIBLE,
  BRIDGE_PROTOCOL_VERSION,
  isSdkVersionCompatible,
  parseBridgeMessage,
  type BridgeMessage
} from '../src/bridge/protocol';
import type { PluginContext } from '../src/index';

const HOST_ORIGIN = 'https://host.spark';
const PLUGIN_ORIGIN = 'https://plugin.spark';

type FakeWindowEnd = WindowMessageEndpoint & {
  /** 测试注入入口：以任意 origin/source 直接向本端监听者派发消息 */
  inject: (data: unknown, origin: string, source: unknown) => void;
};

type WindowPair = {
  /** 宿主（壳层）窗口监听端 */
  host: FakeWindowEnd;
  /** 插件 iframe 窗口监听端 */
  plugin: FakeWindowEnd;
  /** 宿主窗口句柄（插件侧视角，= window.parent）：postMessage 派发给宿主监听者 */
  hostWindowRef: WindowMessageEndpoint;
  /** 插件窗口句柄（宿主侧视角，= iframe.contentWindow）：postMessage 派发给插件监听者 */
  pluginWindowRef: WindowMessageEndpoint;
};

/** 构造一对交叉链接的窗口端点（模拟跨 origin 的宿主/插件双端 postMessage） */
function createWindowPair(): WindowPair {
  const hostListeners = new Set<EventListener>();
  const pluginListeners = new Set<EventListener>();

  const dispatch = (listeners: Set<EventListener>, data: unknown, origin: string, source: unknown): void => {
    const event = new MessageEvent('message', { data });
    Object.defineProperty(event, 'origin', { value: origin });
    Object.defineProperty(event, 'source', { value: source });
    for (const listener of [...listeners]) {
      listener(event);
    }
  };
  const clone = (data: unknown): unknown =>
    data === undefined ? undefined : (JSON.parse(JSON.stringify(data)) as unknown);

  // 结构化克隆 + 异步派发（对齐真实 postMessage 语义）
  const hostWindowRef: WindowMessageEndpoint = {
    postMessage: (data: unknown) =>
      void setTimeout(() => dispatch(hostListeners, clone(data), PLUGIN_ORIGIN, pluginWindowRef), 0)
  } as WindowMessageEndpoint;
  const pluginWindowRef: WindowMessageEndpoint = {
    postMessage: (data: unknown) =>
      void setTimeout(() => dispatch(pluginListeners, clone(data), HOST_ORIGIN, hostWindowRef), 0)
  } as WindowMessageEndpoint;

  const makeEnd = (listeners: Set<EventListener>): FakeWindowEnd => ({
    addEventListener: ((type: string, listener: EventListener) => {
      if (type === 'message') {
        listeners.add(listener);
      }
    }) as WindowMessageEndpoint['addEventListener'],
    removeEventListener: ((type: string, listener: EventListener) => {
      listeners.delete(listener);
    }) as WindowMessageEndpoint['removeEventListener'],
    postMessage: () => {
      throw new Error('测试端点不支持向自身 postMessage，请使用对端窗口句柄');
    },
    inject: (data, origin, source) => dispatch(listeners, data, origin, source)
  });

  return { host: makeEnd(hostListeners), plugin: makeEnd(pluginListeners), hostWindowRef, pluginWindowRef };
}

const TEST_CTX: PluginContext = {
  pluginId: 'weibo-core',
  viewId: 'default',
  domain: 'plugin:weibo-core',
  space: { type: 'org', id: 'org-1' },
  theme: 'light',
  mount: { viewType: 'app' }
};

type Harness = WindowPair & {
  handler: ReturnType<typeof vi.fn>;
  bridge: BridgeHost;
};

/** 搭好宿主侧（插件侧由各用例按需 connect） */
function createHarness(overrides: Record<string, unknown> = {}): Harness {
  const pair = createWindowPair();
  const handler = vi.fn(async (module: string, method: string, args: unknown[]) => ({ module, method, args }));
  const bridge = createBridgeHost({
    iframe: { contentWindow: pair.pluginWindowRef as unknown as Window },
    pluginId: 'weibo-core',
    viewId: 'default',
    expectedOrigin: PLUGIN_ORIGIN,
    ctx: TEST_CTX,
    handler: handler as unknown as BridgeHostHandler,
    listenWindow: pair.host,
    ...overrides
  });
  return { ...pair, handler, bridge };
}

function connect(harness: Harness, overrides: Record<string, unknown> = {}) {
  return connectPluginBridge({
    pluginId: 'weibo-core',
    viewId: 'default',
    sdkVersion: '1',
    listenWindow: harness.plugin,
    hostWindow: harness.hostWindowRef,
    targetOrigin: HOST_ORIGIN,
    ...overrides
  });
}

async function waitFor(condition: () => boolean, timeoutMs = 1_000): Promise<void> {
  const startedAt = Date.now();
  while (!condition()) {
    if (Date.now() - startedAt > timeoutMs) {
      throw new Error('waitFor 超时');
    }
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

describe('bridge/protocol 消息编解码', () => {
  it('合法消息经 JSON 往返后可解析', () => {
    const messages: BridgeMessage[] = [
      { v: BRIDGE_PROTOCOL_VERSION, type: 'hello', sdkVersion: '1', pluginId: 'weibo-core', viewId: 'default' },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'ready', sdkVersion: '1', ctx: TEST_CTX },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'error', code: 'x', message: 'm' },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'call', id: 'c1', module: 'docs', method: 'get', args: ['c', 'i'] },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'result', id: 'c1', ok: true, data: { value: 1 } },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'result', id: 'c2', ok: false, error: { code: 'call-failed', message: 'boom' } },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'subscribe', id: 's1', event: 'theme-changed' },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'unsubscribe', id: 'u1', event: 'theme-changed' },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'event', event: 'theme-changed', payload: { theme: 'dark' } },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'ping', id: 'p1' },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'pong', id: 'p1' },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'action', cardId: 'card1', actionId: 'open', data: { k: 1 } }
    ];
    for (const message of messages) {
      const roundTripped: unknown = JSON.parse(JSON.stringify(message));
      expect(parseBridgeMessage(roundTripped)).toEqual(message);
    }
  });

  it('非法消息一律返回 null', () => {
    const base = { v: BRIDGE_PROTOCOL_VERSION, type: 'call', id: 'c1', module: 'docs', method: 'get', args: [] };
    const invalid: unknown[] = [
      null,
      undefined,
      42,
      'hello',
      [],
      {},
      { ...base, v: 2 }, // 协议版本不符
      { ...base, v: '1' }, // v 必须是数字
      { ...base, type: 'unknown-type' },
      { ...base, id: '' }, // 必填字段为空
      { ...base, args: 'not-array' },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'result', id: 'x', ok: 'yes' },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'result', id: 'x', ok: false, error: { code: 1 } },
      { v: BRIDGE_PROTOCOL_VERSION, type: 'hello', sdkVersion: '1', pluginId: 'p' }, // 缺 viewId
      { v: BRIDGE_PROTOCOL_VERSION, type: 'event' } // 缺 event
    ];
    for (const message of invalid) {
      expect(parseBridgeMessage(message)).toBeNull();
    }
  });

  it('SDK 契约版本协商：v1 下精确一致', () => {
    expect(isSdkVersionCompatible('1', '1')).toBe(true);
    expect(isSdkVersionCompatible('1', '2')).toBe(false);
  });
});

describe('bridge 握手', () => {
  it('握手成功：connect 返回 sdk 与 ctx', async () => {
    const harness = createHarness();
    const connection = await connect(harness);
    expect(connection.ctx).toEqual(TEST_CTX);
    expect(connection.sdk.domain).toBe('plugin:weibo-core');
    await expect(harness.bridge.ready).resolves.toEqual(TEST_CTX);
    harness.bridge.destroy();
  });

  it('SDK 版本不兼容：宿主回 error，connect 拒绝', async () => {
    const harness = createHarness();
    const readyExpectation = expect(harness.bridge.ready).rejects.toThrow(/incompatible/);
    await expect(connect(harness, { sdkVersion: '2' })).rejects.toMatchObject({
      code: BRIDGE_ERROR_SDK_VERSION_INCOMPATIBLE
    });
    await readyExpectation;
    harness.bridge.destroy();
  });

  it('握手超时：宿主 ready 拒绝', async () => {
    const harness = createHarness({ handshakeTimeoutMs: 60 });
    await expect(harness.bridge.ready).rejects.toThrow(/timed out/);
    harness.bridge.destroy();
  });

  it('伪造 origin 的消息被忽略', async () => {
    const harness = createHarness({ handshakeTimeoutMs: 120 });
    const readyExpectation = expect(harness.bridge.ready).rejects.toThrow(/timed out/);
    // origin 不符但结构合法、source 正确的 hello：必须被 origin 校验拦下
    harness.host.inject(
      { v: BRIDGE_PROTOCOL_VERSION, type: 'hello', sdkVersion: '1', pluginId: 'weibo-core', viewId: 'default' },
      'https://evil.example',
      harness.pluginWindowRef
    );
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(harness.handler).not.toHaveBeenCalled();
    await readyExpectation;
    harness.bridge.destroy();
  });

  it('伪造 source 的消息被忽略', async () => {
    const harness = createHarness({ handshakeTimeoutMs: 120 });
    const readyExpectation = expect(harness.bridge.ready).rejects.toThrow(/timed out/);
    // origin 正确但 source 不是绑定的 iframe contentWindow
    harness.host.inject(
      { v: BRIDGE_PROTOCOL_VERSION, type: 'hello', sdkVersion: '1', pluginId: 'weibo-core', viewId: 'default' },
      PLUGIN_ORIGIN,
      harness.hostWindowRef // 宿主窗口自己伪造插件消息
    );
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(harness.handler).not.toHaveBeenCalled();
    await readyExpectation;
    harness.bridge.destroy();
  });
});

describe('bridge call/result 往返', () => {
  it('sdk 方法序列化为 call 并 Promise 化等待 result', async () => {
    const harness = createHarness();
    const { sdk } = await connect(harness);

    const doc = await sdk.docs.get('weibo_posts', 'post-1');
    expect(doc).toEqual({ module: 'docs', method: 'get', args: ['weibo_posts', 'post-1'] });
    expect(harness.handler).toHaveBeenCalledWith('docs', 'get', ['weibo_posts', 'post-1']);

    await sdk.identity.sign('payload');
    expect(harness.handler).toHaveBeenCalledWith('identity', 'sign', ['payload']);
    harness.bridge.destroy();
  });

  it('handler 抛错：调用方收到带 code 的拒绝', async () => {
    const harness = createHarness({
      handler: async () => {
        throw new Error('boom');
      }
    });
    const { sdk } = await connect(harness);
    await expect(sdk.p2p.start()).rejects.toMatchObject({ message: 'boom', code: 'call-failed' });
    harness.bridge.destroy();
  });

  it('调用超时：result 未在超时内返回则拒绝', async () => {
    const harness = createHarness({ handler: () => new Promise(() => {}) });
    const { sdk } = await connect(harness, { callTimeoutMs: 60 });
    await expect(sdk.runtime.currentRoot()).rejects.toThrow(/timed out/);
    harness.bridge.destroy();
  });

  it('伪造 source 的 result 不会解开待决调用', async () => {
    const harness = createHarness({ handler: () => new Promise(() => {}) });
    const { sdk } = await connect(harness, { callTimeoutMs: 120 });
    const pending = sdk.docs.get('c', 'i');
    const expectation = expect(pending).rejects.toThrow(/timed out/);
    // 宿主窗口之外的来源伪造 result：客户端必须丢弃
    harness.plugin.inject(
      { v: BRIDGE_PROTOCOL_VERSION, type: 'result', id: 'call-forged', ok: true, data: {} },
      HOST_ORIGIN,
      harness.pluginWindowRef
    );
    await expectation;
    harness.bridge.destroy();
  });
});

describe('bridge 心跳与事件', () => {
  it('宿主 ping：插件侧自动应答 pong', async () => {
    const harness = createHarness();
    await connect(harness);
    await expect(harness.bridge.ping(500)).resolves.toBeUndefined();
    harness.bridge.destroy();
  });

  it('插件未连接时 ping 超时', async () => {
    const harness = createHarness({ handshakeTimeoutMs: 200 });
    const readyExpectation = expect(harness.bridge.ready).rejects.toThrow(/timed out/);
    await expect(harness.bridge.ping(50)).rejects.toThrow(/timed out/);
    await readyExpectation;
    harness.bridge.destroy();
  });

  it('事件订阅：宿主 pushEvent 分发到插件回调', async () => {
    const harness = createHarness();
    const { sdk } = await connect(harness);
    const received: unknown[] = [];
    await sdk.events!.subscribe('theme-changed', (payload) => received.push(payload));

    harness.bridge.pushEvent('theme-changed', { theme: 'dark' });
    await waitFor(() => received.length === 1);
    expect(received[0]).toEqual({ theme: 'dark' });
    harness.bridge.destroy();
  });

  it('未订阅的事件不下发', async () => {
    const harness = createHarness();
    const { sdk } = await connect(harness);
    const received: unknown[] = [];
    await sdk.events!.subscribe('theme-changed', (payload) => received.push(payload));

    harness.bridge.pushEvent('space-changed', { spaceId: 'org-2' });
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(received).toHaveLength(0);

    await sdk.events!.unsubscribe('theme-changed');
    harness.bridge.pushEvent('theme-changed', { theme: 'dark' });
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(received).toHaveLength(0);
    harness.bridge.destroy();
  });
});
