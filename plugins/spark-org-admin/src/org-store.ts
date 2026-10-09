/**
 * spark-org-admin 共享逻辑与展示助手（纯函数，可单测；移植自壳层
 * components/org/OrgSettingsPanel.vue / PurgeDataPanel.vue 的内联逻辑 +
 * utils/palette.ts 的插件可自足子集）。
 *
 * 真实数据一律来自 sdk.org / sdk.policy（桥）；壳层 localStorage 缓存
 * （org-avatars 展示缓存等）不在插件内——OrgView.avatar 由内核持久化同步，
 * 见任务报告迁移对照表。
 */
import type {
  PluginOrgAPI,
  PluginOrgMember,
  PluginOrgPurgePreview,
  PluginOrgSyncOverview,
  PluginOrgView,
  PluginSpaceContext
} from '../../../packages/plugin-sdk/src';

export type { PluginOrgMember, PluginOrgSyncOverview, PluginOrgView };

// ── 管理界面子项（org 空间第三栏子菜单；与壳层 OrgSettingsPanel sections 同口径扩充） ──

export type OrgSectionKey =
  | 'info'
  | 'roster'
  | 'policy'
  | 'public'
  | 'discover'
  | 'recover'
  | 'purge'
  | 'membership';

export type OrgSectionDef = {
  key: OrgSectionKey;
  label: string;
  /** 仅管理员可见/可操作的子项（非管理员仍可见但面板内自行降级，此处仅标注） */
  adminOnly?: boolean;
};

export const ORG_SECTIONS: OrgSectionDef[] = [
  { key: 'info', label: '组织信息' },
  { key: 'roster', label: '成员名册' },
  { key: 'policy', label: '策略配置' },
  { key: 'public', label: '公开设置' },
  { key: 'discover', label: '发现公开组织' },
  { key: 'recover', label: '找回组织' },
  { key: 'purge', label: '数据治理', adminOnly: true },
  { key: 'membership', label: '创建 / 加入组织' }
];

export function orgSectionLabel(key: OrgSectionKey | null): string {
  return ORG_SECTIONS.find((section) => section.key === key)?.label ?? '';
}

// ── 空间口径（A42 评审决议） ──

/**
 * personal 空间提示文案：sdk.org.listMine 与老面 runtime.listMineOrganizations
 * 口径统一为 personal 拒绝（personal 空间无组织可管，不向插件枚举本机组织名册），
 * 插件 personal 视图据此不调 listMine，如实提示用户切换空间。
 */
export const PERSONAL_SPACE_NOTICE =
  '组织管理以空间为边界：personal（个人）空间不提供组织名册视图，请在空间列表切换到对应组织空间后管理。下方仍可创建新组织或通过邀请码加入。';

/**
 * 当前空间组织加载：org 空间经 sdk.org.listMine 取名册并定位本空间组织；
 * personal 空间一律不调 listMine（桥 ORG_SPACE_CALLS 拒绝），返回 null 由 UI 如实提示。
 */
export async function loadSpaceOrganization(
  api: PluginOrgAPI,
  space: PluginSpaceContext
): Promise<PluginOrgView | null> {
  if (space.type !== 'org') {
    return null;
  }
  const orgs = await api.listMine();
  return orgs.find((org) => org.orgId === space.id) ?? null;
}

// ── 副本健康度（移植 OrgSettingsPanel replicaLabel/replicaTagType/memberReplicasOk） ──

export function replicaLabel(item: PluginOrgSyncOverview | null): string {
  if (!item) {
    return '';
  }
  return `副本 ${item.syncedPeers}/${item.replicaTarget}`;
}

export function replicaTagType(item: PluginOrgSyncOverview | null): 'success' | 'warning' | 'info' {
  if (!item) {
    return 'info';
  }
  return item.syncedPeers >= item.replicaTarget ? 'success' : 'warning';
}

/** 全员数据节点：全体成员 PC 副本合计 ≥3（集体口径，membership §4.1） */
export function memberReplicasOk(item: PluginOrgSyncOverview | null): boolean {
  const accounts = item?.memberReplicas;
  if (!accounts || accounts.length === 0) {
    return true;
  }
  return accounts.filter((account) => account.pcSynced).length >= 3;
}

/** 写操作前网络提示判定：组织网络丢失/仅本地时提示「数据将在恢复后同步」（只提示不阻断） */
export function networkDegraded(item: PluginOrgSyncOverview | null): boolean {
  return item?.status === 'lost' || item?.status === 'localOnly';
}

// ── 名册 ──

/** 名册排序：管理员在前，同级按加入时间升序（返回新数组） */
export function sortedMembers(members: PluginOrgMember[]): PluginOrgMember[] {
  return [...members].sort((a, b) => {
    if (a.role !== b.role) {
      return a.role === 'admin' ? -1 : 1;
    }
    return a.joinedAt - b.joinedAt;
  });
}

/** 成员展示名：组织内昵称 → rootId 短形 */
export function memberDisplayName(member: PluginOrgMember): string {
  return member.nickname?.trim() || shortRootId(member.rootId);
}

export function shortRootId(rootId: string): string {
  return rootId.length > 12 ? `${rootId.slice(0, 6)}…${rootId.slice(-4)}` : rootId;
}

// ── 名片解析（预录成员的名片输入；壳层 utils/card.ts parseCard 的最小自足子集） ──

export type ParsedMemberCard = {
  rootId: string;
  peerId?: string;
  addresses?: string[];
};

const ROOT_ID_PATTERN = /[0-9a-f]{64}/;

/**
 * 从名片内容中解析成员标识：rootId 必须可识别（64 位小写 hex，JSON 字段或
 * 原文匹配均可）；peerId/addresses 为可选寻址线索（JSON 名片携带时提取）。
 * 无法识别 rootId 返回 null。
 */
export function parseMemberCard(raw: string): ParsedMemberCard | null {
  const text = raw.trim();
  if (!text) {
    return null;
  }
  // JSON 名片：rootId/peerId/addresses 字段（大小写不敏感逐一尝试常见键名）
  try {
    const parsed = JSON.parse(text) as Record<string, unknown>;
    if (parsed && typeof parsed === 'object') {
      const rootId = [parsed.rootId, parsed.rootID, parsed.id, parsed.identity]
        .find((value): value is string => typeof value === 'string' && ROOT_ID_PATTERN.test(value));
      if (rootId) {
        const match = rootId.match(ROOT_ID_PATTERN);
        const peerId = typeof parsed.peerId === 'string' && parsed.peerId ? parsed.peerId : undefined;
        const addresses = Array.isArray(parsed.addresses)
          ? parsed.addresses.filter((item): item is string => typeof item === 'string' && item.length > 0)
          : undefined;
        return {
          rootId: match![0],
          ...(peerId ? { peerId } : {}),
          ...(addresses && addresses.length ? { addresses } : {})
        };
      }
    }
  } catch {
    // 非 JSON：走原文正则
  }
  const match = text.match(ROOT_ID_PATTERN);
  if (!match) {
    return null;
  }
  return { rootId: match[0] };
}

// ── 数据治理（移植 PurgeDataPanel 的判定逻辑） ──

/** K 副本充足（预览返回的副本口径 syncedPeers ≥ replicaTarget；无副本信息=不足） */
export function purgeReplicaSufficient(preview: PluginOrgPurgePreview | null): boolean {
  const replica = preview?.replica;
  return Boolean(replica) && replica!.syncedPeers >= replica!.replicaTarget;
}

/** 可执行清理：预览在 + 管理员 + 有影响数据 + 副本充足 + 已确认导出（纵深防御，服务端仍校验） */
export function purgeExecutable(preview: PluginOrgPurgePreview | null, confirmExported: boolean): boolean {
  return Boolean(
    preview &&
      preview.isCurrentUserAdmin &&
      preview.preview.affectedDocs > 0 &&
      purgeReplicaSufficient(preview) &&
      confirmExported
  );
}

/** 选中日期的 00:00（本地时区）作为清理水位 */
export function purgeBeforeTsOf(date: Date | null): number {
  if (!date) {
    return 0;
  }
  const day = new Date(date);
  day.setHours(0, 0, 0, 0);
  return day.getTime();
}

// ── 策略配置 ──

/** 策略文档 JSON 解析（编辑器输入校验；内核侧结构/引擎校验在 submitDraft） */
export function parsePolicyDoc(text: string): { doc: Record<string, unknown> } | { error: string } {
  const trimmed = text.trim();
  if (!trimmed) {
    return { error: '策略文档不能为空' };
  }
  try {
    const doc = JSON.parse(trimmed) as unknown;
    if (!doc || typeof doc !== 'object' || Array.isArray(doc)) {
      return { error: '策略文档必须是 JSON 对象' };
    }
    return { doc: doc as Record<string, unknown> };
  } catch (error) {
    return { error: `JSON 解析失败：${error instanceof Error ? error.message : String(error)}` };
  }
}

// ── 通用展示 ──

export function formatDate(timestamp: number): string {
  return new Intl.DateTimeFormat('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit'
  }).format(new Date(timestamp));
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) {
    return '0 B';
  }
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 'B';
  for (const next of units) {
    if (value < 1024) {
      break;
    }
    value /= 1024;
    unit = next;
  }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${unit}`;
}

// ── 哈希渐变色板（与壳层 utils/palette.ts 同一套配色，同 seed 同色） ──

const PALETTES: Array<[string, string]> = [
  ['#3296fa', '#2b83dd'],
  ['#7b61ff', '#5a3fd6'],
  ['#00b8a9', '#008577'],
  ['#f7b500', '#e08600'],
  ['#f54a45', '#cf352f'],
  ['#eb2f96', '#c41d7f'],
  ['#34c19b', '#1f9c7c'],
  ['#ff7d00', '#e56a00']
];

function hashOf(seed: string): number {
  let hash = 0;
  for (const char of seed || 'spark') {
    hash = (hash * 31 + (char.codePointAt(0) ?? 0)) >>> 0;
  }
  return hash;
}

/** 组织自动头像渐变（与壳层 hashGradient 同口径） */
export function hashGradient(seed: string): string {
  const [from, to] = PALETTES[hashOf(seed) % PALETTES.length];
  return `linear-gradient(135deg, ${from}, ${to})`;
}
