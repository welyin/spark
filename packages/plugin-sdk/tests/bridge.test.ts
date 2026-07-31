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

  it('hello 自报身份与桥绑定不一致：握手拒绝 identity-mismatch', async () => {
    const harness = createHarness();
    const readyExpectation = expect(harness.bridge.ready).rejects.toThrow(/identity mismatch/);
    await expect(connect(harness, { pluginId: 'evil-plugin' })).rejects.toThrow(/identity mismatch/);
    await readyExpectation;
    harness.bridge.destroy();
  });

  it('重复 hello 被忽略（握手已 settle，不二次应答）', async () => {
    const harness = createHarness();
    const { sdk } = await connect(harness);
    const received: BridgeMessage[] = [];
    harness.plugin.addEventListener('message', ((event: MessageEvent) => {
      received.push(event.data as BridgeMessage);
    }) as EventListener);

    harness.host.inject(
      { v: BRIDGE_PROTOCOL_VERSION, type: 'hello', sdkVersion: '1', pluginId: 'weibo-core', viewId: 'default' },
      PLUGIN_ORIGIN,
      harness.pluginWindowRef
    );
    await new Promise((resolve) => setTimeout(resolve, 30));
    // 不再回 ready，也不回 error
    expect(received.filter((m) => m.type === 'ready' || m.type === 'error')).toHaveLength(0);
    // 桥仍可正常调用
    await sdk.docs.get('weibo_posts', 'post-1');
    expect(harness.handler).toHaveBeenCalledWith('docs', 'get', ['weibo_posts', 'post-1']);
    harness.bridge.destroy();
  });

  it('握手未 settle 时 destroy 以 destroyed 拒绝 ready（幂等）', async () => {
    const harness = createHarness();
    const readyExpectation = expect(harness.bridge.ready).rejects.toThrow('Plugin bridge destroyed');
    harness.bridge.destroy();
    await readyExpectation;
    // 二次 destroy 无操作
    harness.bridge.destroy();
  });

  it('destroy 后 ping 立即拒绝（不再等超时）', async () => {
    const harness = createHarness();
    const readyExpectation = expect(harness.bridge.ready).rejects.toThrow('Plugin bridge destroyed');
    harness.bridge.destroy();
    await expect(harness.bridge.ping(10_000)).rejects.toThrow('Plugin bridge destroyed');
    await readyExpectation;
  });
});

describe('bridge 握手前调用', () => {
  it('握手完成前的 call 一律回 not-ready，不分发给 handler', async () => {
    const harness = createHarness({ handshakeTimeoutMs: 200 });
    const readyExpectation = expect(harness.bridge.ready).rejects.toThrow(/timed out/);
    const received: BridgeMessage[] = [];
    harness.plugin.addEventListener('message', ((event: MessageEvent) => {
      received.push(event.data as BridgeMessage);
    }) as EventListener);

    // 插件侧尚未 hello 就发 call：宿主必须拒绝（避免未授权窗口期）
    harness.host.inject(
      { v: BRIDGE_PROTOCOL_VERSION, type: 'call', id: 'c-early', module: 'docs', method: 'get', args: ['c', 'i'] },
      PLUGIN_ORIGIN,
      harness.pluginWindowRef
    );
    await waitFor(() => received.length === 1);
    expect(received[0]).toMatchObject({
      type: 'result',
      id: 'c-early',
      ok: false,
      error: { code: 'not-ready' }
    });
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

describe('bridge messages 域（应用会话 §20）', () => {
  it('sendAppMessage 序列化为 messages.sendAppMessage（payload/card 透传，不含 pluginId/space）', async () => {
    const harness = createHarness();
    const { sdk } = await connect(harness);
    const payload = { summary: '新微博', text: 'hello' };
    const card = { viewId: 'post-card', data: { postId: 'p1' } };
    const result = (await sdk.messages!.sendAppMessage(payload, card)) as Record<string, unknown>;

    expect(harness.handler).toHaveBeenCalledTimes(1);
    const [module, method, args] = harness.handler.mock.calls[0] as [string, string, unknown[]];
    expect(module).toBe('messages');
    expect(method).toBe('sendAppMessage');
    // pluginId/space 由桥注入，插件侧不出现在调用参数里
    expect(args).toEqual([payload, card]);
    expect(result).toEqual({ module: 'messages', method: 'sendAppMessage', args: [payload, card] });
    harness.bridge.destroy();
  });

  it('listAppMessages/markRead 序列化（无参调用）', async () => {
    const harness = createHarness();
    const { sdk } = await connect(harness);
    await sdk.messages!.listAppMessages();
    await sdk.messages!.markRead();
    const calls = harness.handler.mock.calls.map((call) => [call[0], call[1], call[2]]);
    expect(calls).toEqual([
      ['messages', 'listAppMessages', []],
      ['messages', 'markRead', []]
    ]);
    harness.bridge.destroy();
  });

  it('onCardAction：宿主 pushAction 下发到主视图回调；注销后不再接收', async () => {
    const harness = createHarness();
    const { sdk } = await connect(harness);
    const received: unknown[] = [];
    const off = sdk.messages!.onCardAction((action) => received.push(action));

    harness.bridge.pushAction('weibo-core:m1', 'open', { postId: 'p1' });
    await waitFor(() => received.length === 1);
    expect(received[0]).toEqual({ cardId: 'weibo-core:m1', actionId: 'open', data: { postId: 'p1' } });

    off();
    harness.bridge.pushAction('weibo-core:m2', 'like');
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(received).toHaveLength(1);
    harness.bridge.destroy();
  });

  it('triggerCardAction：message-card 视图 action 上行（cardId 取自握手 ctx）', async () => {
    const cardCtx: PluginContext = {
      ...TEST_CTX,
      viewId: 'post-card',
      mount: { viewType: 'message-card', cardId: 'weibo-core:m1', cardData: { postId: 'p1' } }
    };
    const onAction = vi.fn();
    const harness = createHarness({ ctx: cardCtx, viewId: 'post-card', onAction });
    const { sdk, ctx } = await connect(harness, { viewId: 'post-card' });
    expect(ctx.mount.cardId).toBe('weibo-core:m1');

    sdk.messages!.triggerCardAction('like', { value: 1 });
    await waitFor(() => onAction.mock.calls.length === 1);
    expect(onAction).toHaveBeenCalledWith('weibo-core:m1', 'like', { value: 1 });
    harness.bridge.destroy();
  });

  it('triggerCardAction/requestCardHeight：非 message-card 视图（无 cardId）抛错', async () => {
    const harness = createHarness();
    const { sdk } = await connect(harness);
    expect(() => sdk.messages!.triggerCardAction('like')).toThrow(/message-card/);
    expect(() => sdk.messages!.requestCardHeight(240)).toThrow(/message-card/);
    harness.bridge.destroy();
  });

  it('requestCardHeight：message-card 视图经 event card-resize 上行', async () => {
    const cardCtx: PluginContext = {
      ...TEST_CTX,
      viewId: 'post-card',
      mount: { viewType: 'message-card', cardId: 'weibo-core:m1' }
    };
    const onEvent = vi.fn();
    const harness = createHarness({ ctx: cardCtx, viewId: 'post-card', onEvent });
    const { sdk } = await connect(harness, { viewId: 'post-card' });

    sdk.messages!.requestCardHeight(240);
    await waitFor(() => onEvent.mock.calls.length === 1);
    expect(onEvent).toHaveBeenCalledWith('card-resize', { height: 240 });
    harness.bridge.destroy();
  });
});
