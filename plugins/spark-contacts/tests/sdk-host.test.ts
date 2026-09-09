/**
 * spark-contacts sdk-host 适配层单测（communication §4.2 功能对等迁移的数据面）：
 * - HostContactsApi（与壳层 ElectronAPI['contacts'] 同签名）逐方法等语义
 *   映射到 sdk.contacts（space 由桥绑定，spaceKey 实参忽略）；
 * - 事件面：onChanged → ContactsSynced + OrgSynced 双发（两个分支幂等重拉）；
 *   onRequestChanged/onFriendProfileUpdated 透传为壳层 P2pEventDto 同形；
 * - 未绑定 SDK 时退化为种子数据模式（isTauri=false，contactsApi undefined）。
 */
import { describe, expect, it } from 'vitest';
import type { PluginContactsAPI, PluginContext, PluginSDK } from '../../../packages/plugin-sdk/src';
import {
  bindPluginRuntime,
  boundSpaceKey,
  contactsApi,
  isTauri,
  listenP2pEvents,
  organizationApi
} from '../src/sdk-host';

function fakeCtx(space: PluginContext['space'] = { type: 'personal', id: 'personal' }): PluginContext {
  return {
    pluginId: 'spark-contacts',
    viewId: 'default',
    domain: 'plugin:spark-contacts',
    space,
    theme: 'light',
    mount: { viewType: 'app' }
  };
}

type Calls = Array<{ method: string; args: unknown[] }>;

function fakeContacts(calls: Calls, overrides: Partial<PluginContactsAPI> = {}): PluginContactsAPI {
  const record =
    (method: string, result: unknown) =>
    (...args: unknown[]) => {
      calls.push({ method, args });
      return Promise.resolve(result);
    };
  return {
    listFriends: record('listFriends', []),
    listGroups: record('listGroups', []),
    listTags: record('listTags', []),
    overview: record('overview', { friends: [], requests: [], outgoing: [], tags: [], groups: [], groupTree: [], memberExtras: {} }),
    updateProfile: record('updateProfile', { success: true }),
    setBlocked: record('setBlocked', { success: true }),
    removeFriend: record('removeFriend', { success: true }),
    sendRequest: record('sendRequest', { id: 'r1' }),
    replyRequest: record('replyRequest', { id: 'r1' }),
    askRequest: record('askRequest', { id: 'r1' }),
    resolveRequest: record('resolveRequest', { success: true }),
    tagCreate: record('tagCreate', { id: 't1', name: '标签' }),
    tagRename: record('tagRename', { success: true }),
    tagDelete: record('tagDelete', { success: true }),
    groupCreate: record('groupCreate', { id: 'g1', name: '分组' }),
    groupRename: record('groupRename', { success: true }),
    groupDelete: record('groupDelete', { success: true }),
    groupMove: record('groupMove', { success: true }),
    setGroup: record('setGroup', { success: true }),
    orgGroupCreate: record('orgGroupCreate', { id: 'og1', name: '子组', children: [] }),
    orgGroupRename: record('orgGroupRename', { success: true }),
    orgGroupDelete: record('orgGroupDelete', { success: true }),
    orgGroupMove: record('orgGroupMove', { success: true }),
    onChanged: record('onChanged', undefined),
    onRequestChanged: record('onRequestChanged', undefined),
    onFriendProfileUpdated: record('onFriendProfileUpdated', undefined),
    ...overrides
  } as PluginContactsAPI;
}

function bind(contacts: PluginContactsAPI | undefined, space?: PluginContext['space']): void {
  bindPluginRuntime({ contacts } as unknown as PluginSDK, fakeCtx(space));
}

describe('sdk-host 适配层（HostContactsApi → sdk.contacts 等语义映射）', () => {
  it('绑定后 isTauri 为真、spaceKey 取桥下发值；organizationApi v1 恒 undefined（缺口在案）', () => {
    bind(fakeContacts([]), { type: 'org', id: 'org-1' });
    expect(isTauri()).toBe(true);
    expect(boundSpaceKey()).toBe('org:org-1');
    expect(organizationApi()).toBeUndefined();
  });

  it('读写面逐方法直通 sdk.contacts（spaceKey 实参忽略）', async () => {
    const calls: Calls = [];
    bind(fakeContacts(calls));
    const api = contactsApi();
    expect(api).toBeDefined();
    await api!.overview('whatever');
    await api!.updateProfile('personal', 'root-1', { remark: '备注' });
    await api!.setBlocked('personal', 'root-1', true);
    await api!.removeFriend('root-1', true);
    await api!.sendRequest({ id: 'r1', rootId: 'root-2', raw: 'raw', source: 'search', message: '加一下' });
    await api!.replyRequest('r1', '你是谁');
    await api!.askRequest('r1', '我是');
    await api!.resolveRequest('r1', true, 'open');
    await api!.tagCreate('personal', 't1', '标签');
    await api!.tagRename('personal', 't1', '新名');
    await api!.tagDelete('personal', 't1');
    await api!.groupCreate('personal', 'g1', '分组');
    await api!.groupRename('personal', 'g1', '新组名');
    await api!.groupDelete('personal', 'g1');
    await api!.groupMove('personal', 'g1', 0);
    await api!.setGroup('personal', 'root-1', 'g1');
    await api!.orgGroupCreate('org:o1', '', 'og1', '子组');
    await api!.orgGroupRename('org:o1', 'og1', '新子组');
    await api!.orgGroupDelete('org:o1', 'og1');
    await api!.orgGroupMove('org:o1', 'og1', 0, 'og2');
    expect(calls.map((c) => c.method)).toEqual([
      'overview',
      'updateProfile',
      'setBlocked',
      'removeFriend',
      'sendRequest',
      'replyRequest',
      'askRequest',
      'resolveRequest',
      'tagCreate',
      'tagRename',
      'tagDelete',
      'groupCreate',
      'groupRename',
      'groupDelete',
      'groupMove',
      'setGroup',
      'orgGroupCreate',
      'orgGroupRename',
      'orgGroupDelete',
      'orgGroupMove'
    ]);
    // 关键参数透传校验（spaceKey 实参全部丢弃，业务参数原样）
    expect(calls[1].args).toEqual(['root-1', { remark: '备注' }]);
    expect(calls[7].args).toEqual(['r1', true, 'open']);
    expect(calls[19].args).toEqual(['og1', 0, 'og2']);
  });

  it('事件面：onChanged → ContactsSynced + OrgSynced 双发；申请/资料事件透传同形', async () => {
    const calls: Calls = [];
    const handlers: Record<string, (e: never) => void> = {};
    bind(
      fakeContacts(calls, {
        onChanged: ((h: () => void) => {
          handlers.changed = h as never;
          return Promise.resolve();
        }) as never,
        onRequestChanged: ((h: (e: unknown) => void) => {
          handlers.request = h as never;
          return Promise.resolve();
        }) as never,
        onFriendProfileUpdated: ((h: (e: unknown) => void) => {
          handlers.profile = h as never;
          return Promise.resolve();
        }) as never
      })
    );
    const received: Array<{ kind: string; data: unknown }> = [];
    await listenP2pEvents((event) => received.push(event));
    handlers.changed();
    handlers.request({ kind: 'FriendRequestReceived', request: { id: 'r1' } } as never);
    handlers.profile({ rootId: 'root-1', nickname: '新昵称' } as never);
    expect(received.map((e) => e.kind)).toEqual([
      'ContactsSynced',
      'OrgSynced',
      'FriendRequestReceived',
      'FriendProfileUpdated'
    ]);
    expect(received[2].data).toEqual({ request: { id: 'r1' } });
    expect(received[3].data).toEqual({ rootId: 'root-1', nickname: '新昵称' });
  });

  it('未绑定 SDK：isTauri=false（store 退化种子数据），contactsApi undefined，订阅 no-op', async () => {
    bindPluginRuntime(null as unknown as PluginSDK, fakeCtx());
    expect(isTauri()).toBe(false);
    expect(contactsApi()).toBeUndefined();
    await expect(listenP2pEvents(() => {})).resolves.toBeUndefined();
  });
});
