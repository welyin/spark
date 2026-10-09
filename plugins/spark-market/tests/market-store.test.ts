/**
 * spark-market 展示助手测试（market-store.ts，移植自壳层 apps-store/apps-explore
 * 的插件可自足子集）：色板/分类映射/搜索过滤/权限展示名/广播索引展示口径
 * （corrected 优先 + icon 白名单）/ 声明前提展示 / releaseUrl 推导。
 *
 * 经 app 的 vitest 跑（plugins 目录不持有 node_modules，见 package.json test 脚本）。
 */
import { describe, expect, it } from 'vitest';
import type { PluginMarketAnnounceEntry, PluginMarketItem } from '../../../packages/plugin-sdk/src';
import {
  announceCategoryLabel,
  announceDisplayIcon,
  announceDisplayName,
  announceDisplaySummary,
  announceDisplayVersion,
  announceMatches,
  announceReleaseUrl,
  filterMyAnnounces,
  filterVerifiedAnnounces,
  hashGradient,
  MARKET_CATEGORIES,
  marketCategoryOf,
  marketInitialView,
  marketItemMatches,
  PERMISSION_LABELS,
  permissionLabel,
  platformsText,
  safeAnnounceIcon,
  shuffleAnnounces,
  sortAnnouncesByUpdated,
  supportedSpacesText
} from '../src/market-store';

/** 广播索引条目夹具（PluginMarketAnnounce 全字段 + 索引元数据） */
function mkEntry(
  id: string,
  overrides: Partial<PluginMarketAnnounceEntry> = {},
  announceOverrides: Partial<PluginMarketAnnounceEntry['announce']> = {}
): PluginMarketAnnounceEntry {
  return {
    announce: {
      id,
      name: `应用-${id}`,
      icon: '',
      summary: '简介',
      category: 'tool',
      version: '0.1.0',
      releaseUrl: '',
      type: 'plugin-announce',
      timestamp: 1000,
      ttl: 86400,
      publisher: 'root-publisher',
      pubKey: 'pub',
      pow: { bits: 20, nonce: 1 },
      signature: 'sig',
      ...announceOverrides
    },
    firstSeenAt: 1000,
    updatedAt: 1000,
    verified: 'pending',
    verifyError: '',
    verifiedAt: 0,
    ...overrides
  };
}

function mkItem(overrides: Partial<PluginMarketItem> = {}): PluginMarketItem {
  return {
    id: 'github.com/acme/todo',
    domain: 'plugin:todo',
    name: '待办',
    icon: '',
    description: '任务管理',
    category: 'tool',
    version: '0.1.0',
    views: [],
    permissions: [],
    package: { updateManifestUrl: '', signatureUrl: '', packageName: '', installCommand: '' },
    installed: false,
    enabled: false,
    installedVersion: null,
    latestVersion: null,
    updateAvailable: false,
    lastCheckedAt: null,
    lastCheckReason: '',
    grantedPermissions: [],
    ...overrides
  };
}

describe('哈希渐变色板（与壳层 utils/palette 同口径）', () => {
  it('同 seed 同色（确定性），产出 linear-gradient', () => {
    expect(hashGradient('spark-chat')).toBe(hashGradient('spark-chat'));
    expect(hashGradient('spark-chat')).toMatch(/^linear-gradient\(135deg, #[0-9a-f]{6}, #[0-9a-f]{6}\)$/);
    expect(hashGradient('')).toBe(hashGradient('')); // 空 seed 回退 'spark'
  });
});

describe('市场分类映射（ui-apps-market §3.3）', () => {
  it('枚举类映射中文名，未知类归「其他」', () => {
    expect(marketCategoryOf(mkItem({ category: 'foundation' }))).toBe('基础');
    expect(marketCategoryOf(mkItem({ category: 'ai-assistant' }))).toBe('AI 助手');
    expect(marketCategoryOf(mkItem({ category: 'social' }))).toBe('社交');
    expect(marketCategoryOf(mkItem({ category: 'tool' }))).toBe('工具');
    expect(marketCategoryOf(mkItem({ category: 'game' }))).toBe('游戏');
    expect(marketCategoryOf(mkItem({ category: 'other' as PluginMarketItem['category'] }))).toBe('其他');
    expect(MARKET_CATEGORIES).toContain('其他');
  });
});

describe('市场搜索过滤（名称/简介/开发者域名，大小写不敏感）', () => {
  const item = mkItem({ name: '待办 Todo', description: '任务管理', domain: 'plugin:Todo-App' });

  it('空关键词放行全部', () => {
    expect(marketItemMatches(item, '')).toBe(true);
    expect(marketItemMatches(item, '   ')).toBe(true);
  });

  it('命中名称/简介/域名任一即匹配', () => {
    expect(marketItemMatches(item, 'todo')).toBe(true); // 名称（大小写不敏感）
    expect(marketItemMatches(item, '任务')).toBe(true); // 简介
    expect(marketItemMatches(item, 'todo-app')).toBe(true); // 域名
    expect(marketItemMatches(item, '聊天')).toBe(false);
  });
});

describe('起步子视图（A34 initial-view 直达）', () => {
  it("ctx.viewId==='market' 直达市场页；其余/缺省按清单页起步", () => {
    expect(marketInitialView('market')).toBe('market');
    expect(marketInitialView('default')).toBe('list');
    expect(marketInitialView(undefined)).toBe('list');
    expect(marketInitialView(null)).toBe('list');
    expect(marketInitialView('unknown-view')).toBe('list');
  });
});

describe('权限展示名', () => {
  it('已知权限给中文名（含 A34 市场位），未知原样返回', () => {
    expect(permissionLabel('market:read')).toBe('读取应用市场目录');
    expect(permissionLabel('market:write')).toBe('安装/更新/卸载应用');
    expect(permissionLabel('storage:read')).toBe('读取本域数据');
    expect(permissionLabel('future:perm')).toBe('future:perm');
    // A34 两位已登记进展示表（与内核 permissions.rs 对齐）
    expect(Object.keys(PERMISSION_LABELS)).toEqual(expect.arrayContaining(['market:read', 'market:write']));
  });
});

describe('广播索引过滤（plugin-dist §8.7/§8.8）', () => {
  it('探索视图只收 verified 条目', () => {
    const entries = [
      mkEntry('a', { verified: 'verified' }),
      mkEntry('b', { verified: 'pending' }),
      mkEntry('c', { verified: 'failed' })
    ];
    expect(filterVerifiedAnnounces(entries).map((e) => e.announce.id)).toEqual(['a']);
  });

  it('开发者页按 publisher==我的 rootId 过滤（不做 verified 过滤；空 rootId 为空清单）', () => {
    const entries = [
      mkEntry('a', { verified: 'pending' }, { publisher: 'root-me' }),
      mkEntry('b', { verified: 'verified' }, { publisher: 'root-other' })
    ];
    expect(filterMyAnnounces(entries, 'root-me').map((e) => e.announce.id)).toEqual(['a']);
    expect(filterMyAnnounces(entries, '')).toEqual([]);
  });

  it('洗牌不改入参、为同集置换（注入 rng 确定性）', () => {
    const entries = [mkEntry('a'), mkEntry('b'), mkEntry('c'), mkEntry('d')];
    const shuffled = shuffleAnnounces(entries, () => 0);
    expect(entries.map((e) => e.announce.id)).toEqual(['a', 'b', 'c', 'd']); // 入参不变
    expect([...shuffled].map((e) => e.announce.id).sort()).toEqual(['a', 'b', 'c', 'd']);
    expect(shuffled.map((e) => e.announce.id)).toEqual(['b', 'c', 'd', 'a']); // rng=0 时逐位与 0 位交换
  });

  it('稳定序：updatedAt 降序且不改入参', () => {
    const entries = [mkEntry('a', { updatedAt: 1 }), mkEntry('b', { updatedAt: 3 }), mkEntry('c', { updatedAt: 2 })];
    expect(sortAnnouncesByUpdated(entries).map((e) => e.announce.id)).toEqual(['b', 'c', 'a']);
    expect(entries.map((e) => e.announce.id)).toEqual(['a', 'b', 'c']);
  });

  it('探索搜索：命中名称/简介/插件 id 任一', () => {
    const entry = mkEntry('github.com/acme/todo', {}, { name: '待办', summary: '任务管理' });
    expect(announceMatches(entry, 'acme/todo')).toBe(true);
    expect(announceMatches(entry, '待办')).toBe(true);
    expect(announceMatches(entry, '任务')).toBe(true);
    expect(announceMatches(entry, '聊天')).toBe(false);
    expect(announceMatches(entry, '')).toBe(true);
  });
});

describe('广播索引展示口径（corrected 优先，announce 自报值仅占位）', () => {
  it('corrected 字段存在时一律以 corrected 为准', () => {
    const entry = mkEntry(
      'a',
      { corrected: { name: '校正名', icon: 'https://cdn.example.com/i.png', summary: '校正简介', version: '9.9.9' } },
      { name: '自报名', summary: '自报简介', version: '0.0.1' }
    );
    expect(announceDisplayName(entry)).toBe('校正名');
    expect(announceDisplaySummary(entry)).toBe('校正简介');
    expect(announceDisplayVersion(entry)).toBe('9.9.9');
    expect(announceDisplayIcon(entry)).toBe('https://cdn.example.com/i.png');
  });

  it('无 corrected 回落 announce 自报值', () => {
    const entry = mkEntry('a', {}, { name: '自报名', summary: '自报简介', version: '0.0.1' });
    expect(announceDisplayName(entry)).toBe('自报名');
    expect(announceDisplaySummary(entry)).toBe('自报简介');
    expect(announceDisplayVersion(entry)).toBe('0.0.1');
  });
});

describe('icon 渲染白名单（防 javascript:/畸形 scheme）', () => {
  it('仅 https URL 与 data:image/ 内联图放行', () => {
    expect(safeAnnounceIcon('https://example.com/icon.png')).toBe('https://example.com/icon.png');
    expect(safeAnnounceIcon('data:image/png;base64,iVBOR')).toBe('data:image/png;base64,iVBOR');
    expect(safeAnnounceIcon('http://example.com/icon.png')).toBe('');
    expect(safeAnnounceIcon('javascript:alert(1)')).toBe('');
    expect(safeAnnounceIcon('file:///etc/passwd')).toBe('');
    expect(safeAnnounceIcon('')).toBe('');
  });

  it('announceDisplayIcon 对 corrected/自报值都过白名单', () => {
    const evil = mkEntry('a', { corrected: { name: 'x', icon: 'javascript:alert(1)', summary: '', version: '1' } });
    expect(announceDisplayIcon(evil)).toBe('');
  });
});

describe('广播声明分类展示名', () => {
  it('枚举映射中文名；未知值原样展示；空值归「其他」', () => {
    expect(announceCategoryLabel('foundation')).toBe('基础');
    expect(announceCategoryLabel('game')).toBe('游戏');
    expect(announceCategoryLabel('future-category')).toBe('future-category');
    expect(announceCategoryLabel('')).toBe('其他');
  });
});

describe('声明/前提展示助手', () => {
  it('平台约束文案：空=全平台不展示；单平台「仅 X」；双平台并列', () => {
    expect(platformsText(undefined)).toBe('');
    expect(platformsText({ platforms: [] })).toBe('');
    expect(platformsText({ platforms: ['desktop'] })).toBe('仅桌面端');
    expect(platformsText({ platforms: ['mobile'] })).toBe('仅移动端');
    expect(platformsText({ platforms: ['desktop', 'mobile'] })).toBe('桌面端与移动端');
  });

  it('支持空间文案：未声明按 [org] 口径', () => {
    expect(supportedSpacesText(undefined)).toBe('仅组织空间');
    expect(supportedSpacesText([])).toBe('仅组织空间');
    expect(supportedSpacesText(['personal'])).toBe('仅个人空间');
    expect(supportedSpacesText(['personal', 'org'])).toBe('个人与组织空间');
  });
});

describe('releaseUrl 推导（plugin-dist §2.2 tag 规则）', () => {
  it('根仓库：v<version>', () => {
    expect(announceReleaseUrl({ id: 'github.com/acme/todo', version: '0.2.0' })).toBe(
      'https://github.com/acme/todo/releases/tag/v0.2.0'
    );
  });

  it('monorepo：<末段>-v<version>', () => {
    expect(announceReleaseUrl({ id: 'github.com/acme/mono/plugins/todo', version: '1.0.0' })).toBe(
      'https://github.com/acme/mono/releases/tag/todo-v1.0.0'
    );
  });
});
