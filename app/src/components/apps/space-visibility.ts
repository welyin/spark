/**
 * 插件按空间过滤的纯逻辑（spaces-and-plugins §4 / plugin_system.md）。
 *
 * 口径：manifest 未声明 supportedSpaces（undefined 或空数组）按 ['org'] 处理，
 * 保持存量插件兼容——个人空间只展示支持 personal 的插件，
 * 组织空间不展示纯个人插件（supportedSpaces 仅 ['personal']）。
 */
import type { PluginSpaceType } from '../../api/types';

/** 缺省口径：未声明 supportedSpaces 按 ['org']（spaces-and-plugins §4） */
export const DEFAULT_SUPPORTED_SPACES: PluginSpaceType[] = ['org'];

/**
 * 插件在当前空间是否可见（仅 UI 展示过滤，不影响已装插件的会话/消息链路）。
 * supportedSpaces 缺省/空数组 → 仅 org 空间可见。
 */
export function isPluginVisibleInSpace(
  supportedSpaces: PluginSpaceType[] | undefined,
  spaceType: PluginSpaceType
): boolean {
  const spaces =
    supportedSpaces && supportedSpaces.length > 0 ? supportedSpaces : DEFAULT_SUPPORTED_SPACES;
  return spaces.includes(spaceType);
}
