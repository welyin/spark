/**
 * AppIcon 组件的共享类型与纯逻辑（plugin-dist §2.3；从 .vue 抽出以便单测直接引用——
 * .vue SFC 的具名导出不进类型系统）。
 */
import type { PluginSpaceType } from '../../api/types';

/** AppIcon 消费的最小条目形状（PluginMarketItemDto 的结构子集；mock/合成条目同形） */
export type AppIconItem = {
  id: string;
  name: string;
  /** 声明图标（plugin-dist §2.1 icon：data: ≤20KB 或 https URL；空/缺省 = 无） */
  icon?: string;
  /** 是否已安装（缺省 = 未知，按未安装处理：包内图标不可用，fail-closed） */
  installed?: boolean;
  /** 内核全局 enabled（个人空间过渡种子，见 app-enablement） */
  enabled?: boolean;
  supportedSpaces?: PluginSpaceType[];
};

/** 包内 icon 路径净化（宽进严出：只接受包内相对路径——前导 `./`、`/` 折叠
 *  （与 plugin_src.rs 段校验同口径），拒绝上级逃逸 `..`、盘符与 scheme） */
export function sanitizePackageIconPath(raw: string | undefined): string {
  if (!raw) {
    return '';
  }
  const trimmed = raw.trim().replace(/\\/g, '/').replace(/^\.\/+/, '').replace(/^\/+/, '');
  if (!trimmed || /^[a-zA-Z]:/.test(trimmed) || trimmed.includes(':')) {
    return '';
  }
  const segments = trimmed.split('/');
  if (segments.some((segment) => segment === '' || segment === '.' || segment === '..')) {
    return '';
  }
  return segments.map((segment) => encodeURIComponent(segment)).join('/');
}
