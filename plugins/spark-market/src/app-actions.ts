/**
 * 市场插件共享动作（移植自壳层 components/apps/app-actions.ts 的系统层子集）。
 *
 * 口径（install-and-enable §一）：安装＝系统层本机动作，无空间守卫。
 * 插件内不含空间层启停（per-space 启用事实源是壳层 localStorage 存储，
 * 桥面无此数据面——见任务报告遗留）；个人空间内核全局开关仍可由
 * sdk.market.setEnabled 触达（详情页「内核全局开关」不暴露给插件 UI，
 * 启停语义归壳层空间市场，本插件只做装卸/更新）。
 */
import { ElMessage, ElMessageBox } from 'element-plus';
import type { PluginMarketItem, PluginMarketRepoDeclaration } from '../../../packages/plugin-sdk/src';
import { marketApi } from './sdk-host';
import { permissionLabel } from './market-store';

/** 权限确认对话框（设计 §6.1：安装即授权）；用户取消返回 false */
export async function confirmPermissions(name: string, permissions: string[]): Promise<boolean> {
  if (permissions.length === 0) {
    return true;
  }
  const labels = permissions.map((permission) => `${permissionLabel(permission)}（${permission}）`).join('、');
  try {
    await ElMessageBox.confirm(
      `该应用声明以下权限：${labels}。安装即视为授权，运行时可越权调用将被系统拦截。`,
      `授权安装 ${name}`,
      { confirmButtonText: '授权并安装', cancelButtonText: '取消', type: 'warning' }
    );
    return true;
  } catch {
    return false; // 用户取消授权
  }
}

/** 安装市场条目到本机（权限确认 → installFromRepo）；返回是否安装成功 */
export async function installPluginItem(item: PluginMarketItem): Promise<boolean> {
  if (!(await confirmPermissions(item.name, item.permissions))) {
    return false;
  }
  const market = marketApi();
  if (!market) {
    ElMessage.error('市场接口不可用（非插件运行上下文）');
    return false;
  }
  try {
    // 目录驱动 install 已退役：市场项 id 即仓库规范化地址（installFromRepo）
    await market.installFromRepo(item.id);
    ElMessage.success('应用安装成功，启用后即可使用');
    return true;
  } catch (error) {
    ElMessage.error(`应用安装失败：${describeInstallError(error)}`);
    return false;
  }
}

/** 仓库锚定安装（声明文件已在前置解析中展示）：权限确认 → installFromRepo */
export async function installRepoDeclaration(declaration: PluginMarketRepoDeclaration): Promise<boolean> {
  if (!(await confirmPermissions(declaration.name, declaration.permissions))) {
    return false;
  }
  const market = marketApi();
  if (!market) {
    ElMessage.error('市场接口不可用（非插件运行上下文）');
    return false;
  }
  try {
    await market.installFromRepo(declaration.id);
    ElMessage.success('应用安装成功，启用后即可使用');
    return true;
  } catch (error) {
    ElMessage.error(`应用安装失败：${describeInstallError(error)}`);
    return false;
  }
}

/** 网络差降级（plugin_system.md「市场展示与排序」）：仓库不可达时提示手动
 *  侧载路径；判定走结构化前缀（plugin-dist §6 错误串统一 "Repo plugin ... fetch failed" 形态）。
 *  注意桥通路错误是 Error 实例（message 含内核原始串），须取 message 再判前缀——
 *  壳层直连 Tauri invoke 拒绝值是裸字符串，`${error}` 直判即可，桥面不行 */
function describeInstallError(error: unknown): string {
  const message = error instanceof Error ? error.message : `${error}`;
  const unreachable = message.startsWith('Repo plugin') && message.includes('fetch failed');
  return unreachable
    ? `仓库不可达，可自行下载 .spkg 后用「导入 .spkg 文件」侧载安装（${message}）`
    : message;
}
