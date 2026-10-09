/**
 * spark-org-admin 宿主适配层：持有桥握手拿到的 SDK 与运行上下文，
 * 供组件树取用（与 spark-market src/sdk-host.ts 同构）。
 *
 * 空间语义：org 空间（ctx.space.type==='org'）管理当前空间组织；
 * personal 空间只有创建/加入（A42 评审决议：listMine 与老面
 * runtime.listMineOrganizations 口径统一为 personal 拒绝，插件不调）。
 */
import type { PluginContext, PluginOrgAPI, PluginPolicyAPI, PluginSDK } from '../../../packages/plugin-sdk/src';

let _sdk: PluginSDK | null = null;
let _ctx: PluginContext | null = null;

/** 入口绑定运行上下文（index.ts 握手成功后调用） */
export function bindPluginRuntime(sdk: PluginSDK, ctx: PluginContext): void {
  _sdk = sdk;
  _ctx = ctx;
}

/** 组织管理模块句柄（未绑定 SDK——vitest 纯前端模式——为 undefined，组件按纯内存降级） */
export function orgApi(): PluginOrgAPI | undefined {
  return _sdk?.org;
}

/** 策略模块句柄（策略配置节复用既有 sdk.policy 桥面） */
export function policyApi(): PluginPolicyAPI | undefined {
  return _sdk?.policy;
}

/** 当前绑定运行上下文（测试/预览未绑定时为 null） */
export function pluginContext(): PluginContext | null {
  return _ctx;
}

/** 当前身份 rootId（名册「我」的高亮与操作判定；未绑定/未解锁为空串） */
export async function currentRootId(): Promise<string> {
  try {
    const root = await _sdk?.runtime.currentRoot();
    return root?.rootId ?? '';
  } catch {
    return '';
  }
}
