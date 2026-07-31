/**
 * 市场「探索」分区纯逻辑（plugin_system.md「市场展示与排序」，阶段 C 波次 2b）。
 *
 * - 探索页只展示 verified === 'verified' 的全网广播条目（pending/failed 不进视图）；
 * - 浏览默认随机排序（每次「换一批」重新洗牌，无稳定位置即无可抢位置）；
 * - 搜索按名称/简介/id 过滤，搜索时不用随机序（updatedAt 降序精确直达）。
 */
import type { PluginAnnounceIndexEntryDto } from '../../api/types';

/** 只有核查通过（verified）的条目可进探索视图（plugin-dist §8.7）。 */
export function filterVerifiedAnnounces(
  entries: PluginAnnounceIndexEntryDto[]
): PluginAnnounceIndexEntryDto[] {
  return entries.filter((entry) => entry.verified === 'verified');
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
export function sortAnnouncesByUpdated(
  entries: PluginAnnounceIndexEntryDto[]
): PluginAnnounceIndexEntryDto[] {
  return [...entries].sort((a, b) => b.updatedAt - a.updatedAt);
}

/** 探索页搜索：按名称 / 简介 / 插件 id（仓库地址）过滤，大小写不敏感。 */
export function announceMatches(entry: PluginAnnounceIndexEntryDto, keyword: string): boolean {
  const kw = keyword.trim().toLowerCase();
  if (!kw) {
    return true;
  }
  return [entry.announce.name, entry.announce.summary, entry.announce.id].some((field) =>
    field.toLowerCase().includes(kw)
  );
}

/** 广播声明分类的展示名（目录粗分类映射，未知值原样展示）。 */
export function announceCategoryLabel(category: string): string {
  if (category === 'foundation') {
    return '基础';
  }
  if (category === 'business') {
    return '应用';
  }
  return category || '其他';
}
