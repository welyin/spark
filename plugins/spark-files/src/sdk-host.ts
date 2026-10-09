/**
 * spark-files 宿主适配层：持有桥握手拿到的 SDK 与运行上下文，
 * 供组件树取用（与 spark-market src/sdk-host.ts 同构）。
 */
import type { PluginContentAPI, PluginContext, PluginDataAPI, PluginSDK, PluginSysAPI } from '../../../packages/plugin-sdk/src';

let _sdk: PluginSDK | null = null;
let _ctx: PluginContext | null = null;

/** 入口绑定运行上下文（index.ts 握手成功后调用） */
export function bindPluginRuntime(sdk: PluginSDK, ctx: PluginContext): void {
  _sdk = sdk;
  _ctx = ctx;
}

/** 声明式数据模块句柄（文件元数据集合；未绑定 SDK——vitest 纯前端模式——为 undefined） */
export function dataApi(): PluginDataAPI | undefined {
  return _sdk?.data;
}

/** 内容面 blob 模块句柄（文件本体；sdk.content） */
export function contentApi(): PluginContentAPI | undefined {
  return _sdk?.content;
}

/** 系统代理句柄（下载走 sdk.sys.saveFile 壳层代存——沙箱 iframe 无 allow-downloads） */
export function sysApi(): PluginSysAPI | undefined {
  return _sdk?.sys;
}

/** 当前绑定运行上下文（测试/预览未绑定时为 null） */
export function pluginContext(): PluginContext | null {
  return _ctx;
}
