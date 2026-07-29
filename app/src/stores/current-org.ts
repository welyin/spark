/**
 * 当前组织 ID（兼容导出）——从 currentSpace 派生：
 * 组织空间时为该组织 orgId，个人空间时为空串。
 *
 * 历史背景：原为模块级可写 ref，由 OrgPage 写入、NetworkStatusBar 读取。
 * 空间模型落地后（ui-space-navbar §10），组织选择收归 currentSpace，
 * 本导出保留原形状供 NetworkStatusBar 等既有读取方使用（只读）。
 */
import { computed } from 'vue';
import { currentSpaceOrgId } from './current-space';

export const currentOrgId = computed<string>(() => currentSpaceOrgId.value);
