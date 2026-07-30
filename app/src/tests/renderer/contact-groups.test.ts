// 分组 mock store 单测：个人扁平分组 CRUD/重排、组织分组树结构操作/同级重排、成员归属
import { describe, expect, it, vi } from 'vitest';
import {
  contactsOf,
  createGroup,
  createOrgGroup,
  deleteGroup,
  deleteOrgGroup,
  moveGroup,
  moveOrgGroup,
  moveOrgGroupSibling,
  profileOf,
  renameGroup,
  renameOrgGroup,
  setContactGroup
} from '../../mock/contacts';

const PERSONAL = 'personal';
const ORG = 'org:test-groups';

describe('个人空间扁平分组', () => {
  it('新建/重命名/重排/删除，删除后组内联系人回归未分组', () => {
    const space = contactsOf(PERSONAL);
    const a = createGroup(PERSONAL, '测试组A');
    const b = createGroup(PERSONAL, '测试组B');
    expect(space.groups.map((g) => g.id)).toEqual(expect.arrayContaining([a.id, b.id]));

    renameGroup(PERSONAL, a.id, '测试组A2');
    expect(space.groups.find((g) => g.id === a.id)?.name).toBe('测试组A2');

    // 把 b 移到 a 前面
    const indexA = space.groups.findIndex((g) => g.id === a.id);
    moveGroup(PERSONAL, b.id, indexA);
    const ids = space.groups.map((g) => g.id);
    expect(ids.indexOf(b.id)).toBeLessThan(ids.indexOf(a.id));

    // 成员归属：删除分组后 groupId 复位
    const friend = space.friends[0];
    setContactGroup(PERSONAL, friend.rootId, a.id);
    expect(profileOf(PERSONAL, friend.rootId).groupId).toBe(a.id);
    deleteGroup(PERSONAL, a.id);
    expect(space.groups.some((g) => g.id === a.id)).toBe(false);
    expect(profileOf(PERSONAL, friend.rootId).groupId).toBe('');

    deleteGroup(PERSONAL, b.id);
  });
});

describe('组织空间分组树', () => {
  it('新建根/子节点、同级重排、删除时子节点上移且成员复位', () => {
    const space = contactsOf(ORG);
    const seedRootCount = space.groupTree.length;

    const root = createOrgGroup(ORG, '', '测试根节点');
    expect(root).not.toBeNull();
    expect(space.groupTree.length).toBe(seedRootCount + 1);

    const child1 = createOrgGroup(ORG, root!.id, '子节点1');
    const child2 = createOrgGroup(ORG, root!.id, '子节点2');
    expect(root!.children.map((n) => n.id)).toEqual([child1!.id, child2!.id]);

    renameOrgGroup(ORG, child1!.id, '子节点1改');
    expect(root!.children[0].name).toBe('子节点1改');

    // 同级重排：child2 移到 child1 前，树结构（父子关系）不变
    moveOrgGroupSibling(ORG, child2!.id, 0);
    expect(root!.children.map((n) => n.id)).toEqual([child2!.id, child1!.id]);

    // 删除根节点：子节点提升到根层，子树成员回归未分组
    setContactGroup(ORG, 'member-root-1', child1!.id);
    expect(profileOf(ORG, 'member-root-1').groupId).toBe(child1!.id);
    deleteOrgGroup(ORG, root!.id);
    expect(space.groupTree.some((n) => n.id === root!.id)).toBe(false);
    expect(space.groupTree.map((n) => n.id)).toEqual(expect.arrayContaining([child1!.id, child2!.id]));
    expect(profileOf(ORG, 'member-root-1').groupId).toBe('');

    deleteOrgGroup(ORG, child1!.id);
    deleteOrgGroup(ORG, child2!.id);
  });

  it('跨级移动（moveOrgGroup）：换父级/移回根层，禁止移入自己的子树', () => {
    const space = contactsOf(ORG);

    const rootA = createOrgGroup(ORG, '', '跨级根A');
    const rootB = createOrgGroup(ORG, '', '跨级根B');
    const child = createOrgGroup(ORG, rootA!.id, '跨级子');
    const grandchild = createOrgGroup(ORG, child!.id, '跨级孙');

    // 跨级：child（带子孙）从 rootA 移到 rootB 下
    expect(moveOrgGroup(ORG, child!.id, rootB!.id, 0)).toBe(true);
    expect(rootA!.children.length).toBe(0);
    expect(rootB!.children.map((n) => n.id)).toEqual([child!.id]);
    expect(child!.children.map((n) => n.id)).toEqual([grandchild!.id]);

    // 移回根层：newParentId=''，插到根层开头
    expect(moveOrgGroup(ORG, child!.id, '', 0)).toBe(true);
    expect(rootB!.children.length).toBe(0);
    expect(space.groupTree[0].id).toBe(child!.id);

    // 环检测：rootA 不能移入自己的子树（rootA → child 已无子，改用 child/grandchild 验证）
    expect(moveOrgGroup(ORG, child!.id, grandchild!.id, 0)).toBe(false);
    expect(space.groupTree[0].id).toBe(child!.id);
    expect(moveOrgGroup(ORG, child!.id, child!.id, 0)).toBe(false);

    deleteOrgGroup(ORG, child!.id);
    deleteOrgGroup(ORG, grandchild!.id);
    deleteOrgGroup(ORG, rootA!.id);
    deleteOrgGroup(ORG, rootB!.id);
  });

  it('桥接上传原序裸值 toIndex（同层前移调整由内核统一做）', async () => {
    const orgGroupMove = vi.fn().mockResolvedValue({ success: true });
    const ok = () => Promise.resolve({ success: true });
    (window as any).__TAURI_INTERNALS__ = {};
    (window as any).electronAPI = {
      contacts: {
        orgGroupMove,
        orgGroupCreate: vi.fn(ok),
        orgGroupRename: vi.fn(ok),
        orgGroupDelete: vi.fn(ok)
      }
    };
    try {
      const space = contactsOf(ORG);
      const root = createOrgGroup(ORG, '', '桥接根');
      const a = createOrgGroup(ORG, root!.id, '桥接A');
      createOrgGroup(ORG, root!.id, '桥接B');
      createOrgGroup(ORG, root!.id, '桥接C');

      // 同层 [A,B,C] 把 A 移到下标 2：上传原序 2（不是本地调整后的 1）
      expect(moveOrgGroup(ORG, a!.id, root!.id, 2)).toBe(true);
      expect(root!.children.map((n) => n.id).indexOf(a!.id)).toBe(1);
      expect(orgGroupMove).toHaveBeenCalledWith(ORG, a!.id, 2, root!.id);

      for (const child of [...root!.children]) {
        deleteOrgGroup(ORG, child.id);
      }
      deleteOrgGroup(ORG, root!.id);
      expect(space.groupTree.some((n) => n.id === root!.id)).toBe(false);
    } finally {
      delete (window as any).__TAURI_INTERNALS__;
      delete (window as any).electronAPI;
    }
  });
});
