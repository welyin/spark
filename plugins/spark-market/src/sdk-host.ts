/**
 * spark-market 宿主适配层：持有桥握手拿到的 SDK 与运行上下文，
 * 供组件树取用（与 spark-chat src/sdk-host.ts 同构）。
 *
 * 市场操作是系统层本机动作（install-and-enable §一①），不随绑定空间
 * 变化；ctx.space 仅用于展示语义（如图标启用判定不参与——插件版图标
 * 回退链不含包内图标，见 components/AppIcon.vue 头注）。
 */
import type { PluginContext, PluginMarketAPI, PluginSDK } from '../../../packages/plugin-sdk/src';

let _sdk: PluginSDK | null = null;
let _ctx: PluginContext | null = null;

/** 入口绑定运行上下文（index.ts 握手成功后调用） */
export function bindPluginRuntime(sdk: PluginSDK, ctx: PluginContext): void {
  _sdk = sdk;
  _ctx = ctx;
}

/** 市场模块句柄（未绑定 SDK——vitest 纯前端模式——为 undefined，组件按纯内存降级） */
export function marketApi(): PluginMarketAPI | undefined {
  return _sdk?.market;
}

/** 当前绑定运行上下文（测试/预览未绑定时为 null） */
export function pluginContext(): PluginContext | null {
  return _ctx;
}

/** 当前身份 rootId（开发者页「我发布过的应用」过滤；未绑定/未解锁为空串） */
export async function currentRootId(): Promise<string> {
  try {
    const root = await _sdk?.runtime.currentRoot();
    return root?.rootId ?? '';
  } catch {
    return '';
  }
}
