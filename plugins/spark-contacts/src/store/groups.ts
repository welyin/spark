/**
 * 分组（个人空间扁平一层；组织空间树形，仅管理员改结构、任何人同级拖拽排序）。
 * 依赖 store（contactsOf/contactsApi）与 queries（profileOf）。
 */
import type { ContactGroupDef, ContactProfile, OrgGroupNode, SpaceContacts } from './types';
import { contactsApi, contactsOf } from './store';
import { profileOf } from './queries';

/** 把空间内所有资料中指向 groupId 的分组归属重置为未分组 */
const resetGroupMembership = (space: SpaceContacts, groupIds: string[]): void => {
  const strip = (profile: ContactProfile) => {
    if (groupIds.includes(profile.groupId)) {
      profile.groupId = '';
    }
  };
  space.friends.forEach(strip);
  Object.values(space.memberExtras).forEach(strip);
};

/** 设置联系人所属分组（'' = 未分组） */
export function setContactGroup(spaceKey: string, rootId: string, groupId: string): void {
  profileOf(spaceKey, rootId).groupId = groupId;
  contactsApi()
    ?.setGroup(spaceKey, rootId, groupId)
    .catch(() => {});
}

// ---- 个人空间：扁平分组 ----

/** 分组 id 计数器：Date.now 同毫秒 + 同名长度会撞 id，必须加自增后缀 */
let groupIdSeq = 0;

export function createGroup(spaceKey: string, name: string): ContactGroupDef {
  const space = contactsOf(spaceKey);
  groupIdSeq += 1;
  const group: ContactGroupDef = { id: `group-${Date.now()}-${groupIdSeq}`, name };
  space.groups.push(group);
  contactsApi()
    ?.groupCreate(spaceKey, group.id, name)
    .catch(() => {});
  return group;
}

export function renameGroup(spaceKey: string, groupId: string, name: string): void {
  const group = contactsOf(spaceKey).groups.find((item) => item.id === groupId);
  if (group) {
    group.name = name;
    contactsApi()
      ?.groupRename(spaceKey, groupId, name)
      .catch(() => {});
  }
}

/** 删除分组，组内联系人回归未分组 */
export function deleteGroup(spaceKey: string, groupId: string): void {
  const space = contactsOf(spaceKey);
  space.groups = space.groups.filter((item) => item.id !== groupId);
  resetGroupMembership(space, [groupId]);
  contactsApi()
    ?.groupDelete(spaceKey, groupId)
    .catch(() => {});
}

/** 拖拽重排：把分组移动到目标下标 */
export function moveGroup(spaceKey: string, groupId: string, toIndex: number): void {
  const groups = contactsOf(spaceKey).groups;
  const from = groups.findIndex((item) => item.id === groupId);
  if (from === -1) {
    return;
  }
  // toIndex 以拖拽前原序为准（可等于 length 表示移到末尾）；源在目标位之前时，
  // 摘除后目标索引前移一位——否则「拖到 C 前」会落到 C 后，与落点预测不符
  const target = Math.max(0, Math.min(toIndex, groups.length));
  const [moved] = groups.splice(from, 1);
  groups.splice(from < target ? target - 1 : target, 0, moved);
  contactsApi()
    ?.groupMove(spaceKey, groupId, toIndex)
    .catch(() => {});
}

// ---- 组织空间：分组树 ----

/** 在树中查找节点及其所在数组（父级 children 或根数组） */
function findOrgNode(
  tree: OrgGroupNode[],
  id: string
): { node: OrgGroupNode; siblings: OrgGroupNode[] } | null {
  for (const node of tree) {
    if (node.id === id) {
      return { node, siblings: tree };
    }
    const found = findOrgNode(node.children, id);
    if (found) {
      return found;
    }
  }
  return null;
}

/** parentId 为 '' 时挂到根层 */
export function createOrgGroup(spaceKey: string, parentId: string, name: string): OrgGroupNode | null {
  const space = contactsOf(spaceKey);
  groupIdSeq += 1;
  const node: OrgGroupNode = { id: `og-${Date.now()}-${groupIdSeq}`, name, children: [] };
  if (!parentId) {
    space.groupTree.push(node);
    contactsApi()
      ?.orgGroupCreate(spaceKey, parentId, node.id, name)
      .catch(() => {});
    return node;
  }
  const parent = findOrgNode(space.groupTree, parentId);
  if (!parent) {
    return null;
  }
  parent.node.children.push(node);
  contactsApi()
    ?.orgGroupCreate(spaceKey, parentId, node.id, name)
    .catch(() => {});
  return node;
}

export function renameOrgGroup(spaceKey: string, id: string, name: string): void {
  const found = findOrgNode(contactsOf(spaceKey).groupTree, id);
  if (found) {
    found.node.name = name;
    contactsApi()
      ?.orgGroupRename(spaceKey, id, name)
      .catch(() => {});
  }
}

/** 收集节点子树的全部 id（含自身） */
const collectOrgIds = (node: OrgGroupNode): string[] => [
  node.id,
  ...node.children.flatMap(collectOrgIds)
];

/** 删除节点：子节点提升到被删节点所在层，子树涉及的联系人均回归未分组 */
export function deleteOrgGroup(spaceKey: string, id: string): void {
  const space = contactsOf(spaceKey);
  const found = findOrgNode(space.groupTree, id);
  if (!found) {
    return;
  }
  const index = found.siblings.findIndex((item) => item.id === id);
  found.siblings.splice(index, 1, ...found.node.children);
  resetGroupMembership(space, collectOrgIds(found.node));
  contactsApi()
    ?.orgGroupDelete(spaceKey, id)
    .catch(() => {});
}

/** 同级拖拽重排：只在节点当前所在层内移动，不改变树结构 */
export function moveOrgGroupSibling(spaceKey: string, id: string, toIndex: number): void {
  const found = findOrgNode(contactsOf(spaceKey).groupTree, id);
  if (!found) {
    return;
  }
  const from = found.siblings.findIndex((item) => item.id === id);
  // 同 moveGroup：toIndex 以原序为准（可等于 length），源在目标位之前时摘除后前移一位
  const target = Math.max(0, Math.min(toIndex, found.siblings.length));
  const [moved] = found.siblings.splice(from, 1);
  found.siblings.splice(from < target ? target - 1 : target, 0, moved);
  contactsApi()
    ?.orgGroupMove(spaceKey, id, toIndex)
    .catch(() => {});
}

/**
 * 跨级拖拽移动（仅管理员）：把节点移动到新父级（'' = 根层）下的指定位置。
 * 禁止移入自己或自己的子树（成环），命中时返回 false 不做任何改动。
 * 桥接把 newParentId 一并上传（内核 contact_org_group_move 的 Some(parent)，
 * '' = 根层）；内核侧同口径防环兜底，目标父不存在/成环时静默忽略。
 */
export function moveOrgGroup(spaceKey: string, id: string, newParentId: string, toIndex: number): boolean {
  const space = contactsOf(spaceKey);
  const found = findOrgNode(space.groupTree, id);
  if (!found) {
    return false;
  }
  if (newParentId && collectOrgIds(found.node).includes(newParentId)) {
    return false;
  }
  const targetSiblings = newParentId
    ? findOrgNode(space.groupTree, newParentId)?.node.children
    : space.groupTree;
  if (!targetSiblings) {
    return false;
  }
  const from = found.siblings.findIndex((item) => item.id === id);
  const [moved] = found.siblings.splice(from, 1);
  // 同层移动且原位置在目标位之前时，摘除后目标索引前移一位
  let index = toIndex;
  if (targetSiblings === found.siblings && from < toIndex) {
    index -= 1;
  }
  const clamped = Math.max(0, Math.min(index, targetSiblings.length));
  targetSiblings.splice(clamped, 0, moved);
  contactsApi()
    // 上传原序裸值 toIndex：同层前移调整由内核统一做（与 moveGroup/
    // moveOrgGroupSibling 同口径），上传调整后的值会被二次前移
    ?.orgGroupMove(spaceKey, id, toIndex, newParentId)
    .catch(() => {});
  return true;
}
