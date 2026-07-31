/**
 * 市场「探索」分区纯逻辑单测（apps-explore.ts）：
 * verified 过滤 / 洗牌（保多重集合 + rng 可注入）/ 搜索过滤 / 稳定序。
 */
import { describe, expect, it } from 'vitest';
import type { PluginAnnounceIndexEntryDto } from '../../api/types';
import {
  announceCategoryLabel,
  announceDisplayIcon,
  announceDisplayName,
  announceDisplaySummary,
  announceDisplayVersion,
  announceMatches,
  filterVerifiedAnnounces,
  safeAnnounceIcon,
  shuffleAnnounces,
  sortAnnouncesByUpdated
} from './apps-explore';

function entry(
  id: string,
  verified: PluginAnnounceIndexEntryDto['verified'],
  overrides: Partial<PluginAnnounceIndexEntryDto['announce']> = {},
  updatedAt = 0,
  corrected?: PluginAnnounceIndexEntryDto['corrected']
): PluginAnnounceIndexEntryDto {
  return {
    announce: {
      id,
      name: `插件${id}`,
      icon: '',
      summary: `简介${id}`,
      category: 'business',
      version: '1.0.0',
      releaseUrl: '',
      type: 'plugin-announce',
      timestamp: 0,
      ttl: 0,
      publisher: 'publisher',
      pubKey: 'pubkey',
      pow: { bits: 20, nonce: 0 },
      signature: 'sig',
      ...overrides
    },
    firstSeenAt: updatedAt,
    updatedAt,
    verified,
    verifyError: '',
    verifiedAt: 0,
    corrected
  };
}

describe('filterVerifiedAnnounces', () => {
  it('只保留 verified 条目（pending/failed 不进探索视图）', () => {
    const entries = [
      entry('a', 'verified'),
      entry('b', 'pending'),
      entry('c', 'failed'),
      entry('d', 'verified')
    ];
    expect(filterVerifiedAnnounces(entries).map((e) => e.announce.id)).toEqual(['a', 'd']);
  });
});

describe('shuffleAnnounces', () => {
  it('洗牌保多重集合、不改入参', () => {
    const input = [1, 2, 3, 4, 5, 6, 7, 8];
    const snapshot = [...input];
    const out = shuffleAnnounces(input);
    expect(input).toEqual(snapshot);
    expect([...out].sort((a, b) => a - b)).toEqual(snapshot);
  });

  it('注入确定性 rng 时结果可复现', () => {
    // rng 恒返回 0：每轮把末位换到第 0 位
    const out = shuffleAnnounces([1, 2, 3], () => 0);
    expect([...out].sort()).toEqual([1, 2, 3]);
    expect(shuffleAnnounces([1, 2, 3], () => 0)).toEqual(out);
  });
});

describe('announceMatches', () => {
  const target = entry('github.com/acme/todo', 'verified', { name: '待办事项', summary: '团队任务管理' });

  it('空关键字恒匹配', () => {
    expect(announceMatches(target, '  ')).toBe(true);
  });

  it('按名称 / 简介 / id 匹配，大小写不敏感', () => {
    expect(announceMatches(target, '待办')).toBe(true);
    expect(announceMatches(target, '任务管理')).toBe(true);
    expect(announceMatches(target, 'ACME/TODO')).toBe(true);
    expect(announceMatches(target, '不存在')).toBe(false);
  });
});

describe('sortAnnouncesByUpdated', () => {
  it('按 updatedAt 降序（搜索稳定序），不改入参', () => {
    const entries = [entry('a', 'verified', {}, 1), entry('b', 'verified', {}, 3), entry('c', 'verified', {}, 2)];
    const out = sortAnnouncesByUpdated(entries);
    expect(out.map((e) => e.announce.id)).toEqual(['b', 'c', 'a']);
    expect(entries.map((e) => e.announce.id)).toEqual(['a', 'b', 'c']);
  });
});

describe('announceCategoryLabel', () => {
  it('粗分类映射，未知值原样', () => {
    expect(announceCategoryLabel('foundation')).toBe('基础');
    expect(announceCategoryLabel('business')).toBe('应用');
    expect(announceCategoryLabel('social')).toBe('social');
    expect(announceCategoryLabel('')).toBe('其他');
  });
});

describe('展示字段（plugin-dist §8.8 corrected 优先）', () => {
  const corrected = { name: '校正版待办', icon: 'https://cdn.example.com/icon.png', summary: '校正简介', version: '2.0.0' };

  it('corrected 在场时展示校正值，announce 自报值不展示', () => {
    const e = entry('a', 'verified', { name: '自报名', summary: '自报简介', version: '1.0.0' }, 0, corrected);
    expect(announceDisplayName(e)).toBe('校正版待办');
    expect(announceDisplaySummary(e)).toBe('校正简介');
    expect(announceDisplayVersion(e)).toBe('2.0.0');
    expect(announceDisplayIcon(e)).toBe('https://cdn.example.com/icon.png');
  });

  it('corrected 缺席时回落 announce 自报值（占位）', () => {
    const e = entry('a', 'verified', { name: '自报名', summary: '自报简介', version: '1.0.0' });
    expect(announceDisplayName(e)).toBe('自报名');
    expect(announceDisplaySummary(e)).toBe('自报简介');
    expect(announceDisplayVersion(e)).toBe('1.0.0');
    expect(announceDisplayIcon(e)).toBe('');
  });

  it('搜索匹配校正后的名称/简介', () => {
    const e = entry('a', 'verified', { name: '自报名', summary: '自报简介' }, 0, corrected);
    expect(announceMatches(e, '校正版')).toBe(true);
    expect(announceMatches(e, '校正简介')).toBe(true);
    expect(announceMatches(e, '自报名')).toBe(false);
  });
});

describe('safeAnnounceIcon（渲染白名单）', () => {
  it('仅放行 https 与 data:image/，其余一律空串', () => {
    expect(safeAnnounceIcon('https://cdn.example.com/x.png')).toBe('https://cdn.example.com/x.png');
    expect(safeAnnounceIcon('data:image/png;base64,AA==')).toBe('data:image/png;base64,AA==');
    expect(safeAnnounceIcon('data:text/html,<script>1</script>')).toBe('');
    expect(safeAnnounceIcon('javascript:alert(1)')).toBe('');
    expect(safeAnnounceIcon('http://evil.com/x.png')).toBe('');
    expect(safeAnnounceIcon('file:///etc/passwd')).toBe('');
    expect(safeAnnounceIcon('')).toBe('');
  });

  it('announceDisplayIcon 对 corrected/announce 值统一过白名单', () => {
    const bad = entry('a', 'verified', { icon: 'javascript:alert(1)' });
    expect(announceDisplayIcon(bad)).toBe('');
  });
});
