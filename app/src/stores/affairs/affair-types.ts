/**
 * 事务类型注册表（阶段 3 / ui-architecture §4.4 affair-types，§八决策 2「宿主自建」）。
 *
 * 宿主扫描已装插件 manifest 的 affairTypes 字段，本地建 Map<affairType, pluginId>，
 * 内核零改动、零下发。manifest 无 affairTypes 字段即不承接（向后兼容）。
 *
 * 用途：事务列表点卡片时，按事务的类型标识找到能处理它的类型插件（「默认打开程序」，
 * README §4.4）。本机没装能处理该类型的插件时给市场引导空状态。
 */
import { ref } from 'vue';
import { fetchPluginManifest } from '../../plugin/source';

/** 类型注册表：affairType → pluginId（响应式，rebuildAffairTypes 重建） */
export const affairTypeRegistry = ref<Map<string, string>>(new Map());

/** 已装插件 id 列表（扫描来源：pluginMarket.list 的 installed 项） */
async function listInstalledPluginIds(): Promise<string[]> {
  const api = window.electronAPI?.pluginMarket;
  if (!api) {
    return [];
  }
  const items = await api.list();
  return items.filter((item) => item.installed).map((item) => item.id);
}

/**
 * 重建类型注册表：扫描全部已装插件 manifest，收集 affairTypes 声明。
 * 同类型被多个插件声明时后者覆盖前者（先到先得可后续再议，README §4.4 未强制）。
 */
export async function rebuildAffairTypes(): Promise<void> {
  const map = new Map<string, string>();
  try {
    const ids = await listInstalledPluginIds();
    const manifests = await Promise.all(ids.map((id) => fetchPluginManifest(id)));
    for (const manifest of manifests) {
      if (!manifest?.affairTypes) {
        continue;
      }
      for (const type of manifest.affairTypes) {
        if (typeof type === 'string' && type) {
          map.set(type, manifest.id);
        }
      }
    }
  } catch {
    // 扫描失败保留旧注册表
  }
  affairTypeRegistry.value = map;
}

/** 按事务类型查承接插件 id（未装/未声明为 null） */
export function pluginForAffairType(affairType: string): string | null {
  return affairTypeRegistry.value.get(affairType) ?? null;
}
