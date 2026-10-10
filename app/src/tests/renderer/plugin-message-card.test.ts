/**
 * AppMessageCard 组件级测试（桥/源/dispatcher 均 mock）：
 * message-card 是 withPluginShellEnv 的第二调用点（A57 评审建议 3）——
 * 握手 ctx 注入断言与 PluginIframeHost 路径同口径。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h, nextTick, type App } from 'vue';
import { ElIcon } from 'element-plus';
import AppMessageCard from '../../components/messages/AppMessageCard.vue';
import { fetchPluginManifest } from '../../plugin/source';
import { createBridgeHost } from '../../../../packages/plugin-sdk/src/bridge/host';
import pkg from '../../../package.json';

type FakeHost = {
  ready: Promise<unknown>;
  pushEvent: ReturnType<typeof vi.fn>;
  pushAction: ReturnType<typeof vi.fn>;
  ping: ReturnType<typeof vi.fn>;
  destroy: ReturnType<typeof vi.fn>;
};

// vi.mock 工厂提升执行，托管数组须经 vi.hoisted 声明
const createdHosts = vi.hoisted(() => [] as FakeHost[]);

vi.mock('../../../../packages/plugin-sdk/src/bridge/host', () => ({
  createBridgeHost: vi.fn(() => {
    const host: FakeHost = {
      ready: new Promise(() => {}),
      pushEvent: vi.fn(),
      pushAction: vi.fn(),
      ping: vi.fn(() => Promise.resolve()),
      destroy: vi.fn()
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
  createPluginBridgeDispatcher: vi.fn(async () => async () => null)
}));

vi.mock('../../services/deep-link', () => ({
  openPluginDeepLink: vi.fn()
}));

const SPACE = { type: 'personal', id: 'personal' } as const;

/** 等组件异步 init 走完（nextTick + 宏任务 + nextTick） */
async function flush(): Promise<void> {
  await nextTick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await nextTick();
}

function mountCard(): { el: HTMLElement; app: App } {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp({
    render: () =>
      h(AppMessageCard, {
        pluginId: 'spark-example',
        viewId: 'post-card',
        messageId: 'm-1',
        space: { ...SPACE }
      })
  });
  app.component('el-icon', ElIcon);
  app.mount(el);
  return { el, app };
}

beforeEach(() => {
  localStorage.clear();
  createdHosts.length = 0;
  vi.clearAllMocks();
});

afterEach(() => {
  document.body.innerHTML = '';
});

describe('AppMessageCard', () => {
  it('握手 ctx 注入壳层环境信息（appVersion/platform/shellVersion，A57）', async () => {
    // 卡片视图须在 manifest.views 声明为 message-card，否则 init 在握手前降级
    vi.mocked(fetchPluginManifest).mockResolvedValueOnce({
      sdkVersion: '1',
      views: [{ id: 'post-card', type: 'message-card' }]
    } as never);
    const { app } = mountCard();
    await flush();
    expect(createdHosts).toHaveLength(1);
    // 下发给 createBridgeHost 的 ctx 必须携带壳层生成的环境字段（插件只读），
    // 且 shellVersion 与握手 sdkVersion 同值（契约版本单源）
    const options = vi.mocked(createBridgeHost).mock.calls[0][0];
    expect(options.ctx.appVersion).toBe(pkg.version);
    expect(options.ctx.shellVersion).toBe('1');
    expect(options.ctx.shellVersion).toBe(options.sdkVersion);
    expect(['windows', 'macos', 'linux', 'android', 'ios', 'unknown']).toContain(options.ctx.platform);
    expect(options.ctx.mount).toMatchObject({ viewType: 'message-card' });
    app.unmount();
  });
});
