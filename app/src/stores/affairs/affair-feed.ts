/**
 * 事务聚合数据源（阶段 3 / ui-architecture §4.4 affair-feed）。
 *
 * 跨全部域、「只聚合与我相关」的事务列表（README §4.4：要我表决/我发起/我关注）。
 * 数据源 = 内核 affairs 薄壳：listFollowed()（我持有副本/关注的事务 id）+ readLog 取创世元数据
 * （title/summary/tags/originator/createdAt）。公共议题发现不在此（属空间插件，README §八决策 2）。
 *
 * 「待我处理」计数（G7 角标语义）：内核暂无专用谓词，按 ui-architecture §七风险 4 用
 * 「持有副本且进行中（未关闭）」近似——读 readResolution 判 closed，进行中即视为「可能需要我处理」。
 * 待内核提供精确「要我操作」谓词后替换 countActionable 实现（语义见 README §4.4）。
 */
import { computed, ref } from 'vue';

/** 公示期信息（A7：倒计时一律以存证链锚定时刻为准，不用本机时钟冒充链上时间） */
export interface AffairPublicity {
  /** 本副本存证链锚定时刻（ms）；未锚定（unanchored）为 null */
  anchoredMs: number | null;
  /** 公示期时长（ms，决议声明） */
  pubPeriodMs: number;
}

/** 链下执行回报摘要（G8「决议 ≠ 已执行」：链上决议生效 ≠ 链下执行完成） */
export interface AffairExecSummary {
  /** 执行型决议总数 */
  total: number;
  /** 已有执行回报（returned/closed）的决议数 */
  reported: number;
}

/** 事务列表项（外壳卡片元数据；谱系/进度等由类型插件承载） */
export interface AffairFeedItem {
  affairId: string;
  title: string;
  summary: string;
  tags: string[];
  /** 发起人身份 id */
  originator: string;
  /** 创世声明的本地毫秒（仅展示；权威时间以存证链为准，README §4.4） */
  createdAt: number;
  /** 是否已关闭（存在生效/被否决决议） */
  closed: boolean;
  /** 我是否关注（持有副本） */
  following: boolean;
  /** 进行中的公示期（state=pending 的决议；无则 null） */
  publicity: AffairPublicity | null;
  /** 链下执行回报摘要（执行型事务；非执行型/读取失败为 null） */
  exec: AffairExecSummary | null;
}

/** 事务列表（响应式缓存；refreshAffairFeed 填充） */
export const affairFeed = ref<AffairFeedItem[]>([]);
export const affairFeedLoading = ref(false);
export const affairFeedError = ref('');

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object'
    ? (value as Record<string, unknown>)
    : null;
}

/** 从创世记录读元数据（与 spark-affairs model.readGenesisMeta 同口径，外壳内联以避免跨插件 import） */
function readGenesisMeta(genesis: unknown): {
  title: string;
  summary: string;
  tags: string[];
  originator: string;
  createdAt: number;
} | null {
  const record = asRecord(genesis);
  const initiator = asRecord(record?.initiator);
  const title = typeof record?.title === 'string' ? record.title : null;
  const originator =
    typeof initiator?.identity === 'string' ? initiator.identity : null;
  const createdAt =
    typeof record?.createdAt === 'number' ? record.createdAt : null;
  if (!record || title === null || originator === null || createdAt === null) {
    return null;
  }
  const tags = Array.isArray(record.tags)
    ? record.tags.filter((t): t is string => typeof t === 'string')
    : [];
  return {
    title,
    summary: typeof record.summary === 'string' ? record.summary : '',
    tags,
    originator,
    createdAt,
  };
}

/** 从决议集合推导公示期与关闭态（A7/G8：公示期取首条 state=pending 的决议） */
function readResolutionMeta(resolutions: unknown): {
  closed: boolean;
  publicity: AffairPublicity | null;
} {
  const list = (asRecord(resolutions)?.resolutions ?? []) as unknown[];
  let closed = false;
  let publicity: AffairPublicity | null = null;
  for (const r of list) {
    const rec = asRecord(r);
    const state = rec?.state;
    if (state === 'effective' || state === 'vetoed') {
      closed = true;
    }
    if (!publicity && state === 'pending') {
      publicity = {
        anchoredMs: typeof rec?.anchoredMs === 'number' ? rec.anchoredMs : null,
        pubPeriodMs: typeof rec?.pubPeriodMs === 'number' ? rec.pubPeriodMs : 0,
      };
    }
  }
  return { closed, publicity };
}

/** 链下执行回报摘要（G8）：readExec 的 states 按八态归类；非执行型事务（exec 为 null）返回 null */
async function readExecSummary(
  api: NonNullable<typeof window.electronAPI>['affairs'],
  affairId: string,
): Promise<AffairExecSummary | null> {
  try {
    const view = await api.readExec(affairId);
    if (!view || view.exec === null || !Array.isArray(view.states)) {
      return null;
    }
    const reported = view.states.filter(
      (s) => s.state === 'returned' || s.state === 'closed',
    ).length;
    return { total: view.states.length, reported };
  } catch {
    return null;
  }
}

/** 拉取跨域事务列表（与我相关 = 我持有副本/关注）。失败保留旧缓存并置错。 */
export async function refreshAffairFeed(): Promise<void> {
  const api = window.electronAPI?.affairs;
  if (!api) {
    affairFeedError.value = '事务接口不可用';
    return;
  }
  affairFeedLoading.value = true;
  try {
    const ids = await api.listFollowed();
    const items = await Promise.all(
      ids.map(async (affairId): Promise<AffairFeedItem | null> => {
        try {
          const [log, resolutions] = await Promise.all([
            api.readLog(affairId),
            api.readResolution(affairId),
          ]);
          const meta = readGenesisMeta(log.genesis);
          if (!meta) {
            // 创世未同步到位（复制未收敛）：跳过而非编造占位（与 spark-affairs 同纪律）
            return null;
          }
          const { closed, publicity } = readResolutionMeta(resolutions);
          // G8：仅已关闭（已有链上决议）的事务再读执行回报，减少无谓调用
          const exec = closed ? await readExecSummary(api, affairId) : null;
          return {
            affairId,
            ...meta,
            closed,
            following: log.followedAt !== null,
            publicity,
            exec,
          };
        } catch {
          return null;
        }
      }),
    );
    affairFeed.value = items.filter(
      (item): item is AffairFeedItem => item !== null,
    );
    affairFeedError.value = '';
  } catch (err) {
    affairFeedError.value = `加载事务失败：${err}`;
  } finally {
    affairFeedLoading.value = false;
  }
}

/** 「待我处理」计数（tab 角标 G7）：近似 = 进行中（未关闭）的我关注事务数。内核精确谓词就绪后替换。 */
export const actionableCount = computed(
  () =>
    affairFeed.value.filter((item) => !item.closed && item.following).length,
);

/** 按处理状态分组（README §4.4「等我操作」置顶高亮）：近似——进行中在前、已关闭在后 */
export const groupedFeed = computed(() => {
  const open = affairFeed.value.filter((item) => !item.closed);
  const closed = affairFeed.value.filter((item) => item.closed);
  return { open, closed };
});

// ------------------------------------------------------------------
// A6：可组合筛选（收成顶部 chips）。维度：状态 / 与我的关系 / 标签。
// 纯函数便于单测；页面只持有选中态。
// ------------------------------------------------------------------

/** 状态筛选键 */
export type AffairStatusFilter = 'open' | 'closed';
/** 关系筛选键 */
export type AffairRelationFilter = 'mine' | 'following';

export interface AffairFilterState {
  status: AffairStatusFilter[];
  relation: AffairRelationFilter[];
  tags: string[];
}

export const EMPTY_FILTERS: AffairFilterState = {
  status: [],
  relation: [],
  tags: [],
};

/** 列表内出现过的全部标签（去重，供标签 chip 行） */
export function collectAffairTags(items: AffairFeedItem[]): string[] {
  const set = new Set<string>();
  for (const item of items) {
    for (const tag of item.tags) {
      set.add(tag);
    }
  }
  return [...set];
}

/** 组合筛选：同维度内 OR、跨维度 AND；空维度不限制。myRootId 为空时「我发起」不匹配任何项 */
export function filterAffairs(
  items: AffairFeedItem[],
  filters: AffairFilterState,
  myRootId: string,
): AffairFeedItem[] {
  return items.filter((item) => {
    if (filters.status.length > 0) {
      const key: AffairStatusFilter = item.closed ? 'closed' : 'open';
      if (!filters.status.includes(key)) {
        return false;
      }
    }
    if (filters.relation.length > 0) {
      const hit =
        (filters.relation.includes('mine') &&
          myRootId !== '' &&
          item.originator === myRootId) ||
        (filters.relation.includes('following') && item.following);
      if (!hit) {
        return false;
      }
    }
    if (
      filters.tags.length > 0 &&
      !filters.tags.some((tag) => item.tags.includes(tag))
    ) {
      return false;
    }
    return true;
  });
}
