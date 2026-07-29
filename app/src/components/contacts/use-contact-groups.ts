/**
 * 通讯录分组栏（自 ContactsPage 拆出以控制单文件行数）：
 * 第二栏分组列表 -> 第三栏组内成员（个人扁平 / 组织树），含各分组人数、
 * 当前分组名/成员、管理员虚拟行与分组被删后的兜底回退。
 */
import { computed, watch, type ComputedRef, type Ref } from 'vue';
import { profileOf, requestBadgeCount, type OrgGroupNode, type SpaceContacts } from '../../mock/contacts';
import { compareNames } from '../../utils/pinyin';
import type { ContactItem, GroupOption, RightView } from './types';

export interface ContactGroupsContext {
  isPersonal: ComputedRef<boolean>;
  spaceKey: ComputedRef<string>;
  spaceData: ComputedRef<SpaceContacts>;
  contacts: ComputedRef<ContactItem[]>;
  /** 当前选中分组：'ungrouped'=未分组（虚拟组），其余为分组 id（拉黑者不进通讯录，黑名单在个人设置管理） */
  activeGroupId: Ref<string>;
  rightView: Ref<RightView>;
}

export function useContactGroups(ctx: ContactGroupsContext) {
  /** 组织树扁平化（带深度），供分组选项与组名查找复用 */
  const flattenOrgTree = (nodes: OrgGroupNode[], depth = 0): Array<{ id: string; name: string; depth: number }> =>
    nodes.flatMap((node) => [{ id: node.id, name: node.name, depth }, ...flattenOrgTree(node.children, depth + 1)]);

  /** 资料卡「分组」下拉选项：'' = 未分组；组织按树深度缩进 */
  const groupOptions = computed<GroupOption[]>(() => {
    const options: GroupOption[] = [{ id: '', label: '未分组' }];
    if (ctx.isPersonal.value) {
      return options.concat(ctx.spaceData.value.groups.map((group) => ({ id: group.id, label: group.name })));
    }
    return options.concat(
      flattenOrgTree(ctx.spaceData.value.groupTree).map((node) => ({
        id: node.id,
        label: `${'　'.repeat(node.depth)}${node.name}`
      }))
    );
  });

  /** 各分组人数（含虚拟组 ungrouped），第二栏分组行右侧灰字 */
  const groupCounts = computed<Record<string, number>>(() => {
    const counts: Record<string, number> = { ungrouped: 0 };
    for (const contact of ctx.contacts.value) {
      const groupId = profileOf(ctx.spaceKey.value, contact.rootId).groupId;
      if (groupId) {
        counts[groupId] = (counts[groupId] ?? 0) + 1;
      } else {
        counts.ungrouped += 1;
      }
    }
    return counts;
  });

  /** 当前分组名（第三栏标题） */
  const activeGroupName = computed(() => {
    if (ctx.activeGroupId.value === 'admins') {
      return '管理员';
    }
    if (ctx.activeGroupId.value === 'ungrouped') {
      return '未分组';
    }
    const found = groupOptions.value.find((option) => option.id === ctx.activeGroupId.value);
    return found ? found.label.trim() : '未分组';
  });

  /** 当前分组的成员（第三栏列表），按名称排序；'ungrouped' 即 groupId === ''；'admins'=全部管理员（组织空间） */
  const groupMembers = computed<ContactItem[]>(() => {
    if (ctx.activeGroupId.value === 'admins') {
      return ctx.contacts.value
        .filter((contact) => contact.role === 'admin')
        .sort((a, b) => compareNames(a.displayName, b.displayName));
    }
    const targetId = ctx.activeGroupId.value === 'ungrouped' ? '' : ctx.activeGroupId.value;
    return ctx.contacts.value
      .filter((contact) => profileOf(ctx.spaceKey.value, contact.rootId).groupId === targetId)
      .sort((a, b) => compareNames(a.displayName, b.displayName));
  });

  /** 管理员人数（组织空间第二栏「管理员」行）：与第三栏「管理员」列表同口径
      （拉黑者不进通讯录，也不计入） */
  const adminCount = computed(() => ctx.contacts.value.filter((contact) => contact.role === 'admin').length);

  /** 选中分组行：切回联系人态 */
  const onSelectGroup = (groupId: string) => {
    ctx.activeGroupId.value = groupId;
    ctx.rightView.value = 'contact';
  };

  // 兜底：当前分组（或其父节点）被删除后，落回「未分组」（'admins' 为恒在虚拟行，不参与校验）
  watch(groupOptions, (options) => {
    if (
      ctx.activeGroupId.value !== 'ungrouped' &&
      ctx.activeGroupId.value !== 'admins' &&
      !options.some((option) => option.id === ctx.activeGroupId.value)
    ) {
      ctx.activeGroupId.value = 'ungrouped';
    }
  });

  const pendingCount = computed(() => requestBadgeCount(ctx.spaceKey.value));

  /** 第二栏统一列表选中：功能行（新的朋友/标签）切右栏面板，分组行切分组 */
  const onSelectRow = (id: string) => {
    if (id === 'new-friends' || id === 'tags') {
      ctx.rightView.value = id;
      return;
    }
    onSelectGroup(id);
  };

  return {
    groupOptions,
    groupCounts,
    activeGroupName,
    groupMembers,
    adminCount,
    pendingCount,
    onSelectGroup,
    onSelectRow
  };
}
