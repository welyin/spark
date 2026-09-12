/**
 * 应用安装与启停的共享动作（系统层应用管理 AppsPage 与空间应用市场 AppEnablePanel 同用）。
 *
 * 口径（install-and-enable §一 + problem.md 应用管理走查修正）：
 * - 安装＝系统层本机动作，无空间守卫（拿到代码 ≠ 任何空间启用）；
 * - 启用/停用＝空间层动作（per-space 事实源 app-enablement）；先启用后安装——
 *   启用未安装的应用会先走安装（含权限确认），装不上就不启用（§五③ 按需获取）；
 * - 组织空间启停过渡期仅本机生效（成员同步待内核「组织启用清单」），
 *   非管理员只读（正式链路＝组织事务决议，后续工作）。
 */
import { ElMessage, ElMessageBox } from 'element-plus';
import type { PluginMarketItemDto } from '../../api/types';
import { permissionLabel } from './apps-store';
import { isMockApp, setMockAppInstalled } from '../../mock/apps';
import { isDevPlugin } from '../../mock/dev-plugins';
import { spaceKeyOf } from '../../mock/space-key';
import { notifyPluginInstalled } from '../../plugin/messages';
import { currentSpace } from '../../stores/current-space';
import { isAdmin } from '../../stores/org-membership';
import {
  isAppEnableableInSpace,
  isAppEnabledInSpace,
  setAppEnabledInSpace,
  type EnablementSpace
} from '../../stores/app-enablement';

/** 当前空间的启用事实源引用（current-space → EnablementSpace） */
export function currentEnableSpace(): EnablementSpace {
  return currentSpace.value.type === 'org'
    ? { type: 'org', orgId: currentSpace.value.orgId }
    : { type: 'personal' };
}

/** 安装应用到本机（系统层动作）：权限确认（设计 §6.1）→ 安装 → 系统通知。
 *  返回是否安装成功（用户取消授权 / 安装失败均返回 false）。 */
export async function installPluginItem(item: PluginMarketItemDto): Promise<boolean> {
  if (item.permissions.length > 0) {
    const labels = item.permissions
      .map((permission) => `${permissionLabel(permission)}（${permission}）`)
      .join('、');
    try {
      await ElMessageBox.confirm(
        `该应用声明以下权限：${labels}。安装即视为授权，运行时可越权调用将被系统拦截。`,
        `授权安装 ${item.name}`,
        { confirmButtonText: '授权并安装', cancelButtonText: '取消', type: 'warning' }
      );
    } catch {
      return false; // 用户取消授权
    }
  }
  // mock 应用：权限确认流程保留，安装只写 localStorage 状态（见 src/mock/apps.ts）
  if (isMockApp(item)) {
    setMockAppInstalled(item.id, true);
    ElMessage.success('应用安装成功，启用后即可使用');
    notifyPluginInstalled(spaceKeyOf(currentSpace.value), item.name);
    return true;
  }
  try {
    // 目录驱动 install 已退役：市场项 id 即仓库规范化地址（installFromRepo）
    await window.electronAPI.pluginMarket.installFromRepo(item.id);
    ElMessage.success('应用安装成功，启用后即可使用');
    notifyPluginInstalled(spaceKeyOf(currentSpace.value), item.name);
    return true;
  } catch (error) {
    ElMessage.error(`应用安装失败：${error}`);
    return false;
  }
}

/** 启用/停用当前空间的应用（空间层动作，唯一操作面）：
 *  启用＝纯逻辑状态，不要求代码在场（install-and-enable §一 形式化定义，2026-09-10 拍板）；
 *  适用域守卫 → 组织非管理员只读 → 写 per-space 事实源；
 *  启用未安装项给诚实提示（首次打开时就地安装）；个人空间回写内核全局开关
 *  （pluginMarket.setEnabled，运行时要；未安装时无内核对象可写则跳过），失败回滚。 */
export async function toggleAppEnablement(item: PluginMarketItemDto): Promise<void> {
  const space = currentEnableSpace();
  if (!isAppEnableableInSpace(space, item)) {
    ElMessage.warning(
      currentSpace.value.type === 'personal'
        ? `「${item.name}」仅支持组织空间，请切换到组织后使用`
        : `「${item.name}」仅支持个人空间，请切换到个人空间后使用`
    );
    return;
  }
  const next = !isAppEnabledInSpace(space, item);
  if (space.type === 'org') {
    if (!isAdmin(space.orgId)) {
      ElMessage.info('组织空间的应用启用需走组织流程；过渡期内由管理员操作，且仅在本机生效');
      return;
    }
    setAppEnabledInSpace(space, item.id, next);
    // 过渡期诚实标注（install-and-enable §五：启用清单走组织副本同步，待内核接管）
    ElMessage.success(
      next
        ? `已在本机启用（仅本机生效，组织内同步待内核组织启用清单落地）${item.installed ? '' : '；该应用还没装到本机，首次打开时会提示安装'}`
        : '已在本机停用（仅本机生效）'
    );
    return;
  }
  // 个人空间：先写 per-space 事实源；非 mock / 非本地开发插件回写内核全局开关，失败回滚记录
  // （mock 应用与 dev 链路本地开发插件在内核无安装记录，setEnabled 无可写对象）
  setAppEnabledInSpace(space, item.id, next);
  if (isMockApp(item) || isDevPlugin(item.id)) {
    return;
  }
  if (next && !item.installed) {
    // 启用≠安装：代码在首次打开时就地获取（打开路径由插件宿主统一提示）
    ElMessage.success('已启用；该应用还没安装到本机，首次打开时会提示安装');
    return;
  }
  try {
    await window.electronAPI.pluginMarket.setEnabled(item.id, next);
  } catch (error) {
    setAppEnabledInSpace(space, item.id, !next);
    ElMessage.error(`应用启停失败：${error}`);
  }
}
