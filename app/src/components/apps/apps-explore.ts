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

/**
 * 市场「开发者」标签页（plugin-dist §8）：本地索引中由我发布的广播条目
 * （publisher == 我的 rootId）。不做 verified 过滤——自己发布但尚未过核查的
 * 条目也要可见（带核查状态标签展示）；rootId 为空（未加载）时返回空清单。
 */
export function filterMyAnnounces(
  entries: PluginAnnounceIndexEntryDto[],
  rootId: string
): PluginAnnounceIndexEntryDto[] {
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
export function sortAnnouncesByUpdated(
  entries: PluginAnnounceIndexEntryDto[]
): PluginAnnounceIndexEntryDto[] {
  return [...entries].sort((a, b) => b.updatedAt - a.updatedAt);
}

/** 探索页搜索：按名称 / 简介 / 插件 id（仓库地址）过滤，大小写不敏感；
 *  名称/简介读校正后字段（corrected 缺席时回落 announce 自报值）。 */
export function announceMatches(entry: PluginAnnounceIndexEntryDto, keyword: string): boolean {
  const kw = keyword.trim().toLowerCase();
  if (!kw) {
    return true;
  }
  return [announceDisplayName(entry), announceDisplaySummary(entry), entry.announce.id].some((field) =>
    field.toLowerCase().includes(kw)
  );
}

/**
 * 展示字段口径（plugin-dist §8.8）：懒惰核查通过时内核已把仓库声明文件的
 * name/icon/summary/version 回写 corrected——列表一律以 corrected 为准，
 * announce 自报值仅在 corrected 缺席时作占位（索引是提示层，自报不可信）。
 */
export function announceDisplayName(entry: PluginAnnounceIndexEntryDto): string {
  return entry.corrected?.name || entry.announce.name;
}

/** 展示简介（同 announceDisplayName 口径） */
export function announceDisplaySummary(entry: PluginAnnounceIndexEntryDto): string {
  return entry.corrected?.summary || entry.announce.summary;
}

/** 展示版本（同 announceDisplayName 口径） */
export function announceDisplayVersion(entry: PluginAnnounceIndexEntryDto): string {
  return entry.corrected?.version || entry.announce.version;
}

/** icon 渲染白名单：仅 https URL 与 data:image/ 内联图，其余一律不渲染（防
 *  javascript:/畸形 scheme 进 img src；空串走首字母占位） */
export function safeAnnounceIcon(icon: string): string {
  return icon.startsWith('https://') || icon.startsWith('data:image/') ? icon : '';
}

/** 展示 icon：corrected 优先，过 safeAnnounceIcon 白名单 */
export function announceDisplayIcon(entry: PluginAnnounceIndexEntryDto): string {
  return safeAnnounceIcon(entry.corrected?.icon || entry.announce.icon);
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
