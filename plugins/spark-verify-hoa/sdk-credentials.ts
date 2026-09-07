/**
 * 业主资格验证示例（spark-verify-hoa）· sdk.credentials 对接层。
 *
 * 已落地 SDK 面（community-affairs §7.2，类型 PluginCredentialsAPI）：
 *   listHeld() / presentHolderProof(input) / queryVerifiers(orgId) /
 *   verify(credential) / queryRevocations(issuer)
 * 全部只读，权限位 credentials:read（高级，须 manifest 声明并安装授权，
 * 桥 dispatcher 逐调用校验）。**无签发接口**——内核凭证体系刻意不对插件
 * 开放签发，签发走验证插件的人机流程（见 service.issueCredential 的诚实
 * 口径与 README）。
 *
 * verify / queryRevocations 是内核验证链（credential §6 第 1–5 步结构化
 * 裁决 + 按 issuer 的注销快照）的 SDK 正式暴露——协议线形凭证的验证与
 * 注销查询必须走这里，不得用本地顶替（本插件签发的演示级线形不过此链，
 * 只能本地自查，如实标注，见 service.verifyCredential 头注）。
 *
 * 对接纪律同 spark-affairs：对用到的每个方法做存在性校验，fail-fast 点名
 * 缺失方法——模块级存在性检查会把接口错位推迟到调用现场才以 unknown-call
 * 炸出，且曾被上层按「权限降级」静默吞掉（评审阻塞 2），此处杜绝。
 */

import type { PluginCredentialsAPI, PluginSDK } from '../../packages/plugin-sdk/src';

/** 本插件用到的 sdk.credentials 方法全集（对接核对清单） */
export const REQUIRED_CREDENTIALS_METHODS = [
  'listHeld',
  'presentHolderProof',
  'queryVerifiers',
  'verify',
  'queryRevocations'
] as const;

/** 宿主 sdk.credentials 缺失或接口错位时的统一错误 */
export const CREDENTIALS_MODULE_MISSING =
  '当前宿主未提供可用的 sdk.credentials（资格凭证 SDK 模块）。持有凭证查询/出示/验证人信任声明不可用。';

/** 宿主是否提供可用的 sdk.credentials（模块存在且本插件用到的方法齐全） */
export function hasCredentialsModule(sdk: PluginSDK): boolean {
  const credentials = sdk.credentials as Partial<PluginCredentialsAPI> | undefined;
  return Boolean(
    credentials && REQUIRED_CREDENTIALS_METHODS.every((method) => typeof credentials[method] === 'function')
  );
}

/** 从 SDK 实例取 sdk.credentials；缺失或方法不齐时抛明确错误（点名缺失方法） */
export function requireCredentialsModule(sdk: PluginSDK): PluginCredentialsAPI {
  const credentials = sdk.credentials as Partial<PluginCredentialsAPI> | undefined;
  if (!credentials) {
    throw new Error(CREDENTIALS_MODULE_MISSING);
  }
  const missing = REQUIRED_CREDENTIALS_METHODS.filter((method) => typeof credentials[method] !== 'function');
  if (missing.length > 0) {
    throw new Error(`sdk.credentials 接口错位：缺少方法 ${missing.join(' / ')}（与已落地 SDK 面不符）。`);
  }
  return credentials as PluginCredentialsAPI;
}
