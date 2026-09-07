/**
 * 公共议题客户端（spark-affairs）· sdk.affairs 对接层。
 *
 * 已落地 SDK 面（community-affairs §7.2，plugin-sdk 类型 PluginAffairsAPI）：
 *   create(genesisInput) / follow(genesis) / unfollow(affairId) / listFollowed() /
 *   submitOp(op) / readLog(affairId) / readRules(affairId) / readResolution(affairId) /
 *   ladderStatus(affairId) / readExec(affairId) / orgEffects(orgId, affairId) /
 *   applyOrgEffects(orgId, affairId) / onChange(handler)
 * 权限：创建/关注/取关/提交操作/回执编排须 affairs:write，只读查询须 affairs:read
 * （五位全高级，须 manifest 声明并安装授权；桥 dispatcher 逐调用校验）。
 * onChange 走事件订阅通道（AffairChanged，载荷仅 affairId+变更类别）。
 *
 * 对接纪律：requireAffairsModule 对插件实际用到的每个方法做存在性校验——
 * 只做模块级存在性检查会把「接口错位」推迟到调用现场才以 unknown-call
 * 炸出（且可能被上层静默吞掉），此处 fail-fast 并点名缺失方法。
 */

import type { PluginAffairsAPI, PluginSDK } from '../../packages/plugin-sdk/src';

/** 本插件用到的 sdk.affairs 方法全集（对接核对清单） */
export const REQUIRED_AFFAIRS_METHODS = [
  'create',
  'follow',
  'unfollow',
  'listFollowed',
  'submitOp',
  'readLog',
  'readRules',
  'readResolution',
  'ladderStatus',
  'readExec',
  'orgEffects',
  'applyOrgEffects',
  'onChange'
] as const;

/** 宿主 sdk.affairs 缺失或接口错位时的统一错误（视图据此降级为提示页） */
export const AFFAIRS_MODULE_MISSING =
  '当前宿主未提供可用的 sdk.affairs（共同体事务 SDK 模块）。本插件是参考实现，需宿主实现 affairs 桥模块后可用。';

/** 宿主是否提供可用的 sdk.affairs（模块存在且本插件用到的方法齐全） */
export function hasAffairsModule(sdk: PluginSDK): boolean {
  const affairs = sdk.affairs as Partial<PluginAffairsAPI> | undefined;
  return Boolean(
    affairs && REQUIRED_AFFAIRS_METHODS.every((method) => typeof affairs[method] === 'function')
  );
}

/** 从 SDK 实例取 sdk.affairs；缺失或方法不齐时抛明确错误（点名缺失方法） */
export function requireAffairsModule(sdk: PluginSDK): PluginAffairsAPI {
  const affairs = sdk.affairs as Partial<PluginAffairsAPI> | undefined;
  if (!affairs) {
    throw new Error(AFFAIRS_MODULE_MISSING);
  }
  const missing = REQUIRED_AFFAIRS_METHODS.filter((method) => typeof affairs[method] !== 'function');
  if (missing.length > 0) {
    throw new Error(`sdk.affairs 接口错位：缺少方法 ${missing.join(' / ')}（与已落地 SDK 面不符）。`);
  }
  return affairs as PluginAffairsAPI;
}
