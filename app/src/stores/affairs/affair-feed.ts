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
}

/** 事务列表（响应式缓存；refreshAffairFeed 填充） */
export const affairFeed = ref<AffairFeedItem[]>([]);
export const affairFeedLoading = ref(false);
export const affairFeedError = ref('');

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' ? (value as Record<string, unknown>) : null;
}

/** 从创世记录读元数据（与 spark-affairs model.readGenesisMeta 同口径，外壳内联以避免跨插件 import） */
function readGenesisMeta(genesis: unknown): { title: string; summary: string; tags: string[]; originator: string; createdAt: number } | null {
  const record = asRecord(genesis);
  const initiator = asRecord(record?.initiator);
  const title = typeof record?.title === 'string' ? record.title : null;
  const originator = typeof initiator?.identity === 'string' ? initiator.identity : null;
  const createdAt = typeof record?.createdAt === 'number' ? record.createdAt : null;
  if (!record || title === null || originator === null || createdAt === null) {
    return null;
  }
  const tags = Array.isArray(record.tags) ? record.tags.filter((t): t is string => typeof t === 'string') : [];
  return {
    title,
    summary: typeof record.summary === 'string' ? record.summary : '',
    tags,
    originator,
    createdAt
  };
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
          const [log, resolutions] = await Promise.all([api.readLog(affairId), api.readResolution(affairId)]);
          const meta = readGenesisMeta(log.genesis);
          if (!meta) {
            // 创世未同步到位（复制未收敛）：跳过而非编造占位（与 spark-affairs 同纪律）
            return null;
          }
          const closed = (resolutions.resolutions ?? []).some((r) => {
            const state = asRecord(r)?.state;
            return state === 'effective' || state === 'vetoed';
          });
          return {
            affairId,
            ...meta,
            closed,
            following: log.followedAt !== null
          };
        } catch {
          return null;
        }
      })
    );
    affairFeed.value = items.filter((item): item is AffairFeedItem => item !== null);
    affairFeedError.value = '';
  } catch (err) {
    affairFeedError.value = `加载事务失败：${err}`;
  } finally {
    affairFeedLoading.value = false;
  }
}

/** 「待我处理」计数（tab 角标 G7）：近似 = 进行中（未关闭）的我关注事务数。内核精确谓词就绪后替换。 */
export const actionableCount = computed(
  () => affairFeed.value.filter((item) => !item.closed && item.following).length
);

/** 按处理状态分组（README §4.4「等我操作」置顶高亮）：近似——进行中在前、已关闭在后 */
export const groupedFeed = computed(() => {
  const open = affairFeed.value.filter((item) => !item.closed);
  const closed = affairFeed.value.filter((item) => item.closed);
  return { open, closed };
});
