/**
 * 壳层运行环境信息（A57，桥握手 ctx 注入 appVersion / platform / shellVersion）。
 *
 * 口径：
 * - 三字段一律由壳层在本模块生成，随握手 ctx 经 createBridgeHost 下发——
 *   插件只读、不可自报伪造（hello 只携 sdkVersion/pluginId/viewId 做一致性
 *   核对，无环境信息上报通道）；
 * - 低敏环境信息（版本号 + 平台大类），免权限：不占权限位、不进
 *   bridge-dispatcher 的 CALL_PERMISSIONS 表；
 * - 向后兼容：SDK 侧三字段均为可选（PluginContext 注释），旧壳层不注入时
 *   插件读取得到 undefined，须按缺省降级而非视为必填。
 *
 * 用途：问题反馈等插件采集运行环境信息（注入前 MVP 用户手填）。
 */

import { version as APP_VERSION } from '../../package.json';
import type { PluginContext, PluginPlatform } from '../../../packages/plugin-sdk/src';

/** 平台判定：userAgent 口径（与 plugin/source.ts 同源先例，无 OS 探测依赖） */
export function resolvePluginPlatform(userAgent: string): PluginPlatform {
  if (userAgent.includes('Windows')) {
    return 'windows';
  }
  if (userAgent.includes('Android')) {
    return 'android';
  }
  if (/iPhone|iPad|iPod/.test(userAgent)) {
    return 'ios';
  }
  if (userAgent.includes('Macintosh') || userAgent.includes('Mac OS X')) {
    return 'macos';
  }
  if (userAgent.includes('Linux')) {
    return 'linux';
  }
  return 'unknown';
}

/**
 * 壳层环境信息快照（每次握手现取：版本随构建产物，platform 随当前 UA）。
 * shellVersion 由调用方传入握手同值 sdkVersion（manifest?.sdkVersion ?? '1'），
 * 保证 ctx.shellVersion ≡ ready.sdkVersion——契约版本单源在握手侧，此处不再
 * 独立硬编码（A57 评审问题 1：双源字面量无编译期约束，契约演进时会漂移）。
 */
export function resolvePluginShellEnv(sdkVersion: string): Pick<PluginContext, 'appVersion' | 'platform' | 'shellVersion'> {
  return {
    appVersion: APP_VERSION,
    platform: resolvePluginPlatform(typeof navigator !== 'undefined' ? navigator.userAgent : ''),
    shellVersion: sdkVersion
  };
}

/** 握手 ctx 注入环境信息：返回新对象，不改入参（ctx 下发后插件侧只读） */
export function withPluginShellEnv(ctx: PluginContext, sdkVersion: string): PluginContext {
  return { ...ctx, ...resolvePluginShellEnv(sdkVersion) };
}
