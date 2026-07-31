/**
 * 桥 dispatcher 权限中间件测试（设计文档「权限模型」运行时强制）：
 * - 三重过滤：grantedPermissions ∩ view type 裁剪 ∩ 当前 space；
 * - identity:sign 使用时询问（ElMessageBox，按 插件 ID+域名 会话级记忆，并发首调复用同一确认）；
 * - 未授权一律 Access denied。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('element-plus', async (importOriginal) => {
  const actual = await importOriginal<typeof import('element-plus')>();
  return { ...actual, ElMessageBox: { confirm: vi.fn() } };
});

import { ElMessageBox } from 'element-plus';
import { createPluginBridgeDispatcher, type PluginBridgeIdentity } from '../../plugin-bridge-dispatcher';
import { getAppMessages, getConversation } from '../../mock/messages';
import type { AppMessageDto } from '../../api/types';

/** 后端桩：任意层级任意方法返回 Promise<null>（test-setup 同款代理；
 * test-setup 把 electronAPI.plugin 覆盖成 listCatalog 桩，这里按测试需要重装） */
const makeNullApi = (): any =>
  new Proxy(function () {}, {
    get(target, key) {
      return key in target ? (target as any)[key] : makeNullApi();
    },
    apply() {
      return Promise.resolve(null);
    }
  });

const BASE_IDENTITY: PluginBridgeIdentity = {
  pluginId: 'weibo-core',
  viewId: 'default',
  domain: 'plugin:weibo-core',
  space: { type: 'org', id: 'org_1' },
  pluginName: '组织微博',
  supportedSpaces: ['org']
};

/** 市场安装状态授权清单（grantedPermissions 数据源） */
function mockGrantedPermissions(permissions: string[], pluginId = 'weibo-core'): void {
  (window.electronAPI as any).pluginMarket = {
    list: async () => [{ id: pluginId, grantedPermissions: permissions }]
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  (window.electronAPI as any).plugin = makeNullApi();
  mockGrantedPermissions([]);
});

describe('三重过滤：grantedPermissions', () => {
  it('免权限基础调用在空授权下放行', async () => {
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('identity', 'verify', ['p', 's', 'k'])).resolves.toBeNull();
    await expect(handler('evidence', 'verify', [])).resolves.toBeNull();
    await expect(handler('runtime', 'currentRoot', [])).resolves.toBeNull();
  });

  it('未授权调用抛 Access denied；授权后放行', async () => {
    let handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('docs', 'get', ['c', 'id'])).rejects.toThrow(/Access denied/);
    await expect(handler('docs', 'put', ['c', 'id', {}])).rejects.toThrow(/Access denied/);

    mockGrantedPermissions(['storage:read', 'storage:write']);
    handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('docs', 'get', ['c', 'id'])).resolves.toBeNull();
    await expect(handler('docs', 'put', ['c', 'id', {}])).resolves.toBeNull();
  });

  it('市场状态读取失败按空清单（最小授权）', async () => {
    (window.electronAPI as any).pluginMarket = { list: async () => Promise.reject(new Error('ipc down')) };
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('docs', 'get', ['c', 'id'])).rejects.toThrow(/Access denied/);
    await expect(handler('identity', 'verify', ['p', 's', 'k'])).resolves.toBeNull();
  });

  it('未知调用抛 Access denied', async () => {
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('docs', 'dropTable', [])).rejects.toThrow(/Access denied/);
    await expect(handler('shell', 'exec', [])).rejects.toThrow(/Access denied/);
  });
});

describe('三重过滤：view type 裁剪', () => {
  it('message-card 仅 docs 只读与验签类（有 storage:write 也拒绝写）', async () => {
    mockGrantedPermissions(['storage:read', 'storage:write']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('docs', 'get', ['c', 'id'])).resolves.toBeNull();
    await expect(handler('identity', 'verify', ['p', 's', 'k'])).resolves.toBeNull();
    await expect(handler('docs', 'put', ['c', 'id', {}])).rejects.toThrow(/Access denied/);
    await expect(handler('runtime', 'currentRoot', [])).rejects.toThrow(/Access denied/);
  });

  it('app 主视图全量（仅 grantedPermissions 过滤）', async () => {
    mockGrantedPermissions(['storage:read', 'storage:write']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'app' });
    await expect(handler('runtime', 'currentRoot', [])).resolves.toBeNull();
    await expect(handler('docs', 'put', ['c', 'id', {}])).resolves.toBeNull();
  });
});

describe('三重过滤：当前 space', () => {
  it('manifest 不支持的 space 类型整域拒绝', async () => {
    mockGrantedPermissions(['storage:read']);
    const handler = await createPluginBridgeDispatcher({
      ...BASE_IDENTITY,
      space: { type: 'personal', id: 'personal' }
    });
    await expect(handler('docs', 'get', ['c', 'id'])).rejects.toThrow(/Access denied/);
  });

  it('syncOrganizationData 的 org 实参必须与当前 space 一致', async () => {
    mockGrantedPermissions(['org:sync']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('runtime', 'syncOrganizationData', ['org_2'])).rejects.toThrow(/Access denied/);
    await expect(handler('runtime', 'syncOrganizationData', ['org_1'])).resolves.toBeNull();
  });

  it('personal 空间下 org 域调用一律拒绝（无 org 实参可校验）', async () => {
    mockGrantedPermissions(['org:sync', 'org:read']);
    const handler = await createPluginBridgeDispatcher({
      ...BASE_IDENTITY,
      space: { type: 'personal', id: 'personal' },
      supportedSpaces: ['personal', 'org']
    });
    await expect(handler('runtime', 'syncOrganizationData', ['org_1'])).rejects.toThrow(/Access denied/);
    await expect(handler('runtime', 'syncOrganizationData', ['personal'])).rejects.toThrow(/Access denied/);
    await expect(handler('runtime', 'listMineOrganizations', [])).rejects.toThrow(/Access denied/);
  });
});

describe('使用时询问：identity:sign', () => {
  it('首次调用弹确认，会话内按 插件名+域名 记忆决定', async () => {
    mockGrantedPermissions(['identity:sign']);
    (ElMessageBox.confirm as ReturnType<typeof vi.fn>).mockResolvedValue({});
    // 独立域名避免与其他用例共享会话级记忆
    const identity = { ...BASE_IDENTITY, domain: 'plugin:sign-consent-test', pluginName: '签名测试' };
    const handler = await createPluginBridgeDispatcher(identity);

    await expect(handler('identity', 'sign', ['payload'])).resolves.toBeNull();
    expect(ElMessageBox.confirm).toHaveBeenCalledTimes(1);

    await expect(handler('identity', 'sign', ['payload-2'])).resolves.toBeNull();
    expect(ElMessageBox.confirm).toHaveBeenCalledTimes(1); // 会话内不再询问
  });

  it('用户拒绝签名抛 Access denied', async () => {
    mockGrantedPermissions(['identity:sign']);
    (ElMessageBox.confirm as ReturnType<typeof vi.fn>).mockRejectedValue('cancel');
    const identity = { ...BASE_IDENTITY, domain: 'plugin:sign-reject-test', pluginName: '签名拒绝' };
    const handler = await createPluginBridgeDispatcher(identity);
    await expect(handler('identity', 'sign', ['payload'])).rejects.toThrow(/Access denied/);
  });

  it('并发首调复用同一确认（in-flight），只弹一次框', async () => {
    mockGrantedPermissions(['identity:sign'], 'sign-inflight-test');
    // 确认 Promise 手动控制，保证两次调用都落在确认进行中
    let resolveConfirm: (value: unknown) => void = () => {};
    (ElMessageBox.confirm as ReturnType<typeof vi.fn>).mockImplementation(
      () => new Promise((resolve) => { resolveConfirm = resolve; })
    );
    const identity = {
      ...BASE_IDENTITY,
      pluginId: 'sign-inflight-test',
      domain: 'plugin:sign-inflight-test',
      pluginName: '并发签名'
    };
    const handler = await createPluginBridgeDispatcher(identity);

    const first = handler('identity', 'sign', ['payload-1']);
    const second = handler('identity', 'sign', ['payload-2']);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(ElMessageBox.confirm).toHaveBeenCalledTimes(1);

    resolveConfirm({});
    await expect(first).resolves.toBeNull();
    await expect(second).resolves.toBeNull();
    expect(ElMessageBox.confirm).toHaveBeenCalledTimes(1);
  });
});

describe('messages 域（应用会话 §20，pluginId/space 由桥注入）', () => {
  it('message:app 未授权：sendAppMessage/listAppMessages/markRead 一律拒绝', async () => {
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('messages', 'sendAppMessage', [{ summary: 'x' }])).rejects.toThrow(/Access denied/);
    await expect(handler('messages', 'listAppMessages', [])).rejects.toThrow(/Access denied/);
    await expect(handler('messages', 'markRead', [])).rejects.toThrow(/Access denied/);
  });

  it('pluginId/space 注入：写入落在绑定身份的应用会话，插件自报一律忽略', async () => {
    mockGrantedPermissions(['message:app']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    // payload 夹带 pluginId/spaceKey 不影响归属（桥按绑定身份注入，§20.4 归属不变量）
    const dto = (await handler('messages', 'sendAppMessage', [
      { summary: '新微博', pluginId: 'evil', spaceKey: 'personal' }
    ])) as AppMessageDto;
    expect(dto.pluginId).toBe('weibo-core');
    expect(dto.status).toBe('local');
    const conv = getConversation('org:org_1', 'app:weibo-core');
    expect(conv?.kind).toBe('app');
    expect(conv?.unreadCount).toBe(1);
    expect(getConversation('org:org_1', 'app:evil')).toBeUndefined();
    expect(getConversation('personal', 'app:weibo-core')).toBeUndefined();
    expect(getAppMessages('org:org_1', 'app:weibo-core')).toHaveLength(1);
  });

  it('personal 空间注入：space key 为 personal', async () => {
    mockGrantedPermissions(['message:app']);
    const handler = await createPluginBridgeDispatcher({
      ...BASE_IDENTITY,
      space: { type: 'personal', id: 'personal' },
      supportedSpaces: ['personal', 'org']
    });
    await handler('messages', 'sendAppMessage', [{ summary: '个人空间通知' }]);
    expect(getConversation('personal', 'app:weibo-core')?.kind).toBe('app');
    expect(getConversation('org:org_1', 'app:weibo-core')?.unreadCount).toBe(1); // 上一用例的会话不受影响
  });

  it('message-card 视图：有 message:app 授权也拒绝应用会话读写（仅卡片回调经 action 上行）', async () => {
    mockGrantedPermissions(['message:app']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('messages', 'sendAppMessage', [{ summary: 'x' }])).rejects.toThrow(/Access denied/);
    await expect(handler('messages', 'listAppMessages', [])).rejects.toThrow(/Access denied/);
    await expect(handler('messages', 'markRead', [])).rejects.toThrow(/Access denied/);
  });
});
