/**
 * 「创建 / 加入组织」对话框的打开状态（problem L10）：
 * 对话框宿主提升到 App 根（MembershipDialogs.vue），以顶级对话框＋遮罩弹出（同全局搜索），
 * 任何入口（rail「空间」＋菜单等）只需调用 openMembershipDialog。
 * 修复原缺陷：原实现把对话框挂在 RailSpaceList 内、经 v-for 模板 ref 转发打开，
 * ref 在 v-for 下拿到的是实例数组，转发静默失效，对话框打不开。
 */
import { ref } from 'vue';

export type MembershipDialogMode = 'create' | 'join';

/** 当前应打开的组织成员资格对话框（null=都不开；两框互斥单实例） */
export const membershipDialogMode = ref<MembershipDialogMode | null>(null);

export function openMembershipDialog(mode: MembershipDialogMode): void {
  membershipDialogMode.value = mode;
}

export function closeMembershipDialog(): void {
  membershipDialogMode.value = null;
}
