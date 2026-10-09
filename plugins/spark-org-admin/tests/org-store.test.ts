/**
 * spark-org-admin org-store 纯逻辑测试（A42）：
 * 副本健康度展示口径、名册排序与展示名、名片解析、数据治理判定、
 * 策略文档校验、通用展示助手——与壳层 OrgSettingsPanel/PurgeDataPanel
 * 内联逻辑逐函数同语义（迁移对照见任务报告）。
 */
import { describe, expect, it, vi } from 'vitest';
import type { PluginOrgAPI, PluginOrgMember, PluginOrgPurgePreview, PluginOrgSyncOverview } from '../../../packages/plugin-sdk/src';
import {
  formatBytes,
  hashGradient,
  loadSpaceOrganization,
  memberDisplayName,
  memberReplicasOk,
  networkDegraded,
  ORG_SECTIONS,
  orgSectionLabel,
  parseMemberCard,
  parsePolicyDoc,
  PERSONAL_SPACE_NOTICE,
  purgeBeforeTsOf,
  purgeExecutable,
  purgeReplicaSufficient,
  replicaLabel,
  replicaTagType,
  shortRootId,
  sortedMembers
} from '../src/org-store';

const overviewOf = (patch: Partial<PluginOrgSyncOverview>): PluginOrgSyncOverview => ({
  orgId: 'org_1',
  replicaTarget: 3,
  syncedPeers: 3,
  totalMembers: 4,
  members: [],
  connectedPeers: 2,
  recoveryState: 'idle',
  recoveryStartedAt: null,
  lastConnectedAt: null,
  dhtMode: 'server',
  status: 'good',
  kApplicable: true,
  memberReplicas: [],
  ...patch
});

const previewOf = (patch: Partial<PluginOrgPurgePreview> = {}): PluginOrgPurgePreview => ({
  domain: 'org:org_1',
  beforeTs: 1000,
  preview: { collections: ['docs'], affectedDocs: 5, affectedBytes: 2048 },
  replica: overviewOf({}),
  isCurrentUserAdmin: true,
  ...patch
});

const memberOf = (patch: Partial<PluginOrgMember>): PluginOrgMember => ({
  rootId: 'a'.repeat(64),
  role: 'member',
  joinedAt: 100,
  addedBy: 'b'.repeat(64),
  ...patch
});

describe('副本健康度（OrgSettingsPanel 同口径）', () => {
  it('replicaLabel / replicaTagType：达标 success，不足 warning，无数据空串/info', () => {
    expect(replicaLabel(null)).toBe('');
    expect(replicaTagType(null)).toBe('info');
    expect(replicaLabel(overviewOf({ syncedPeers: 3, replicaTarget: 3 }))).toBe('副本 3/3');
    expect(replicaTagType(overviewOf({ syncedPeers: 3, replicaTarget: 3 }))).toBe('success');
    expect(replicaTagType(overviewOf({ syncedPeers: 1, replicaTarget: 3 }))).toBe('warning');
  });

  it('memberReplicasOk：无记录视为达标；PC 副本 <3 不达标', () => {
    expect(memberReplicasOk(null)).toBe(true);
    expect(memberReplicasOk(overviewOf({ memberReplicas: [] }))).toBe(true);
    const replicas = [
      { rootId: 'a', pcSynced: true, deviceClass: 'pc' },
      { rootId: 'b', pcSynced: true, deviceClass: 'pc' },
      { rootId: 'c', pcSynced: false, deviceClass: 'mobile' }
    ];
    expect(memberReplicasOk(overviewOf({ memberReplicas: replicas }))).toBe(false);
    expect(
      memberReplicasOk(overviewOf({ memberReplicas: [...replicas, { rootId: 'd', pcSynced: true, deviceClass: 'pc' }] }))
    ).toBe(true);
  });

  it('networkDegraded：lost/localOnly 提示，其余不提示', () => {
    expect(networkDegraded(overviewOf({ status: 'lost' }))).toBe(true);
    expect(networkDegraded(overviewOf({ status: 'localOnly' }))).toBe(true);
    expect(networkDegraded(overviewOf({ status: 'good' }))).toBe(false);
    expect(networkDegraded(null)).toBe(false);
  });
});

describe('名册', () => {
  it('sortedMembers：管理员在前，同级按加入时间升序（不改入参）', () => {
    const members = [
      memberOf({ rootId: 'm1', role: 'member', joinedAt: 50 }),
      memberOf({ rootId: 'a1', role: 'admin', joinedAt: 300 }),
      memberOf({ rootId: 'm2', role: 'member', joinedAt: 10 }),
      memberOf({ rootId: 'a2', role: 'admin', joinedAt: 100 })
    ];
    const sorted = sortedMembers(members);
    expect(sorted.map((m) => m.rootId)).toEqual(['a2', 'a1', 'm2', 'm1']);
    expect(members[0].rootId).toBe('m1');
  });

  it('memberDisplayName：组织内昵称优先，缺省 rootId 短形', () => {
    expect(memberDisplayName(memberOf({ nickname: ' 小明 ' }))).toBe('小明');
    expect(memberDisplayName(memberOf({ rootId: 'abcdef0123456789' + '0'.repeat(48) }))).toBe('abcdef…0000');
  });

  it('shortRootId：≤12 原样，否则头 6 尾 4 省略', () => {
    expect(shortRootId('abc')).toBe('abc');
    expect(shortRootId('0123456789abcdef')).toBe('012345…cdef');
  });
});

describe('名片解析（parseMemberCard）', () => {
  const rootId = 'f'.repeat(64);

  it('JSON 名片：提取 rootId/peerId/addresses', () => {
    const card = parseMemberCard(JSON.stringify({ rootId, peerId: 'peer-x', addresses: ['/ip4/1.2.3.4/tcp/9'] }));
    expect(card).toEqual({ rootId, peerId: 'peer-x', addresses: ['/ip4/1.2.3.4/tcp/9'] });
  });

  it('纯文本：正则识别 64hex rootId，无寻址线索', () => {
    expect(parseMemberCard(`我的 rootId 是 ${rootId} 请拉我`)).toEqual({ rootId });
  });

  it('无法识别返回 null；空串返回 null', () => {
    expect(parseMemberCard('没有身份的文本')).toBeNull();
    expect(parseMemberCard('   ')).toBeNull();
    expect(parseMemberCard(JSON.stringify({ peerId: 'x' }))).toBeNull();
  });
});

describe('数据治理判定（PurgeDataPanel 同口径）', () => {
  it('purgeReplicaSufficient：syncedPeers ≥ replicaTarget 且副本信息在', () => {
    expect(purgeReplicaSufficient(previewOf())).toBe(true);
    expect(purgeReplicaSufficient(previewOf({ replica: overviewOf({ syncedPeers: 1 }) }))).toBe(false);
    expect(purgeReplicaSufficient(previewOf({ replica: null }))).toBe(false);
    expect(purgeReplicaSufficient(null)).toBe(false);
  });

  it('purgeExecutable：管理员 + 有影响 + 副本足 + 已确认导出 缺一不可', () => {
    expect(purgeExecutable(previewOf(), true)).toBe(true);
    expect(purgeExecutable(previewOf(), false)).toBe(false);
    expect(purgeExecutable(previewOf({ isCurrentUserAdmin: false }), true)).toBe(false);
    expect(purgeExecutable(previewOf({ preview: { collections: [], affectedDocs: 0, affectedBytes: 0 } }), true)).toBe(false);
    expect(purgeExecutable(previewOf({ replica: null }), true)).toBe(false);
    expect(purgeExecutable(null, true)).toBe(false);
  });

  it('purgeBeforeTsOf：选中日期的本地 00:00；空为 0', () => {
    expect(purgeBeforeTsOf(null)).toBe(0);
    const date = new Date(2026, 9, 1, 15, 30);
    const ts = purgeBeforeTsOf(date);
    expect(new Date(ts).getHours()).toBe(0);
    expect(new Date(ts).getDate()).toBe(1);
  });
});

describe('策略文档校验', () => {
  it('合法 JSON 对象放行；空/非对象/坏 JSON 报错', () => {
    expect(parsePolicyDoc('{"v":1}')).toEqual({ doc: { v: 1 } });
    expect('error' in parsePolicyDoc('')).toBe(true);
    expect('error' in parsePolicyDoc('[1,2]')).toBe(true);
    expect('error' in parsePolicyDoc('"str"')).toBe(true);
    expect('error' in parsePolicyDoc('{bad')).toBe(true);
  });
});

describe('通用展示与子项注册', () => {
  it('ORG_SECTIONS 含全部管理子项（与壳层 OrgSettingsPanel 对应 + 名册/策略/创建加入扩充）', () => {
    const keys = ORG_SECTIONS.map((section) => section.key);
    expect(keys).toEqual(['info', 'roster', 'policy', 'public', 'discover', 'recover', 'purge', 'membership']);
    expect(orgSectionLabel('roster')).toBe('成员名册');
    expect(orgSectionLabel(null)).toBe('');
  });

  it('formatBytes：分级进位', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(2048)).toBe('2.0 KB');
    expect(formatBytes(5 * 1024 * 1024)).toBe('5.0 MB');
  });

  it('hashGradient：同 seed 同色（稳定渐变串）', () => {
    expect(hashGradient('org_1')).toBe(hashGradient('org_1'));
    expect(hashGradient('org_1')).toMatch(/^linear-gradient/);
  });
});

describe('空间口径（A42 评审决议：listMine personal 拒绝）', () => {
  const orgViewOf = (orgId: string) => ({ orgId, name: orgId }) as never;

  it('personal 空间不调 listMine，返回 null（UI 如实提示切换空间）', async () => {
    const api = { listMine: vi.fn(async () => [orgViewOf('org_1')]) } as unknown as PluginOrgAPI;
    const found = await loadSpaceOrganization(api, { type: 'personal', id: 'personal' });
    expect(found).toBeNull();
    expect(api.listMine).not.toHaveBeenCalled();
  });

  it('org 空间经 listMine 定位本空间组织；未加入返回 null', async () => {
    const api = {
      listMine: vi.fn(async () => [orgViewOf('org_1'), orgViewOf('org_2')])
    } as unknown as PluginOrgAPI;
    const found = await loadSpaceOrganization(api, { type: 'org', id: 'org_2' });
    expect((found as { orgId: string } | null)?.orgId).toBe('org_2');
    expect(await loadSpaceOrganization(api, { type: 'org', id: 'org_x' })).toBeNull();
  });

  it('PERSONAL_SPACE_NOTICE 如实说明口径（不提报名册数据）', () => {
    expect(PERSONAL_SPACE_NOTICE).toContain('切换');
    expect(PERSONAL_SPACE_NOTICE).toContain('personal');
  });
});
