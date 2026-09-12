/**
 * 事务收藏（M17 长按次级动作）：内核 affairs 薄壳暂无收藏接口，
 * 先本机持久化（localStorage，按 affairId 定位），列表卡片以星标呈现；
 * 待内核收藏能力就绪后迁移，UI 文案不承诺跨设备同步。
 */
import { ref } from 'vue';

const STORAGE_KEY = 'spark:affair-favorites';

function load(): Record<string, true> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Record<string, unknown>;
      const result: Record<string, true> = {};
      for (const [id, value] of Object.entries(parsed)) {
        if (value === true) result[id] = true;
      }
      return result;
    }
  } catch {
    // 本地存储不可读时按空表处理
  }
  return {};
}

const favorites = ref<Record<string, true>>(load());

function persist(): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(favorites.value));
  } catch {
    // 持久化失败不阻断标记
  }
}

/** 事务是否已收藏（响应式） */
export function isAffairFavorited(affairId: string): boolean {
  return favorites.value[affairId] === true;
}

/** 切换收藏态，返回切换后的状态（true=已收藏） */
export function toggleAffairFavorite(affairId: string): boolean {
  const next = { ...favorites.value };
  if (next[affairId]) {
    delete next[affairId];
  } else {
    next[affairId] = true;
  }
  favorites.value = next;
  persist();
  return next[affairId] === true;
}
