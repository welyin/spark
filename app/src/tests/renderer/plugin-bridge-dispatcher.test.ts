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

// market.pickSpkg / sys.saveFile 走壳层代开文件对话框（api.pickSpkgFile /
// saveFileWithDialog → tauri dialog）；jsdom 无对话框运行时，打桩以验证桥面路由
vi.mock('../../api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../api')>();
  return {
    ...actual,
    pickSpkgFile: vi.fn(async () => '/tmp/todo.spkg'),
    saveFileWithDialog: vi.fn(async () => ({ cancelled: false as const, path: '/tmp/out.txt' }))
  };
});

import { ElMessageBox } from 'element-plus';
import { saveFileWithDialog } from '../../api';
import { createPluginBridgeDispatcher, type PluginBridgeIdentity } from '../../plugin/bridge-dispatcher';
import { getAppMessages, getConversation } from '../../stores/messages';
import type { AppMessageDto } from '../../api/types';

/** 后端桩：任意层级任意方法返回 Promise<null>（test-setup 同款代理；
 * 这里按测试需要重装具体桩） */
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
  pluginId: 'spark-example',
  viewId: 'default',
  domain: 'plugin:spark-example',
  space: { type: 'org', id: 'org_1' },
  pluginName: '组织微博',
  supportedSpaces: ['org']
};

/** 市场安装状态授权清单（grantedPermissions 数据源） */
function mockGrantedPermissions(permissions: string[], pluginId = 'spark-example'): void {
  (window.electronAPI as any).pluginMarket = {
    list: async () => [{ id: pluginId, grantedPermissions: permissions }]
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  (window.electronAPI as any).plugin = makeNullApi();
  // contacts 只读门面走 electronAPI.contacts（backend.contacts 透传）
  (window.electronAPI as any).contacts = makeNullApi();
  // 社交投递走 electronAPI.feed（backend.feed 透传）
  (window.electronAPI as any).feed = makeNullApi();
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
    // A42 评审决议：新面 org.listMine 与老面口径统一——personal 空间同样拒绝
    await expect(handler('org', 'listMine', [])).rejects.toThrow(/Access denied/);
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
    expect(dto.pluginId).toBe('spark-example');
    expect(dto.status).toBe('local');
    const conv = getConversation('org:org_1', 'app:spark-example');
    expect(conv?.kind).toBe('app');
    expect(conv?.unreadCount).toBe(1);
    expect(getConversation('org:org_1', 'app:evil')).toBeUndefined();
    expect(getConversation('personal', 'app:spark-example')).toBeUndefined();
    expect(getAppMessages('org:org_1', 'app:spark-example')).toHaveLength(1);
  });

  it('personal 空间注入：space key 为 personal', async () => {
    // 独立插件身份，与其他用例无顺序耦合
    mockGrantedPermissions(['message:app'], 'demo-personal');
    const handler = await createPluginBridgeDispatcher({
      ...BASE_IDENTITY,
      pluginId: 'demo-personal',
      domain: 'plugin:demo-personal',
      space: { type: 'personal', id: 'personal' },
      supportedSpaces: ['personal', 'org']
    });
    await handler('messages', 'sendAppMessage', [{ summary: '个人空间通知' }]);
    expect(getConversation('personal', 'app:demo-personal')?.kind).toBe('app');
    expect(getConversation('personal', 'app:demo-personal')?.unreadCount).toBe(1);
    expect(getConversation('org:org_1', 'app:demo-personal')).toBeUndefined();
  });

  it('message:app 授权通过：listAppMessages/markRead 走绑定身份的应用会话', async () => {
    // 独立插件身份，与其他用例无顺序耦合
    mockGrantedPermissions(['message:app'], 'demo-reader');
    const handler = await createPluginBridgeDispatcher({
      ...BASE_IDENTITY,
      pluginId: 'demo-reader',
      domain: 'plugin:demo-reader'
    });
    await handler('messages', 'sendAppMessage', [{ summary: '一条' }]);
    const list = (await handler('messages', 'listAppMessages', [])) as AppMessageDto[];
    expect(list).toHaveLength(1);
    expect(list[0].pluginId).toBe('demo-reader');
    expect(list[0].summary).toBe('一条');
    await expect(handler('messages', 'markRead', [])).resolves.toEqual({ success: true });
    expect(getConversation('org:org_1', 'app:demo-reader')?.unreadCount).toBe(0);
    expect(getAppMessages('org:org_1', 'app:demo-reader').every((msg) => msg.read)).toBe(true);
  });

  it('message-card 视图：有 message:app 授权也拒绝应用会话读写（仅卡片回调经 action 上行）', async () => {
    mockGrantedPermissions(['message:app']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('messages', 'sendAppMessage', [{ summary: 'x' }])).rejects.toThrow(/Access denied/);
    await expect(handler('messages', 'listAppMessages', [])).rejects.toThrow(/Access denied/);
    await expect(handler('messages', 'markRead', [])).rejects.toThrow(/Access denied/);
  });
});

describe('contacts 域（社交投递层 §9.4 contact:read 只读门面）', () => {
  it('contact:read 未授权：listFriends/listGroups/listTags 一律拒绝', async () => {
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('contacts', 'listFriends', [])).rejects.toThrow(/Access denied/);
    await expect(handler('contacts', 'listGroups', [])).rejects.toThrow(/Access denied/);
    await expect(handler('contacts', 'listTags', [])).rejects.toThrow(/Access denied/);
  });

  it('contact:read 授权：三个只读方法放行，落到内核命令', async () => {
    // makeNullApi 对任意方法返回 Promise<null>——授权后调用 resolve 为 null（放行即可）
    mockGrantedPermissions(['contact:read']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('contacts', 'listFriends', [])).resolves.toBeNull();
    await expect(handler('contacts', 'listGroups', [])).resolves.toBeNull();
    await expect(handler('contacts', 'listTags', [])).resolves.toBeNull();
  });

  it('message-card 视图：有 contact:read 授权也拒绝（卡片视图不暴露通讯录）', async () => {
    mockGrantedPermissions(['contact:read']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('contacts', 'listFriends', [])).rejects.toThrow(/Access denied/);
    await expect(handler('contacts', 'listGroups', [])).rejects.toThrow(/Access denied/);
    await expect(handler('contacts', 'listTags', [])).rejects.toThrow(/Access denied/);
  });

  it('未知 contacts 方法拒绝（未知调用）', async () => {
    mockGrantedPermissions(['contact:read']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('contacts', 'addFriend', [])).rejects.toThrow(/Access denied/);
  });
});

describe('feed 域（社交投递层 §9 + A18 §4.1 权限归一：deliver 需 feed:write，pull/订阅收件需 feed:read）', () => {
  it('feed:write 未授权：feed.deliver 拒绝（旧 feed:deliver 位不再门控）', async () => {
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(
      handler('feed', 'deliver', [{ topic: 'spark-example:posts', payload: {}, recipients: [] }])
    ).rejects.toThrow(/Access denied/);
    // 仅有旧位 feed:deliver 也拒绝（canonical 已迁 feed:write）
    mockGrantedPermissions(['feed:deliver']);
    const legacy = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(
      legacy('feed', 'deliver', [{ topic: 'spark-example:posts', payload: {}, recipients: [] }])
    ).rejects.toThrow(/Access denied/);
  });

  it('feed:write 授权：deliver 放行，落到 electronAPI.feed', async () => {
    mockGrantedPermissions(['feed:write']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(
      handler('feed', 'deliver', [{ topic: 'spark-example:posts', payload: { t: 1 }, recipients: ['bob'] }])
    ).resolves.toBeNull();
  });

  it('出站 topic 前缀校验：非本插件前缀拒绝（架构 §8）', async () => {
    mockGrantedPermissions(['feed:write']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    // 前缀 != 调用方插件 id（spark-example）→ InvalidTopic
    await expect(
      handler('feed', 'deliver', [{ topic: 'evil:posts', payload: {}, recipients: [] }])
    ).rejects.toThrow(/InvalidTopic.*does not match plugin/);
    // 前缀 == 插件 id → 放行
    await expect(
      handler('feed', 'deliver', [{ topic: 'spark-example:posts', payload: {}, recipients: [] }])
    ).resolves.toBeNull();
  });

  it('feed.pull 需 feed:read（A18 §4.1）：未授权拒绝，授权放行', async () => {
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(
      handler('feed', 'pull', [{ topic: 'spark-example:posts', cursor: undefined, limit: 20 }])
    ).rejects.toThrow(/Access denied/);
    mockGrantedPermissions(['feed:read']);
    const granted = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(
      granted('feed', 'pull', [{ topic: 'spark-example:posts', cursor: undefined, limit: 20 }])
    ).resolves.toBeNull();
  });

  it('B2：pull 跨插件 topic 归属校验——读他人收件箱被拒', async () => {
    mockGrantedPermissions(['feed:read']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    // 前缀 != 本插件 id（spark-example）→ InvalidTopic（防 `pull({topic:"spark-moments:posts"})` 读他人收件箱）
    await expect(
      handler('feed', 'pull', [{ topic: 'spark-moments:posts' }])
    ).rejects.toThrow(/InvalidTopic.*does not match plugin/);
    // 前缀 == 本插件 id → 放行
    await expect(
      handler('feed', 'pull', [{ topic: 'spark-example:posts' }])
    ).resolves.toBeNull();
  });

  it('message-card 视图无 feed 域（有 feed:write/feed:read 也拒绝）', async () => {
    mockGrantedPermissions(['feed:write', 'feed:read']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(
      handler('feed', 'deliver', [{ topic: 'spark-example:posts', payload: {}, recipients: [] }])
    ).rejects.toThrow(/Access denied/);
    await expect(
      handler('feed', 'pull', [{ topic: 'spark-example:posts' }])
    ).rejects.toThrow(/Access denied/);
  });

  it('未知 feed 方法拒绝（未知调用）', async () => {
    mockGrantedPermissions(['feed:write']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('feed', 'onReceive', ['x'])).rejects.toThrow(/Access denied/);
  });
});


describe('affairs 域（community-affairs §7.2 sdk.affairs：读须 affairs:read，写须 affairs:write）', () => {
  it('未授权：读/写方法一律拒绝', async () => {
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('affairs', 'listFollowed', [])).rejects.toThrow(/Access denied/);
    await expect(handler('affairs', 'readLog', ['af_x'])).rejects.toThrow(/Access denied/);
    await expect(handler('affairs', 'readResolution', ['af_x'])).rejects.toThrow(/Access denied/);
    await expect(handler('affairs', 'ladderStatus', ['af_x'])).rejects.toThrow(/Access denied/);
    await expect(handler('affairs', 'follow', [{ kind: 'affair-genesis' }])).rejects.toThrow(/Access denied/);
    await expect(handler('affairs', 'unfollow', ['af_x'])).rejects.toThrow(/Access denied/);
    await expect(handler('affairs', 'submitOp', [{ affairId: 'af_x', opType: 'vote' }])).rejects.toThrow(
      /Access denied/
    );
  });

  it('affairs:read 授权：只读方法放行，写方法仍拒绝', async () => {
    mockGrantedPermissions(['affairs:read']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('affairs', 'listFollowed', [])).resolves.toBeNull();
    await expect(handler('affairs', 'readLog', ['af_x'])).resolves.toBeNull();
    await expect(handler('affairs', 'readResolution', ['af_x'])).resolves.toBeNull();
    await expect(handler('affairs', 'ladderStatus', ['af_x'])).resolves.toBeNull();
    await expect(handler('affairs', 'follow', [{ kind: 'affair-genesis' }])).rejects.toThrow(/Access denied/);
    await expect(handler('affairs', 'submitOp', [{ affairId: 'af_x', opType: 'vote' }])).rejects.toThrow(
      /Access denied/
    );
  });

  it('affairs:write 授权：写方法放行（读方法按 affairs:read 仍拒绝）', async () => {
    mockGrantedPermissions(['affairs:write']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('affairs', 'follow', [{ kind: 'affair-genesis' }])).resolves.toBeNull();
    await expect(handler('affairs', 'unfollow', ['af_x'])).resolves.toBeNull();
    await expect(handler('affairs', 'submitOp', [{ affairId: 'af_x', opType: 'vote' }])).resolves.toBeNull();
    await expect(handler('affairs', 'readLog', ['af_x'])).rejects.toThrow(/Access denied/);
  });

  it('message-card 视图仅 affairs 只读（readLog/readResolution 需 affairs:read；写面有授权也拒绝）', async () => {
    mockGrantedPermissions(['affairs:read', 'affairs:write']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('affairs', 'readLog', ['af_x'])).resolves.toBeNull();
    await expect(handler('affairs', 'readResolution', ['af_x'])).resolves.toBeNull();
    await expect(handler('affairs', 'readRules', ['af_x'])).rejects.toThrow(/Access denied/);
    await expect(handler('affairs', 'follow', [{ kind: 'affair-genesis' }])).rejects.toThrow(/Access denied/);
    await expect(handler('affairs', 'submitOp', [{ affairId: 'af_x', opType: 'vote' }])).rejects.toThrow(/Access denied/);
  });

  it('message-card 视图 affairs 只读仍受 affairs:read 授权门控', async () => {
    mockGrantedPermissions(['affairs:write']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('affairs', 'readLog', ['af_x'])).rejects.toThrow(/Access denied/);
  });

  it('未知 affairs 方法拒绝（未知调用）', async () => {
    mockGrantedPermissions(['affairs:read', 'affairs:write']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('affairs', 'deleteAffair', ['af_x'])).rejects.toThrow(/Access denied/);
  });
});

describe('credentials 域（community-affairs §7.2 sdk.credentials：credentials:read）', () => {
  it('credentials:read 未授权：三个方法一律拒绝', async () => {
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('credentials', 'listHeld', [])).rejects.toThrow(/Access denied/);
    await expect(
      handler('credentials', 'presentHolderProof', [{ credId: 'c1', requestId: 'r1', orgId: 'org_1', collection: 'docs' }])
    ).rejects.toThrow(/Access denied/);
    await expect(handler('credentials', 'queryVerifiers', ['org_1'])).rejects.toThrow(/Access denied/);
  });

  it('credentials:read 授权：放行并落到 electronAPI.credentials', async () => {
    mockGrantedPermissions(['credentials:read']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('credentials', 'listHeld', [])).resolves.toBeNull();
    await expect(
      handler('credentials', 'presentHolderProof', [{ credId: 'c1', requestId: 'r1', orgId: 'org_1', collection: 'docs' }])
    ).resolves.toBeNull();
    await expect(handler('credentials', 'queryVerifiers', ['org_1'])).resolves.toBeNull();
  });

  it('message-card 视图无 credentials 域（有授权也拒绝）', async () => {
    mockGrantedPermissions(['credentials:read']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('credentials', 'listHeld', [])).rejects.toThrow(/Access denied/);
  });

  it('未知 credentials 方法拒绝（签发接口不存在于 SDK）', async () => {
    mockGrantedPermissions(['credentials:read']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('credentials', 'issue', [{}])).rejects.toThrow(/Access denied/);
  });
});

describe('policy 域（community-affairs §7.2 sdk.policy：读须 policy:read，写须 policy:write）', () => {
  it('未授权：read/submitDraft 一律拒绝', async () => {
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('policy', 'read', ['org_1'])).rejects.toThrow(/Access denied/);
    await expect(handler('policy', 'submitDraft', [{ policyV: 1 }])).rejects.toThrow(/Access denied/);
  });

  it('policy:read 授权：read 放行，submitDraft 仍拒绝', async () => {
    mockGrantedPermissions(['policy:read']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('policy', 'read', ['org_1'])).resolves.toBeNull();
    await expect(handler('policy', 'submitDraft', [{ policyV: 1 }])).rejects.toThrow(/Access denied/);
  });

  it('policy:write 授权：submitDraft 放行（read 按 policy:read 仍拒绝）', async () => {
    mockGrantedPermissions(['policy:write']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('policy', 'submitDraft', [{ policyV: 1 }])).resolves.toBeNull();
    await expect(handler('policy', 'read', ['org_1'])).rejects.toThrow(/Access denied/);
  });

  it('message-card 视图无 policy 域（有授权也拒绝）', async () => {
    mockGrantedPermissions(['policy:read', 'policy:write']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('policy', 'read', ['org_1'])).rejects.toThrow(/Access denied/);
    await expect(handler('policy', 'submitDraft', [{ policyV: 1 }])).rejects.toThrow(/Access denied/);
  });
});

// ------------------------------------------------------------------
// A34 市场模块（sdk.market）：plugin-market-* 命令等语义移植。
// 读位 market:read（list/checkUpdates/resolveRepo/inspectLocal/
// announceList/announceGet/pickSpkg），写位 market:write（upgrade/
// setEnabled/uninstall/installFromRepo/importLocal/announcePublish）
// ------------------------------------------------------------------

/** 记录型市场命令桩（vi.fn 记录调用参数；list 兼任 grantedPermissions 数据源） */
function mockMarketApi(granted: string[]) {
  const api = {
    list: vi.fn(async () => [{ id: 'spark-example', grantedPermissions: granted }]),
    checkUpdates: vi.fn(async () => []),
    upgrade: vi.fn(async () => null),
    setEnabled: vi.fn(async () => null),
    uninstall: vi.fn(async () => undefined),
    resolveRepo: vi.fn(async () => null),
    installFromRepo: vi.fn(async () => null),
    inspectLocal: vi.fn(async () => null),
    importLocal: vi.fn(async () => null),
    announcePublish: vi.fn(async () => null),
    announceList: vi.fn(async () => []),
    announceGet: vi.fn(async () => null)
  };
  (window.electronAPI as any).pluginMarket = api;
  return api;
}

describe('market 域（A34 sdk.market：读须 market:read，写须 market:write）', () => {
  it('未授权：读面与写面一律拒绝', async () => {
    mockMarketApi([]);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('market', 'list', [])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'checkUpdates', [])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'resolveRepo', ['github.com/acme/todo'])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'inspectLocal', ['/tmp/todo.spkg'])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'announceList', [])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'announceGet', ['github.com/acme/todo'])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'pickSpkg', [])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'upgrade', ['spark-chat'])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'setEnabled', ['spark-chat', false])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'uninstall', ['spark-chat'])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'installFromRepo', ['github.com/acme/todo'])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'importLocal', ['/tmp/todo.spkg', 'ab12'])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'announcePublish', [{ id: 'github.com/acme/todo' }])).rejects.toThrow(/Access denied/);
  });

  it('market:read 授权：读面放行（含 pickSpkg 壳层代开对话框），写面仍拒绝', async () => {
    const api = mockMarketApi(['market:read']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    // 构造时已调一次 list 读授权清单；清零后只观察桥面调用
    api.list.mockClear();

    await handler('market', 'list', []);
    expect(api.list).toHaveBeenCalledTimes(1);

    await handler('market', 'checkUpdates', []);
    expect(api.checkUpdates).toHaveBeenCalledWith(undefined);
    await handler('market', 'checkUpdates', ['spark-chat']);
    expect(api.checkUpdates).toHaveBeenCalledWith('spark-chat');

    await handler('market', 'resolveRepo', ['github.com/acme/todo']);
    expect(api.resolveRepo).toHaveBeenCalledWith('github.com/acme/todo');
    await handler('market', 'inspectLocal', ['/tmp/todo.spkg']);
    expect(api.inspectLocal).toHaveBeenCalledWith('/tmp/todo.spkg');
    await handler('market', 'announceList', []);
    expect(api.announceList).toHaveBeenCalledTimes(1);
    await handler('market', 'announceGet', ['github.com/acme/todo']);
    expect(api.announceGet).toHaveBeenCalledWith('github.com/acme/todo');

    // pickSpkg 不进 electronAPI.pluginMarket：壳层代开系统对话框（打桩路径）
    await expect(handler('market', 'pickSpkg', [])).resolves.toBe('/tmp/todo.spkg');

    await expect(handler('market', 'upgrade', ['spark-chat'])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'uninstall', ['spark-chat'])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'installFromRepo', ['github.com/acme/todo'])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'importLocal', ['/tmp/todo.spkg', 'ab12'])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'announcePublish', [{ id: 'github.com/acme/todo' }])).rejects.toThrow(/Access denied/);
    expect(api.upgrade).not.toHaveBeenCalled();
    expect(api.uninstall).not.toHaveBeenCalled();
  });

  it('market:write 授权：写面放行（参数透传），读面仍拒绝', async () => {
    const api = mockMarketApi(['market:write']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    api.list.mockClear();

    await handler('market', 'upgrade', ['spark-chat']);
    expect(api.upgrade).toHaveBeenCalledWith('spark-chat');
    await handler('market', 'setEnabled', ['spark-chat', false]);
    expect(api.setEnabled).toHaveBeenCalledWith('spark-chat', false);
    await handler('market', 'uninstall', ['spark-chat']);
    expect(api.uninstall).toHaveBeenCalledWith('spark-chat');
    await handler('market', 'installFromRepo', ['github.com/acme/todo']);
    expect(api.installFromRepo).toHaveBeenCalledWith('github.com/acme/todo');

    // importLocal：confirmOverwrite 可选位透传
    await handler('market', 'importLocal', ['/tmp/todo.spkg', 'ab12']);
    expect(api.importLocal).toHaveBeenCalledWith('/tmp/todo.spkg', 'ab12', undefined);
    await handler('market', 'importLocal', ['/tmp/todo.spkg', 'ab12', true]);
    expect(api.importLocal).toHaveBeenCalledWith('/tmp/todo.spkg', 'ab12', true);

    const announce = { id: 'github.com/acme/todo', name: '待办', version: '0.1.0' };
    await handler('market', 'announcePublish', [announce]);
    expect(api.announcePublish).toHaveBeenCalledWith(announce);

    await expect(handler('market', 'announceList', [])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'pickSpkg', [])).rejects.toThrow(/Access denied/);
    expect(api.announceList).not.toHaveBeenCalled();
  });

  it('message-card 视图无 market 域（有授权也整域拒绝）', async () => {
    mockMarketApi(['market:read', 'market:write']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('market', 'list', [])).rejects.toThrow(/Access denied/);
    await expect(handler('market', 'uninstall', ['spark-chat'])).rejects.toThrow(/Access denied/);
  });
});

// ------------------------------------------------------------------
// A18 插件数据 API 面（communication §4.1）：sdk.messages / sdk.contacts
// 等语义移植现有 Tauri 命令（同输入同结果 + space 桥绑定注入）+ 权限拒绝
// ------------------------------------------------------------------

/** 记录型 messages 命令桩（vi.fn 记录调用参数） */
function mockMessagesApi() {
  const api = {
    listConversations: vi.fn(async () => []),
    listMessages: vi.fn(async () => []),
    sendText: vi.fn(async (_space: string, _conv: string, id: string, text: string) => ({
      id,
      senderId: 'me',
      senderName: '我',
      type: 'text',
      content: text,
      createdAt: 1000,
      status: 'sent',
      recalled: false
    })),
    recall: vi.fn(async () => ({ success: true })),
    markRead: vi.fn(async () => ({ success: true })),
    ensureDirect: vi.fn(async () => ({ id: 'direct-1' })),
    resend: vi.fn(async (_s: string, _c: string, id: string) => ({ id })),
    deleteMessage: vi.fn(async () => ({ success: true })),
    setDraft: vi.fn(async () => ({ success: true })),
    togglePin: vi.fn(async () => ({ success: true })),
    toggleMute: vi.fn(async () => ({ success: true })),
    clear: vi.fn(async () => ({ success: true })),
    deleteConversation: vi.fn(async () => ({ success: true }))
  };
  (window.electronAPI as any).messages = api;
  return api;
}

/** 记录型 contacts 命令桩 */
function mockContactsApi() {
  const api = {
    overview: vi.fn(async () => ({ friends: [], requests: [], outgoing: [], tags: [], groups: [] })),
    updateProfile: vi.fn(async () => ({ success: true })),
    setBlocked: vi.fn(async () => ({ success: true })),
    removeFriend: vi.fn(async () => ({ success: true })),
    sendRequest: vi.fn(async () => ({ id: 'r1' })),
    replyRequest: vi.fn(async () => ({ id: 'r1' })),
    askRequest: vi.fn(async () => ({ id: 'r1' })),
    resolveRequest: vi.fn(async () => ({ success: true })),
    tagCreate: vi.fn(async (_s: string, id: string, name: string) => ({ id, name })),
    tagRename: vi.fn(async () => ({ success: true })),
    tagDelete: vi.fn(async () => ({ success: true })),
    groupCreate: vi.fn(async (_s: string, id: string, name: string) => ({ id, name })),
    groupRename: vi.fn(async () => ({ success: true })),
    groupDelete: vi.fn(async () => ({ success: true })),
    groupMove: vi.fn(async () => ({ success: true })),
    setGroup: vi.fn(async () => ({ success: true })),
    orgGroupCreate: vi.fn(async () => null),
    orgGroupRename: vi.fn(async () => ({ success: true })),
    orgGroupDelete: vi.fn(async () => ({ success: true })),
    orgGroupMove: vi.fn(async () => ({ success: true }))
  };
  (window.electronAPI as any).contacts = api;
  return api;
}

describe('A18 sdk.messages IM 数据面（§4.1：等语义移植 + space 桥注入 + messages:read/write）', () => {
  it('messages:read 未授权：conversations/list 拒绝；messages:write 未授权：send/recall/markConversationRead 拒绝', async () => {
    mockMessagesApi();
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('messages', 'conversations', [])).rejects.toThrow(/Access denied/);
    await expect(handler('messages', 'list', ['c1'])).rejects.toThrow(/Access denied/);
    mockGrantedPermissions(['messages:read']);
    const reader = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(reader('messages', 'send', ['c1', 'hi'])).rejects.toThrow(/Access denied/);
    await expect(reader('messages', 'recall', ['c1', 'm1'])).rejects.toThrow(/Access denied/);
    await expect(reader('messages', 'markConversationRead', ['c1'])).rejects.toThrow(/Access denied/);
  });

  it('等语义对照：同输入落到同一 Tauri 命令（space 由桥绑定注入）', async () => {
    const api = mockMessagesApi();
    mockGrantedPermissions(['messages:read', 'messages:write']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await handler('messages', 'conversations', []);
    expect(api.listConversations).toHaveBeenCalledWith('org:org_1');
    await handler('messages', 'list', ['c1']);
    expect(api.listMessages).toHaveBeenCalledWith('org:org_1', 'c1');
    const quote = { messageId: 'm0', senderName: 'A', preview: 'p' };
    const sent = (await handler('messages', 'send', ['c1', 'hello', quote])) as { id: string; content: string };
    expect(api.sendText).toHaveBeenCalledWith('org:org_1', 'c1', expect.stringMatching(/^m\d+-\d+$/), 'hello', quote);
    expect(sent.content).toBe('hello');
    await handler('messages', 'recall', ['c1', 'm1']);
    expect(api.recall).toHaveBeenCalledWith('org:org_1', 'c1', 'm1');
    await handler('messages', 'markConversationRead', ['c1']);
    expect(api.markRead).toHaveBeenCalledWith('org:org_1', 'c1');
  });

  it('personal 空间注入：space key 为 personal', async () => {
    const api = mockMessagesApi();
    mockGrantedPermissions(['messages:read'], 'demo-space');
    const handler = await createPluginBridgeDispatcher({
      ...BASE_IDENTITY,
      pluginId: 'demo-space',
      domain: 'plugin:demo-space',
      space: { type: 'personal', id: 'personal' },
      supportedSpaces: ['personal', 'org']
    });
    await handler('messages', 'conversations', []);
    expect(api.listConversations).toHaveBeenCalledWith('personal');
  });

  it('message-card 视图：有 messages 授权也拒绝（卡片视图不暴露 IM 面）', async () => {
    mockMessagesApi();
    mockGrantedPermissions(['messages:read', 'messages:write']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('messages', 'conversations', [])).rejects.toThrow(/Access denied/);
    await expect(handler('messages', 'send', ['c1', 'hi'])).rejects.toThrow(/Access denied/);
  });

  it('A19 写面补全：八命令等语义映射（space 桥注入），未授权拒绝', async () => {
    const api = mockMessagesApi();
    // 未授权：八命令一律拒绝
    const denied = await createPluginBridgeDispatcher(BASE_IDENTITY);
    for (const [method, args] of [
      ['ensureDirect', ['peer-1', '甲']],
      ['resend', ['c1', 'm1']],
      ['deleteMessage', ['c1', 'm1']],
      ['setDraft', ['c1', 'draft']],
      ['togglePin', ['c1']],
      ['toggleMute', ['c1']],
      ['clear', ['c1']],
      ['deleteConversation', ['c1']]
    ] as const) {
      await expect(denied('messages', method, [...args])).rejects.toThrow(/Access denied/);
    }
    // 授权：等语义对照（space 桥绑定注入为第一参数）
    mockGrantedPermissions(['messages:write']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await handler('messages', 'ensureDirect', ['peer-1', '甲']);
    expect(api.ensureDirect).toHaveBeenCalledWith('org:org_1', 'peer-1', '甲');
    await handler('messages', 'resend', ['c1', 'm1']);
    expect(api.resend).toHaveBeenCalledWith('org:org_1', 'c1', 'm1');
    await handler('messages', 'deleteMessage', ['c1', 'm1']);
    expect(api.deleteMessage).toHaveBeenCalledWith('org:org_1', 'c1', 'm1');
    await handler('messages', 'setDraft', ['c1', 'draft']);
    expect(api.setDraft).toHaveBeenCalledWith('org:org_1', 'c1', 'draft');
    await handler('messages', 'togglePin', ['c1']);
    expect(api.togglePin).toHaveBeenCalledWith('org:org_1', 'c1');
    await handler('messages', 'toggleMute', ['c1']);
    expect(api.toggleMute).toHaveBeenCalledWith('org:org_1', 'c1');
    await handler('messages', 'clear', ['c1']);
    expect(api.clear).toHaveBeenCalledWith('org:org_1', 'c1');
    await handler('messages', 'deleteConversation', ['c1']);
    expect(api.deleteConversation).toHaveBeenCalledWith('org:org_1', 'c1');
  });
});

describe('A18 sdk.contacts 数据面（§4.1：等语义移植 + space 桥注入 + contacts:read/write）', () => {
  it('contacts:read 未授权：overview 拒绝；contacts:write 未授权：写操作一律拒绝', async () => {
    mockContactsApi();
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('contacts', 'overview', [])).rejects.toThrow(/Access denied/);
    mockGrantedPermissions(['contacts:read']);
    const reader = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(reader('contacts', 'setBlocked', ['r1', true])).rejects.toThrow(/Access denied/);
    await expect(reader('contacts', 'tagCreate', ['t1', '同事'])).rejects.toThrow(/Access denied/);
    await expect(reader('contacts', 'resolveRequest', ['r1', true, 'open'])).rejects.toThrow(/Access denied/);
    await expect(reader('contacts', 'groupMove', ['g1', 0])).rejects.toThrow(/Access denied/);
    await expect(reader('contacts', 'orgGroupCreate', ['', 'g1', '分组'])).rejects.toThrow(/Access denied/);
  });

  it('等语义对照：同输入落到同一 Tauri 命令（space 由桥绑定注入）', async () => {
    const api = mockContactsApi();
    mockGrantedPermissions(['contacts:read', 'contacts:write']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await handler('contacts', 'overview', []);
    expect(api.overview).toHaveBeenCalledWith('org:org_1');
    await handler('contacts', 'updateProfile', ['r1', { remark: 'x' }]);
    expect(api.updateProfile).toHaveBeenCalledWith('org:org_1', 'r1', { remark: 'x' });
    await handler('contacts', 'setBlocked', ['r1', true]);
    expect(api.setBlocked).toHaveBeenCalledWith('org:org_1', 'r1', true);
    await handler('contacts', 'removeFriend', ['r1', true]);
    expect(api.removeFriend).toHaveBeenCalledWith('r1', true);
    const input = { id: 'q1', rootId: 'r2', raw: '{}', source: 'card', message: 'hi' };
    await handler('contacts', 'sendRequest', [input]);
    expect(api.sendRequest).toHaveBeenCalledWith(input);
    await handler('contacts', 'replyRequest', ['r1', '你好']);
    expect(api.replyRequest).toHaveBeenCalledWith('r1', '你好');
    await handler('contacts', 'askRequest', ['r1', '哪位']);
    expect(api.askRequest).toHaveBeenCalledWith('r1', '哪位');
    await handler('contacts', 'resolveRequest', ['r1', true, 'open']);
    expect(api.resolveRequest).toHaveBeenCalledWith('r1', true, 'open');
    await handler('contacts', 'tagCreate', ['t1', '同事']);
    expect(api.tagCreate).toHaveBeenCalledWith('org:org_1', 't1', '同事');
    await handler('contacts', 'tagRename', ['t1', '伙伴']);
    expect(api.tagRename).toHaveBeenCalledWith('org:org_1', 't1', '伙伴');
    await handler('contacts', 'tagDelete', ['t1']);
    expect(api.tagDelete).toHaveBeenCalledWith('org:org_1', 't1');
    await handler('contacts', 'groupCreate', ['g1', '家人']);
    expect(api.groupCreate).toHaveBeenCalledWith('org:org_1', 'g1', '家人');
    await handler('contacts', 'groupRename', ['g1', '亲友']);
    expect(api.groupRename).toHaveBeenCalledWith('org:org_1', 'g1', '亲友');
    await handler('contacts', 'groupDelete', ['g1']);
    expect(api.groupDelete).toHaveBeenCalledWith('org:org_1', 'g1');
    await handler('contacts', 'groupMove', ['g1', 2]);
    expect(api.groupMove).toHaveBeenCalledWith('org:org_1', 'g1', 2);
    await handler('contacts', 'setGroup', ['r1', 'g1']);
    expect(api.setGroup).toHaveBeenCalledWith('org:org_1', 'r1', 'g1');
    await handler('contacts', 'orgGroupCreate', ['', 'og1', '总部']);
    expect(api.orgGroupCreate).toHaveBeenCalledWith('org:org_1', '', 'og1', '总部');
    await handler('contacts', 'orgGroupRename', ['og1', '分部']);
    expect(api.orgGroupRename).toHaveBeenCalledWith('org:org_1', 'og1', '分部');
    await handler('contacts', 'orgGroupDelete', ['og1']);
    expect(api.orgGroupDelete).toHaveBeenCalledWith('org:org_1', 'og1');
    await handler('contacts', 'orgGroupMove', ['og1', 1, 'og0']);
    expect(api.orgGroupMove).toHaveBeenCalledWith('org:org_1', 'og1', 1, 'og0');
  });

  it('message-card 视图：有 contacts 授权也拒绝数据面', async () => {
    mockContactsApi();
    mockGrantedPermissions(['contacts:read', 'contacts:write']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('contacts', 'overview', [])).rejects.toThrow(/Access denied/);
    await expect(handler('contacts', 'setBlocked', ['r1', true])).rejects.toThrow(/Access denied/);
  });
});

describe('navigation 域（sdk.navigation：免权限纯 UI 导航意图 + 参数白名单）', () => {
  const ROOT_ID = 'ab'.repeat(32);

  it('空授权下合法 openChat 放行并派发壳内 spark:open-chat 事件', async () => {
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    const seen: unknown[] = [];
    const listener = (event: Event) => seen.push((event as CustomEvent).detail);
    window.addEventListener('spark:open-chat', listener);
    try {
      await expect(handler('navigation', 'openChat', [{ rootId: ROOT_ID, name: '张三' }])).resolves.toBeUndefined();
      expect(seen).toEqual([{ rootId: ROOT_ID, name: '张三' }]);
    } finally {
      window.removeEventListener('spark:open-chat', listener);
    }
  });

  it('openChat 参数白名单：rootId 非 64hex / name 超长一律拒绝（不产出跳转）', async () => {
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('navigation', 'openChat', [{ rootId: 'not-hex' }])).rejects.toThrow(/InvalidArgs/);
    await expect(handler('navigation', 'openChat', [{ rootId: ROOT_ID.toUpperCase() }])).rejects.toThrow(/InvalidArgs/);
    await expect(handler('navigation', 'openChat', [{ rootId: ROOT_ID, name: 'x'.repeat(101) }])).rejects.toThrow(/InvalidArgs/);
    await expect(handler('navigation', 'openChat', [{}])).rejects.toThrow(/InvalidArgs/);
  });

  it('openPlugin 只接受市场注册表内的插件 id，合法调用派发统一深链事件', async () => {
    mockGrantedPermissions([], 'spark-example');
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    const seen: unknown[] = [];
    const listener = (event: Event) => seen.push((event as CustomEvent).detail);
    window.addEventListener('spark:open-plugin', listener);
    try {
      await expect(
        handler('navigation', 'openPlugin', [{ pluginId: 'spark-example', cardData: { affairId: 'af_1' } }])
      ).resolves.toBeUndefined();
      expect(seen).toEqual([{ pluginId: 'spark-example', cardData: { affairId: 'af_1' } }]);
      await expect(handler('navigation', 'openPlugin', [{ pluginId: 'evil-plugin' }])).rejects.toThrow(/Access denied/);
    } finally {
      window.removeEventListener('spark:open-plugin', listener);
    }
  });

  it('message-card 视图裁剪不放行 navigation（卡片不得驱动壳层跳转）', async () => {
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('navigation', 'openChat', [{ rootId: ROOT_ID }])).rejects.toThrow(/Access denied/);
    await expect(handler('navigation', 'openPlugin', [{ pluginId: 'spark-example' }])).rejects.toThrow(/Access denied/);
  });
});

// ------------------------------------------------------------------
// A42 组织管理模块（sdk.org）：org-* 命令等语义移植。
// 读位 org:read（基础位，granted 清单恒含；空授权测试仍按表拒绝），
// 写位 org:write（高级）；orgId 作用域调用的空间门控
// （ORG_ID_SCOPED_CALLS + makeNodeCard 带实参）
// ------------------------------------------------------------------

/** 记录型组织命令桩（organization/p2p/dataManagement 三路） */
function mockOrgApi(granted: string[]) {
  mockGrantedPermissions(granted);
  const api = {
    organization: {
      listMine: vi.fn(async () => []),
      create: vi.fn(async (input: unknown) => input),
      leave: vi.fn(async () => null),
      addMember: vi.fn(async () => null),
      removeMember: vi.fn(async () => null),
      getGatewayActiveSet: vi.fn(async () => []),
      createInvite: vi.fn(async () => ({ invite: 'code', orgId: 'org_1', orgName: '组织' })),
      acceptInvite: vi.fn(async () => ({ orgId: 'org_1', orgName: '组织', memberCount: 2 })),
      getSyncOverview: vi.fn(async () => null),
      setPublic: vi.fn(async () => null),
      updateInfo: vi.fn(async () => null),
      updateMyIdentity: vi.fn(async () => null),
      resolveAddress: vi.fn(async () => null),
      searchKnown: vi.fn(async () => []),
      sendInvite: vi.fn(async (input: unknown) => input),
      respondInvite: vi.fn(async (input: unknown) => input),
      inviteRecords: vi.fn(async () => [])
    },
    p2p: {
      makeNodeCard: vi.fn(async () => ({ card: 'node-card' })),
      importNodeCard: vi.fn(async () => ({ peerId: 'peer', hasRecoveryToken: false, connectError: null }))
    },
    dataManagement: {
      purgePreview: vi.fn(async () => null),
      purgeExecute: vi.fn(async () => null),
      exportData: vi.fn(async () => ({ cancelled: true }))
    }
  };
  (window.electronAPI as any).organization = api.organization;
  (window.electronAPI as any).p2p = api.p2p;
  (window.electronAPI as any).dataManagement = api.dataManagement;
  return api;
}

describe('org 域（A42 sdk.org：读须 org:read，写须 org:write + orgId 空间门控）', () => {
  it('未授权：读面与写面一律拒绝', async () => {
    mockOrgApi([]);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('org', 'listMine', [])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'getSyncOverview', ['org_1'])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'resolveAddress', ['addr'])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'exportData', [])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'create', [{ name: 'x' }])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'addMember', ['org_1', { rootId: 'r' }])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'purgeExecute', ['org_1', 1, true])).rejects.toThrow(/Access denied/);
  });

  it('org:read 授权：读面放行（参数透传到 electronAPI），写面仍拒绝', async () => {
    const api = mockOrgApi(['org:read']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);

    await handler('org', 'listMine', []);
    expect(api.organization.listMine).toHaveBeenCalledTimes(1);
    await handler('org', 'getSyncOverview', ['org_1']);
    expect(api.organization.getSyncOverview).toHaveBeenCalledWith('org_1');
    await handler('org', 'getGatewayActiveSet', ['org_1']);
    expect(api.organization.getGatewayActiveSet).toHaveBeenCalledWith('org_1');
    await handler('org', 'inviteRecords', ['org_1']);
    expect(api.organization.inviteRecords).toHaveBeenCalledWith('org_1');
    await handler('org', 'resolveAddress', ['addr']);
    expect(api.organization.resolveAddress).toHaveBeenCalledWith('addr');
    await handler('org', 'searchKnown', ['关键词']);
    expect(api.organization.searchKnown).toHaveBeenCalledWith('关键词');
    // 节点名片读面：无实参放行（空间无关）
    await handler('org', 'makeNodeCard', []);
    expect(api.p2p.makeNodeCard).toHaveBeenCalledWith(undefined);
    await handler('org', 'purgePreview', ['org_1', 100]);
    expect(api.dataManagement.purgePreview).toHaveBeenCalledWith('org_1', 100);
    await handler('org', 'exportData', []);
    expect(api.dataManagement.exportData).toHaveBeenCalledTimes(1);

    await expect(handler('org', 'create', [{ name: 'x' }])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'leave', ['org_1'])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'importNodeCard', ['card'])).rejects.toThrow(/Access denied/);
    expect(api.organization.create).not.toHaveBeenCalled();
    expect(api.p2p.importNodeCard).not.toHaveBeenCalled();
  });

  it('org:write 授权：写面放行（含 sendInvite/respondInvite 对象实参），读面仍拒绝', async () => {
    const api = mockOrgApi(['org:write']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);

    await handler('org', 'create', [{ name: '新组织', domainType: 'leaf' }]);
    expect(api.organization.create).toHaveBeenCalledWith({ name: '新组织', domainType: 'leaf' });
    await handler('org', 'leave', ['org_1']);
    expect(api.organization.leave).toHaveBeenCalledWith('org_1');
    await handler('org', 'addMember', ['org_1', { rootId: 'r'.repeat(64) }]);
    expect(api.organization.addMember).toHaveBeenCalledWith('org_1', { rootId: 'r'.repeat(64) });
    await handler('org', 'removeMember', ['org_1', 'm1']);
    expect(api.organization.removeMember).toHaveBeenCalledWith('org_1', 'm1');
    await handler('org', 'createInvite', ['org_1']);
    expect(api.organization.createInvite).toHaveBeenCalledWith('org_1');
    await handler('org', 'acceptInvite', ['code-x']);
    expect(api.organization.acceptInvite).toHaveBeenCalledWith('code-x');
    await handler('org', 'setPublic', ['org_1', true, '展示名']);
    expect(api.organization.setPublic).toHaveBeenCalledWith('org_1', true, '展示名');
    await handler('org', 'updateInfo', ['org_1', { name: '改名' }]);
    expect(api.organization.updateInfo).toHaveBeenCalledWith('org_1', { name: '改名' });
    await handler('org', 'updateMyIdentity', ['org_1', { nickname: '组织内昵称' }]);
    expect(api.organization.updateMyIdentity).toHaveBeenCalledWith('org_1', { nickname: '组织内昵称' });
    const invite = { orgId: 'org_1', targetRootId: 't'.repeat(64), targetPeerId: null, targetAddresses: null };
    await handler('org', 'sendInvite', [invite]);
    expect(api.organization.sendInvite).toHaveBeenCalledWith(invite);
    await handler('org', 'respondInvite', [{ inviteId: 'i1', accept: true }]);
    expect(api.organization.respondInvite).toHaveBeenCalledWith({ inviteId: 'i1', accept: true });
    await handler('org', 'importNodeCard', ['card-x']);
    expect(api.p2p.importNodeCard).toHaveBeenCalledWith('card-x');
    await handler('org', 'purgeExecute', ['org_1', 100, true]);
    expect(api.dataManagement.purgeExecute).toHaveBeenCalledWith('org_1', 100, true);

    await expect(handler('org', 'listMine', [])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'getSyncOverview', ['org_1'])).rejects.toThrow(/Access denied/);
  });

  it('orgId 作用域：org 空间实参须等于当前空间，越域拒绝（含 sendInvite 的 input.orgId）', async () => {
    const api = mockOrgApi(['org:read', 'org:write']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('org', 'getSyncOverview', ['org_2'])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'leave', ['org_2'])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'sendInvite', [{ orgId: 'org_2', targetRootId: 't' }])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'makeNodeCard', ['org_2'])).rejects.toThrow(/Access denied/);
    // 命中当前空间放行
    await handler('org', 'getSyncOverview', ['org_1']);
    expect(api.organization.getSyncOverview).toHaveBeenCalledWith('org_1');
    await handler('org', 'makeNodeCard', ['org_1']);
    expect(api.p2p.makeNodeCard).toHaveBeenCalledWith('org_1');
  });

  it('personal 空间：orgId 作用域调用与 listMine 一律拒绝，空间无关面放行', async () => {
    const api = mockOrgApi(['org:read', 'org:write']);
    const handler = await createPluginBridgeDispatcher({
      ...BASE_IDENTITY,
      space: { type: 'personal', id: 'personal' },
      supportedSpaces: ['personal', 'org']
    });
    // orgId 作用域（含带实参的 makeNodeCard）拒绝
    await expect(handler('org', 'getSyncOverview', ['org_1'])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'leave', ['org_1'])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'sendInvite', [{ orgId: 'org_1', targetRootId: 't' }])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'makeNodeCard', ['org_1'])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'purgeExecute', ['org_1', 1, true])).rejects.toThrow(/Access denied/);
    // listMine 与老面 runtime.listMineOrganizations 口径统一：personal 拒绝
    // （A42 评审决议：personal 空间无组织可管，不向插件枚举本机组织名册）
    await expect(handler('org', 'listMine', [])).rejects.toThrow(/Access denied/);
    expect(api.organization.listMine).not.toHaveBeenCalled();
    // 空间无关面（创建/加入/发现/导出/无参名片）放行
    await handler('org', 'create', [{ name: 'x' }]);
    await handler('org', 'acceptInvite', ['code']);
    await handler('org', 'searchKnown', ['k']);
    await handler('org', 'makeNodeCard', []);
    await handler('org', 'exportData', []);
    expect(api.organization.create).toHaveBeenCalledTimes(1);
    expect(api.p2p.makeNodeCard).toHaveBeenCalledWith(undefined);
  });

  it('message-card 视图无 org 域（有授权也整域拒绝）', async () => {
    mockOrgApi(['org:read', 'org:write']);
    const handler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(handler('org', 'listMine', [])).rejects.toThrow(/Access denied/);
    await expect(handler('org', 'leave', ['org_1'])).rejects.toThrow(/Access denied/);
  });
});

// ------------------------------------------------------------------
// sys.saveFile（A42 修复：spark-files 下载壳层代存通路）：storage:read
// 权限 + 文件名/体积白名单（assertSaveFileInput）+ 转发壳层保存对话框
// ------------------------------------------------------------------

describe('sys.saveFile（壳层代存：storage:read + 入参白名单）', () => {
  it('未授权拒绝；授权后转发 saveFileWithDialog 并返回壳层结果', async () => {
    mockGrantedPermissions([]);
    let handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    await expect(handler('sys', 'saveFile', [{ name: 'a.txt', dataBase64: 'QUJD' }])).rejects.toThrow(/Access denied/);

    mockGrantedPermissions(['storage:read']);
    handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    const result = await handler('sys', 'saveFile', [{ name: 'a.txt', dataBase64: 'QUJD' }]);
    expect(saveFileWithDialog).toHaveBeenCalledWith('a.txt', 'QUJD');
    expect(result).toEqual({ cancelled: false, path: '/tmp/out.txt' });
  });

  it('文件名白名单：路径分隔/控制字符/纯点名/超长一律 InvalidArgs（不触达壳层）', async () => {
    mockGrantedPermissions(['storage:read']);
    const handler = await createPluginBridgeDispatcher(BASE_IDENTITY);
    const bad = ['a/b.txt', 'a\\b.txt', 'a\u0000b', '.', '..', 'x'.repeat(256)];
    for (const name of bad) {
      await expect(handler('sys', 'saveFile', [{ name, dataBase64: 'QUJD' }])).rejects.toThrow(/InvalidArgs/);
    }
    await expect(handler('sys', 'saveFile', [{ name: 'a.txt' }])).rejects.toThrow(/InvalidArgs/);
    await expect(handler('sys', 'saveFile', [{ name: 'a.txt', dataBase64: 42 }])).rejects.toThrow(/InvalidArgs/);
    expect(saveFileWithDialog).not.toHaveBeenCalled();
  });

  it('message-card 视图不放行 sys.saveFile；personal 空间放行（空间无关）', async () => {
    mockGrantedPermissions(['storage:read']);
    const cardHandler = await createPluginBridgeDispatcher({ ...BASE_IDENTITY, viewType: 'message-card' });
    await expect(cardHandler('sys', 'saveFile', [{ name: 'a.txt', dataBase64: 'QUJD' }])).rejects.toThrow(/Access denied/);

    const personalHandler = await createPluginBridgeDispatcher({
      ...BASE_IDENTITY,
      space: { type: 'personal', id: 'personal' },
      supportedSpaces: ['personal', 'org']
    });
    await expect(personalHandler('sys', 'saveFile', [{ name: 'a.txt', dataBase64: 'QUJD' }])).resolves.toEqual({
      cancelled: false,
      path: '/tmp/out.txt'
    });
  });
});
