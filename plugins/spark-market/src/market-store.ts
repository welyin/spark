/**
 * 市场插件共享常量与展示助手（移植自壳层 components/apps/apps-store.ts +
 * utils/palette.ts + components/apps/app-icon.ts 的插件可自足子集）。
 *
 * 真实数据一律来自 sdk.market（桥）；壳层 localStorage mock 面（分组/最近使用/
 * 空间启用）不在插件内——见任务报告遗留。
 */
import type { PluginMarketAnnounceEntry, PluginMarketItem, PluginRequires } from '../../../packages/plugin-sdk/src';

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

/** 应用图标哈希渐变（与壳层 hashGradient 同口径） */
export function hashGradient(seed: string): string {
  const [from, to] = PALETTES[hashOf(seed) % PALETTES.length];
  return `linear-gradient(135deg, ${from}, ${to})`;
}

// ── 市场分类（ui-apps-market §3.3，与壳层 apps-store 同口径） ──

export type MarketCategory = '基础' | '社交' | '工具' | 'AI 助手' | '游戏' | '其他';
export const MARKET_CATEGORIES: MarketCategory[] = ['基础', '社交', '工具', 'AI 助手', '游戏', '其他'];

export function marketCategoryOf(item: Pick<PluginMarketItem, 'category'>): MarketCategory {
  switch (item.category) {
    case 'foundation':
      return '基础';
    case 'ai-assistant':
      return 'AI 助手';
    case 'social':
      return '社交';
    case 'tool':
      return '工具';
    case 'game':
      return '游戏';
    default:
      return '其他';
  }
}

/** 市场搜索：按名称、简介、开发者（域名）过滤（ui-apps-market §3.2） */
export function marketItemMatches(
  item: Pick<PluginMarketItem, 'name' | 'description' | 'domain'>,
  keyword: string
): boolean {
  const kw = keyword.trim().toLowerCase();
  if (!kw) {
    return true;
  }
  return [item.name, item.description, item.domain].some((field) => field.toLowerCase().includes(kw));
}

// ── 起步子视图（A34 initial-view 直达） ──

/** 起步子视图：壳层 BuiltinAppHost initial-view 经桥 ctx.viewId 透传
 *  （'market' = Dock「应用市场」入口直达市场页）；其余/缺省按清单页起步 */
export function marketInitialView(viewId: string | undefined | null): 'list' | 'market' {
  return viewId === 'market' ? 'market' : 'list';
}

// ── 权限展示名（与壳层 apps-store PERMISSION_LABELS 同口径 + A34 市场位） ──

export const PERMISSION_LABELS: Record<string, string> = {
  'storage:read': '读取本域数据',
  'storage:write': '写入本域数据',
  'org:read': '读取组织信息',
  'org:sync': '同步组织数据',
  'network:broadcast': '网络广播',
  'proof:verify': '存证核验',
  'identity:sign': '域身份签名',
  'message:app': '发送应用消息',
  'affairs:read': '读取共同体事务',
  'affairs:write': '关注/提交共同体事务',
  'credentials:read': '读取资格凭证',
  'policy:read': '读取策略文档',
  'policy:write': '提交策略文档草稿',
  'messages:read': '读取会话与消息',
  'messages:write': '发送/管理消息',
  'contacts:read': '读取通讯录',
  'contacts:write': '管理通讯录',
  'feed:read': '接收社交投递',
  'feed:write': '发送社交投递',
  'market:read': '读取应用市场目录',
  'market:write': '安装/更新/卸载应用'
};

export function permissionLabel(permission: string): string {
  return PERMISSION_LABELS[permission] ?? permission;
}

// ── 广播索引展示助手（移植自壳层 components/apps/apps-explore.ts，逐函数同语义） ──

/** 只有核查通过（verified）的条目可进探索视图（plugin-dist §8.7）。 */
export function filterVerifiedAnnounces(entries: PluginMarketAnnounceEntry[]): PluginMarketAnnounceEntry[] {
  return entries.filter((entry) => entry.verified === 'verified');
}

/** 开发者页：本地索引中由我发布的广播条目（publisher == 我的 rootId）；
 *  不做 verified 过滤（自己发布但未过核查的条目带状态标签展示）；rootId 空 = 空清单 */
export function filterMyAnnounces(entries: PluginMarketAnnounceEntry[], rootId: string): PluginMarketAnnounceEntry[] {
  if (!rootId) {
    return [];
  }
  return entries.filter((entry) => entry.announce.publisher === rootId);
}

/** Fisher-Yates 洗牌（返回新数组，不改入参；rng 可注入便于测试）。 */
export function shuffleAnnounces<T>(items: T[], rng: () => number = Math.random): T[] {
  const out = [...items];
  for (let i = out.length - 1; i > 0; i -= 1) {
    const j = Math.floor(rng() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

/** 稳定序（搜索直达用）：updatedAt 降序（返回新数组）。 */
export function sortAnnouncesByUpdated(entries: PluginMarketAnnounceEntry[]): PluginMarketAnnounceEntry[] {
  return [...entries].sort((a, b) => b.updatedAt - a.updatedAt);
}

/** 探索页搜索：按名称 / 简介 / 插件 id（仓库地址）过滤，大小写不敏感 */
export function announceMatches(entry: PluginMarketAnnounceEntry, keyword: string): boolean {
  const kw = keyword.trim().toLowerCase();
  if (!kw) {
    return true;
  }
  return [announceDisplayName(entry), announceDisplaySummary(entry), entry.announce.id].some((field) =>
    field.toLowerCase().includes(kw)
  );
}

/** 展示字段口径（plugin-dist §8.8）：一律以 corrected 为准，announce 自报值仅占位 */
export function announceDisplayName(entry: PluginMarketAnnounceEntry): string {
  return entry.corrected?.name || entry.announce.name;
}

export function announceDisplaySummary(entry: PluginMarketAnnounceEntry): string {
  return entry.corrected?.summary || entry.announce.summary;
}

export function announceDisplayVersion(entry: PluginMarketAnnounceEntry): string {
  return entry.corrected?.version || entry.announce.version;
}

/** icon 渲染白名单：仅 https URL 与 data:image/ 内联图（防 javascript:/畸形 scheme） */
export function safeAnnounceIcon(icon: string): string {
  return icon.startsWith('https://') || icon.startsWith('data:image/') ? icon : '';
}

export function announceDisplayIcon(entry: PluginMarketAnnounceEntry): string {
  return safeAnnounceIcon(entry.corrected?.icon || entry.announce.icon);
}

/** 广播声明分类的展示名（目录粗分类映射，未知值原样展示）。 */
export function announceCategoryLabel(category: string): string {
  if (!category) return '其他';
  const map: Record<string, string> = {
    foundation: '基础',
    'ai-assistant': 'AI 助手',
    social: '社交',
    tool: '工具',
    game: '游戏'
  };
  return map[category] ?? category;
}

// ── 声明/前提展示助手（移植自壳层 AppInstallTools/app-icon 同语义子集） ──

/** 平台约束展示文案（requires.platforms；空 = 全平台不展示） */
export function platformsText(requires?: PluginRequires): string {
  const platforms = requires?.platforms ?? [];
  if (platforms.length === 0) {
    return '';
  }
  const labels = platforms.map((platform) => (platform === 'desktop' ? '桌面端' : '移动端'));
  return labels.length === 2 ? '桌面端与移动端' : `仅${labels[0]}`;
}

/** supportedSpaces 展示（spaces-and-plugins §4）：未声明按 ['org'] 口径 */
export function supportedSpacesText(spaces?: Array<'personal' | 'org'>): string {
  const effective = spaces && spaces.length > 0 ? spaces : ['org'];
  const labels = effective.map((space) => (space === 'personal' ? '个人空间' : '组织空间'));
  return labels.length === 2 ? '个人与组织空间' : `仅${labels[0]}`;
}

/** 发布声明的 releaseUrl：按 plugin-dist §2.2 tag 规则从声明 id/version 推导
 *  （根仓库 v<version>，monorepo <末段>-v<version>） */
export function announceReleaseUrl(declaration: { id: string; version: string }): string {
  const segments = declaration.id.split('/');
  const base = segments.slice(0, 3).join('/');
  const tag =
    segments.length > 3 ? `${segments[segments.length - 1]}-v${declaration.version}` : `v${declaration.version}`;
  return `https://${base}/releases/tag/${tag}`;
}
