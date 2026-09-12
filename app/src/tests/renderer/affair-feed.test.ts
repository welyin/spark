// affair-feed 数据源扩展回归（A6/A7/G8）：
// - A7 公示期：state=pending 决议的链上锚定时刻/公示期被如实带出；未锚定为 null（不拿本机时钟冒充）；
// - G8 决议≠已执行：已关闭事务追加 readExec，汇总执行回报（reported/total）；
//   进行中事务不多发 readExec；非执行型（exec=null）不计；
// - A6 组合筛选：同维度 OR、跨维度 AND；collectAffairTags 去重。
import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  affairFeed,
  collectAffairTags,
  EMPTY_FILTERS,
  filterAffairs,
  refreshAffairFeed,
  type AffairFeedItem
} from '../../stores/affairs/affair-feed';

function genesis(title: string, tags: string[] = []) {
  return { title, summary: '', tags, initiator: { identity: 'root-me' }, createdAt: 1700000000000 };
}

function setupApi(options: {
  ids: string[];
  resolutions?: Record<string, unknown>;
  exec?: Record<string, unknown>;
}) {
  const readExec = vi.fn().mockImplementation(async (affairId: string) => options.exec?.[affairId] ?? { affairId, nowMs: 0, exec: null, states: [] });
  (window as any).electronAPI = {
    affairs: {
      listFollowed: vi.fn().mockResolvedValue(options.ids),
      readLog: vi.fn().mockImplementation(async (affairId: string) => ({
        affairId,
        genesis: genesis(`议题-${affairId}`),
        ops: [],
        heads: [],
        followedAt: 1700000000000
      })),
      readResolution: vi.fn().mockImplementation(async (affairId: string) => ({
        affairId,
        resolutions: options.resolutions?.[affairId] ?? []
      })),
      readExec
    }
  };
  return { readExec };
}

beforeEach(() => {
  affairFeed.value = [];
});

describe('refreshAffairFeed 扩展字段（A7/G8）', () => {
  it('pending 决议带出公示期（链上锚定时刻 + 公示期时长）', async () => {
    setupApi({
      ids: ['a1'],
      resolutions: {
        a1: [{ state: 'pending', anchoredMs: 1700000100000, pubPeriodMs: 86_400_000 }]
      }
    });
    await refreshAffairFeed();
    expect(affairFeed.value[0].publicity).toEqual({ anchoredMs: 1700000100000, pubPeriodMs: 86_400_000 });
    expect(affairFeed.value[0].closed).toBe(false);
  });

  it('未锚定决议（unanchored 口径）：anchoredMs 为 null，不编造时间', async () => {
    setupApi({
      ids: ['a1'],
      resolutions: { a1: [{ state: 'pending', anchoredMs: null, pubPeriodMs: 3_600_000 }] }
    });
    await refreshAffairFeed();
    expect(affairFeed.value[0].publicity).toEqual({ anchoredMs: null, pubPeriodMs: 3_600_000 });
  });

  it('无 pending 决议时 publicity 为 null', async () => {
    setupApi({ ids: ['a1'], resolutions: { a1: [{ state: 'effective' }] } });
    await refreshAffairFeed();
    expect(affairFeed.value[0].publicity).toBeNull();
    expect(affairFeed.value[0].closed).toBe(true);
  });

  it('G8：已关闭事务读执行回报并汇总（reported/total）；进行中事务不读', async () => {
    const { readExec } = setupApi({
      ids: ['open1', 'closed1'],
      resolutions: {
        closed1: [{ state: 'effective' }]
      },
      exec: {
        closed1: {
          affairId: 'closed1',
          nowMs: 0,
          exec: { kind: 'exec' },
          states: [
            { state: 'returned', reportOpHash: 'r1', anchoredMs: 1, effectiveMs: 2 },
            { state: 'awaiting-execution', reportOpHash: null, anchoredMs: 1, effectiveMs: 2 }
          ]
        }
      }
    });
    await refreshAffairFeed();
    const open = affairFeed.value.find((i) => i.affairId === 'open1')!;
    const closed = affairFeed.value.find((i) => i.affairId === 'closed1')!;
    expect(open.exec).toBeNull();
    expect(closed.exec).toEqual({ total: 2, reported: 1 });
    expect(readExec).toHaveBeenCalledTimes(1);
    expect(readExec).toHaveBeenCalledWith('closed1');
  });

  it('非执行型事务（exec=null）：不计执行摘要', async () => {
    setupApi({
      ids: ['c1'],
      resolutions: { c1: [{ state: 'vetoed' }] },
      exec: { c1: { affairId: 'c1', nowMs: 0, exec: null, states: [] } }
    });
    await refreshAffairFeed();
    expect(affairFeed.value[0].exec).toBeNull();
  });
});

describe('A6 组合筛选（filterAffairs / collectAffairTags）', () => {
  const items: AffairFeedItem[] = [
    { affairId: 'a', title: 'A', summary: '', tags: ['预算'], originator: 'root-me', createdAt: 1, closed: false, following: true, publicity: null, exec: null },
    { affairId: 'b', title: 'B', summary: '', tags: ['预算', '选举'], originator: 'root-other', createdAt: 2, closed: true, following: true, publicity: null, exec: null },
    { affairId: 'c', title: 'C', summary: '', tags: ['选举'], originator: 'root-other', createdAt: 3, closed: false, following: false, publicity: null, exec: null }
  ];

  it('状态筛选：进行中/已关闭', () => {
    expect(filterAffairs(items, { ...EMPTY_FILTERS, status: ['open'], relation: [], tags: [] }, 'root-me').map((i) => i.affairId)).toEqual(['a', 'c']);
    expect(filterAffairs(items, { ...EMPTY_FILTERS, status: ['closed'], relation: [], tags: [] }, 'root-me').map((i) => i.affairId)).toEqual(['b']);
  });

  it('关系筛选：我发起/我关注（同维度 OR）', () => {
    expect(filterAffairs(items, { ...EMPTY_FILTERS, status: [], relation: ['mine'], tags: [] }, 'root-me').map((i) => i.affairId)).toEqual(['a']);
    expect(filterAffairs(items, { ...EMPTY_FILTERS, status: [], relation: ['following'], tags: [] }, 'root-me').map((i) => i.affairId)).toEqual(['a', 'b']);
    expect(filterAffairs(items, { ...EMPTY_FILTERS, status: [], relation: ['mine', 'following'], tags: [] }, 'root-me').map((i) => i.affairId)).toEqual(['a', 'b']);
  });

  it('跨维度 AND：状态 × 标签', () => {
    expect(
      filterAffairs(items, { status: ['open'], relation: [], tags: ['选举'] }, 'root-me').map((i) => i.affairId)
    ).toEqual(['c']);
  });

  it('标签维度内 OR', () => {
    expect(
      filterAffairs(items, { status: [], relation: [], tags: ['预算', '选举'] }, 'root-me').map((i) => i.affairId)
    ).toEqual(['a', 'b', 'c']);
  });

  it('collectAffairTags 去重', () => {
    expect(collectAffairTags(items)).toEqual(['预算', '选举']);
  });
});
