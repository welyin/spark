/**
 * PluginIframeHost 组件级测试（桥/源/dispatcher 均 mock，watchdog 与
 * plugin/disabled 用真实模块——纯逻辑、localStorage 持久化，jsdom 直测）：
 * - init 竞态：快速连续 reload 只留一个活 host（旧代全部销毁）；
 * - 已停用实例首帧不渲染 iframe；
 * - 重新启用清零停用标记与熔断计数后重新加载。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h, nextTick, type App } from 'vue';
import { ElButton, ElIcon } from 'element-plus';
import PluginIframeHost from '../../components/plugin/PluginIframeHost.vue';
import {
  disablePluginInstance,
  isPluginInstanceDisabled,
  pluginInstanceKey
} from '../../plugin/disabled';
import { getWatchdogCounters } from '../../plugin/watchdog';

type FakeHost = {
  ready: Promise<unknown>;
  pushEvent: ReturnType<typeof vi.fn>;
  ping: ReturnType<typeof vi.fn>;
  destroy: ReturnType<typeof vi.fn>;
  resolveReady: () => void;
};

// vi.mock 工厂提升执行，托管数组须经 vi.hoisted 声明
const createdHosts = vi.hoisted(() => [] as FakeHost[]);

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
      // 与真实实现一致：destroy 拒绝未 settle 的 ready（过期代 init 借此解除 await）
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
  createPluginBridgeDispatcher: vi.fn(async () => async () => null)
}));

const SPACE = { type: 'personal', id: 'personal' } as const;
const INSTANCE_KEY = pluginInstanceKey('spark-example', SPACE);

/** 等组件异步 init 走完（nextTick + 宏任务 + nextTick） */
async function flush(): Promise<void> {
  await nextTick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await nextTick();
}

function mountHost(): { el: HTMLElement; app: App } {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp({
    render: () =>
      h(PluginIframeHost, {
        pluginId: 'spark-example',
        viewId: 'default',
        space: { ...SPACE }
      })
  });
  app.component('el-button', ElButton);
  app.component('el-icon', ElIcon);
  app.mount(el);
  return { el, app };
}

/** 取 PluginIframeHost 子组件实例（根 render 直挂），用于触发 reload */
function childInstance(app: App): any {
  return (app as any)._instance.subTree.component;
}

beforeEach(() => {
  localStorage.clear();
  createdHosts.length = 0;
  vi.clearAllMocks();
});

afterEach(() => {
  document.body.innerHTML = '';
});

describe('PluginIframeHost', () => {
  it('init 竞态：快速连续 reload 只留一个活 host（旧代全部销毁）', async () => {
    const { el, app } = mountHost();
    await flush();
    expect(createdHosts).toHaveLength(1);

    // 首次握手未完成时连续两次 reload：第二代过期即弃，只第三代建 host
    const instance = childInstance(app);
    instance.setupState.reload();
    instance.setupState.reload();
    await flush();

    expect(createdHosts).toHaveLength(2);
    expect(createdHosts[0].destroy).toHaveBeenCalledTimes(1);
    expect(createdHosts[1].destroy).not.toHaveBeenCalled();

    // 活 host 完成握手后进入 ready（覆盖层收起）
    createdHosts[1].resolveReady();
    await flush();
    expect(el.querySelector('.plugin-iframe-overlay')).toBeNull();
    expect(el.querySelector('iframe')).not.toBeNull();

    app.unmount();
    expect(createdHosts[1].destroy).toHaveBeenCalledTimes(1);
  });

  it('已停用实例：首帧即覆盖层，不渲染 iframe（插件代码不加载）', async () => {
    disablePluginInstance(INSTANCE_KEY, 'ready-errors');
    const { el, app } = mountHost();
    await flush();

    expect(el.querySelector('iframe')).toBeNull();
    expect(el.textContent).toContain('已自动停用');
    expect(el.textContent).toContain('启动阶段连续异常');
    expect(createdHosts).toHaveLength(0);

    app.unmount();
  });

  it('重新启用：清零停用标记与熔断计数后重新加载', async () => {
    disablePluginInstance(INSTANCE_KEY, 'unresponsive');
    // 造一点实例级计数，验证 reenable 一并清零
    getWatchdogCounters(INSTANCE_KEY).readyErrorTimestamps.push(Date.now());
    getWatchdogCounters(INSTANCE_KEY).unresponsiveTrips = 2;

    const { el, app } = mountHost();
    await flush();
    expect(el.querySelector('iframe')).toBeNull();

    const reenableButton = [...el.querySelectorAll('button')].find((button) =>
      button.textContent?.includes('重新启用')
    );
    expect(reenableButton).toBeDefined();
    reenableButton!.click();
    await flush();

    expect(isPluginInstanceDisabled(INSTANCE_KEY)).toBe(false);
    expect(getWatchdogCounters(INSTANCE_KEY).readyErrorTimestamps).toHaveLength(0);
    expect(getWatchdogCounters(INSTANCE_KEY).unresponsiveTrips).toBe(0);
    // 重新进入加载流程：iframe 已重建、活 host 已创建
    expect(el.querySelector('iframe')).not.toBeNull();
    expect(createdHosts).toHaveLength(1);

    app.unmount();
  });
});
