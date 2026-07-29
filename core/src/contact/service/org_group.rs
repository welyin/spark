//! 组织空间：分组树（children 数组顺序即同级排序）。

use crate::storage::StorageBackend;

use super::*;
use crate::contact::{ContactProfileRecord, OrgGroupNode, org_extra_prefix, org_tree_key};

impl ContactService {
    /// 新建组织分组（id 由调用方给定；kernel 门面以客户端生成的 id 落库，
    /// 不做冲突避让）：`parent_id` 为 `""` 挂根层；父不存在返回 `Ok(None)`
    /// （对齐 TS `createOrgGroup`）。
    pub fn create_org_group_with_id<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        parent_id: &str,
        id: &str,
        name: &str,
    ) -> Result<Option<OrgGroupNode>> {
        let org_id = require_org_space(space)?;
        let key = org_tree_key(org_id);
        let mut tree: Vec<OrgGroupNode> = read_vec(storage, &key)?;
        let node = OrgGroupNode {
            id: id.to_string(),
            name: name.to_string(),
            children: Vec::new(),
        };
        if parent_id.is_empty() {
            tree.push(node.clone());
        } else {
            let Some(siblings) = find_siblings_mut(&mut tree, parent_id) else {
                return Ok(None);
            };
            let parent = siblings
                .iter_mut()
                .find(|item| item.id == parent_id)
                .expect("siblings located by parent_id");
            parent.children.push(node.clone());
        }
        write_json(storage, &key, &tree)?;
        Ok(Some(node))
    }

    /// 重命名组织分组；不存在时忽略（对齐 TS `renameOrgGroup`）。
    pub fn rename_org_group<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        id: &str,
        name: &str,
    ) -> Result<()> {
        let org_id = require_org_space(space)?;
        let key = org_tree_key(org_id);
        let mut tree: Vec<OrgGroupNode> = read_vec(storage, &key)?;
        let Some(siblings) = find_siblings_mut(&mut tree, id) else {
            return Ok(());
        };
        let node = siblings
            .iter_mut()
            .find(|item| item.id == id)
            .expect("siblings located by id");
        node.name = name.to_string();
        write_json(storage, &key, &tree)
    }

    /// 删除组织分组：子节点提升到被删节点所在层；子树所有 id 涉及的成员附加
    /// 资料 `group_id` 复位为 `""`（对齐 TS `deleteOrgGroup`）。
    pub fn delete_org_group<S: StorageBackend>(storage: &mut S, space: &str, id: &str) -> Result<()> {
        let org_id = require_org_space(space)?;
        let key = org_tree_key(org_id);
        let mut tree: Vec<OrgGroupNode> = read_vec(storage, &key)?;
        let Some(siblings) = find_siblings_mut(&mut tree, id) else {
            return Ok(());
        };
        let index = siblings
            .iter()
            .position(|item| item.id == id)
            .expect("siblings located by id");
        let node = siblings.remove(index);
        let removed_ids = collect_ids(&node);
        for (offset, child) in node.children.into_iter().enumerate() {
            siblings.insert(index + offset, child);
        }
        write_json(storage, &key, &tree)?;
        let prefix = org_extra_prefix(org_id);
        for (key, mut profile) in scan_json::<S, ContactProfileRecord>(storage, &prefix)? {
            if removed_ids.iter().any(|removed| removed == &profile.group_id) {
                profile.group_id = String::new();
                write_json(storage, &key, &profile)?;
            }
        }
        Ok(())
    }

    /// 同级拖拽重排：只在节点当前所在层内移动，不改变树结构
    /// （越界夹紧；不存在时忽略，对齐 TS `moveOrgGroupSibling`）。
    pub fn move_org_group_sibling<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        id: &str,
        to_index: usize,
    ) -> Result<()> {
        let org_id = require_org_space(space)?;
        let key = org_tree_key(org_id);
        let mut tree: Vec<OrgGroupNode> = read_vec(storage, &key)?;
        let Some(siblings) = find_siblings_mut(&mut tree, id) else {
            return Ok(());
        };
        let from = siblings
            .iter()
            .position(|item| item.id == id)
            .expect("siblings located by id");
        // 对齐 TS `moveOrgGroupSibling`：to_index 以拖拽前原序为准（可等于长度
        // 表示移到末尾），越界夹紧到 [0, len]；源在目标位之前时摘除后目标索引
        // 前移一位——否则「拖到 C 前」会落到 C 后，与落点预测不符
        let target = to_index.min(siblings.len());
        let moved = siblings.remove(from);
        let insert_at = if from < target { target - 1 } else { target };
        siblings.insert(insert_at, moved);
        write_json(storage, &key, &tree)
    }
}

/// 子树（含自身）是否包含指定 id。
fn tree_contains(nodes: &[OrgGroupNode], id: &str) -> bool {
    nodes
        .iter()
        .any(|node| node.id == id || tree_contains(&node.children, id))
}

/// 查找 id 所在层（根数组或某节点的 children）的可变引用；不存在返回 `None`。
fn find_siblings_mut<'a>(tree: &'a mut Vec<OrgGroupNode>, id: &str) -> Option<&'a mut Vec<OrgGroupNode>> {
    if tree.iter().any(|node| node.id == id) {
        return Some(tree);
    }
    let index = tree.iter().position(|node| tree_contains(&node.children, id))?;
    find_siblings_mut(&mut tree[index].children, id)
}

/// 收集节点子树的全部 id（含自身）。
fn collect_ids(node: &OrgGroupNode) -> Vec<String> {
    let mut ids = vec![node.id.clone()];
    for child in &node.children {
        ids.extend(collect_ids(child));
    }
    ids
}
