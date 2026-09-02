//! org:meta 写路径的原子段原语（F8，org-meta-rmw-fix §2.2）。
//!
//! 背景：org:meta 的每条本地写路径此前是**两时点分离**的读-改-写——内容在
//! 操作开始时读（service 层 require_organization → 改 → save_record_pdsync），
//! pmeta 由版本化中间件在 batch 提交时才读当时最新值 bump；两时点之间入站
//! 合入落地对端新版本，写出即「旧内容 + vv 覆盖对端分量」的脱节记录，对端
//! 判 Remote 整值覆盖丢成员。
//!
//! 本原语把全部 org:meta 变更收敛到单一入口：
//! 1. **同步段持锁**：[`OrgMetaWriteLock`] 由 kernel 装配注入（facade / 入站
//!    落库 / worker 同一把 `io_lock`）——临界区仅限同步 sled 操作，**严禁在
//!    原子段内引入任何 await/网络调用**（跨 await 编排一律拆段：网络取数在
//!    段外，提交走本原语）；
//! 2. **提交前重读 pmeta**：若基线 vv 被外部推进（入站合入）→ 重读当前记录，
//!    用 F1 的 [`merge_org_meta_record`] 把「我的产出」与「当前记录」三路
//!    合并后再落库——版本化中间件基于**最新** pmeta bump，产出 vv 支配双方，
//!    内容与 vv 一致，脱节结构性消除。
//!
//! 已知残余（设计 §5，后续项收口）：三路合并 members 并集在「合法踢出与并发
//! 合入交错」下可复活被踢成员。

use std::sync::{Arc, Mutex};

use crate::storage::StorageBackend;

use super::super::types::{OrganizationRecord, organization_key};
use super::{OrganizationService, Result};

/// org:meta 原子段互斥锁（kernel 装配注入的 `io_lock` 同一把）。纯逻辑层
/// 只见 std 互斥锁类型，不感知 kernel/运行时。
pub type OrgMetaWriteLock = Arc<Mutex<()>>;

impl OrganizationService {
    /// org:meta 变更的原子段入口：读记录 + 读 pmeta（记 base_vv）→ mutate →
    /// 提交前重读 pmeta（基线被外部推进则三路合并）→ 写记录（版本化句柄上
    /// 由中间件基于最新 pmeta bump；raw 句柄无 pmeta，基线恒等、自动退化
    /// 为普通读-改-写）。
    ///
    /// `mutate` 返回 `Ok(true)` = 有变更要落库；`Ok(false)` = 幂等无写
    /// （不 bump 版本——对齐各变更路径的既有「无变化不落库」口径）。事务
    /// 追加等附属存储写放在 mutate 内（同属临界区）。
    ///
    /// 返回落库后的记录（无写时为当前记录）。
    pub fn update_record_atomic<S, F>(
        storage: &mut S,
        io_lock: &OrgMetaWriteLock,
        org_id: &str,
        mutate: F,
    ) -> Result<OrganizationRecord>
    where
        S: StorageBackend,
        F: FnOnce(&mut S, &mut OrganizationRecord) -> Result<bool>,
    {
        Self::update_record_atomic_with_hook(storage, io_lock, org_id, mutate, None)
    }

    /// [`Self::update_record_atomic`] 的测试变体：`commit_hook` 在 mutate 之后、
    /// 提交前重读之前执行——注入「入站合入推进 pmeta」的确定性竞态复现
    /// （间歇竞态不靠复跑赌命中率，org-meta-rmw-fix §4）。生产调用方传 None。
    pub fn update_record_atomic_with_hook<S, F>(
        storage: &mut S,
        io_lock: &OrgMetaWriteLock,
        org_id: &str,
        mutate: F,
        commit_hook: Option<&dyn Fn(&mut S)>,
    ) -> Result<OrganizationRecord>
    where
        S: StorageBackend,
        F: FnOnce(&mut S, &mut OrganizationRecord) -> Result<bool>,
    {
        let _guard = io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let key = organization_key(org_id);
        let read_vv = |storage: &S| {
            storage
                .get(&crate::sync::personal_meta_key(&key))
                .ok()
                .flatten()
                .and_then(|raw| serde_json::from_str::<crate::sync::meta::DocMeta>(&raw).ok())
                .map(|m| m.vv)
        };
        let base_vv = read_vv(storage);
        let mut record = Self::require_organization(storage, org_id)?;
        if !mutate(storage, &mut record)? {
            return Ok(record); // 幂等无写（不 bump 版本）
        }
        if let Some(hook) = commit_hook {
            hook(storage);
        }
        // 提交前校验：基线被外部推进（入站合入落地对端新版本）→ 三路合并
        // 「我的产出」（base + 我的变更）与「当前记录」（base + 外部变更），
        // 消除「旧内容 + 新 vv」的脱节写出。
        if read_vv(storage) != base_vv {
            let current = Self::require_organization(storage, org_id)?;
            log::warn!(
                "[ORG-META] RMW baseline advanced during mutation, three-way merging | org={org_id}"
            );
            record = crate::org::meta_merge::merge_org_meta_record(&record, &current);
        }
        Self::save_record(storage, &record)?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::types::{OrganizationMember, OrganizationRole};
    use crate::storage::MemoryStorage;
    use crate::sync::versioned::{VersionedStorage, shared_node_id};

    fn member(root_id: &str, role: OrganizationRole) -> OrganizationMember {
        OrganizationMember {
            root_id: root_id.to_string(),
            role,
            joined_at: 1000,
            added_by: "creator".to_string(),
            ..Default::default()
        }
    }

    fn prefab_org(storage: &mut MemoryStorage) -> OrganizationRecord {
        let record = OrganizationRecord {
            org_id: "org_01".to_string(),
            name: "t".to_string(),
            created_at: 1000,
            created_by: "root-a".to_string(),
            updated_at: 1000,
            members: vec![
                member("root-a", OrganizationRole::Admin),
                member("root-b", OrganizationRole::Member),
            ],
            ..Default::default()
        };
        OrganizationService::save_record(storage, &record).unwrap();
        record
    }

    /// F8 §4 原语级确定性复现：钩子注入「mutate 与提交之间一次入站合入」
    /// （对端新增成员 C 的版本 + 其 pmeta 分量）——断言提交产出 = 三路合并
    /// （我的变更 + 外部新增都在）、vv 支配双输入、本机分量按最新 pmeta bump。
    #[test]
    fn atomic_section_three_way_merges_when_baseline_advanced() {
        let inner = MemoryStorage::new();
        let mut s = VersionedStorage::new(inner, shared_node_id("node-b"));
        prefab_org(s.raw_mut());
        // 既有基线：B 已有一笔本地写（pmeta {node-b:1}）
        let base_record = OrganizationService::get_record(s.raw(), "org_01")
            .unwrap()
            .unwrap();
        crate::sync::put_personal(
            s.raw_mut(),
            "node-b",
            "org:meta:org_01",
            &serde_json::to_string(&base_record).unwrap(),
            1500,
        )
        .unwrap();
        let lock: OrgMetaWriteLock = Arc::new(Mutex::new(()));

        // 我的变更：B 本人改 nickname
        let hook = |st: &mut VersionedStorage<MemoryStorage>| {
            // 模拟入站合入：A 加了成员 C（内容 + pmeta {node-a:7}）——经 raw
            // 句柄写（对端版本不 bump 本机分量）
            let mut rec = OrganizationService::get_record(st.raw(), "org_01")
                .unwrap()
                .unwrap();
            rec.members.push(member("root-c", OrganizationRole::Member));
            rec.updated_at = 2000;
            crate::sync::apply_personal_remote_no_dlog(
                st.raw_mut(),
                "org:meta:org_01",
                &serde_json::to_string(&rec).unwrap(),
                &crate::sync::meta::DocMeta {
                    vv: [("node-a".to_string(), 7)].into_iter().collect(),
                    ts: 2000,
                    node_id: Some("node-a".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        };
        let out = OrganizationService::update_record_atomic_with_hook(
            &mut s,
            &lock,
            "org_01",
            |_st, rec| {
                rec.members
                    .iter_mut()
                    .find(|m| m.root_id == "root-b")
                    .unwrap()
                    .nickname = Some("B 改名".to_string());
                // 我的产出记录秩须高于外部版本（2100 > 2000）——F1 合并的字段组
                // 按记录秩选取，秩低侧的本人字段组变更会被覆盖（§5 字段组级残余）
                rec.updated_at = 2100;
                Ok(true)
            },
            Some(&hook),
        )
        .unwrap();

        // 三路合并：我的 nickname 变更 + 外部新增成员 C 都在
        let mb = out.members.iter().find(|m| m.root_id == "root-b").unwrap();
        assert_eq!(mb.nickname.as_deref(), Some("B 改名"), "我的变更保留");
        assert!(
            out.members.iter().any(|m| m.root_id == "root-c"),
            "外部新增成员 C 合并进来（不丢）"
        );
        // vv 支配双输入：{node-a:7}（外部）+ node-b 按最新 pmeta bump（1→2）
        let meta = crate::sync::get_personal_meta(s.raw(), "org:meta:org_01")
            .unwrap()
            .unwrap();
        assert_eq!(meta.vv.get("node-a"), Some(&7), "外部分量并入");
        assert_eq!(meta.vv.get("node-b"), Some(&2), "本机分量按最新 pmeta bump");
    }

    /// 无注入时行为与现状一致（普通 RMW 落库，本机分量 +1）。
    #[test]
    fn atomic_section_plain_path_unchanged() {
        let inner = MemoryStorage::new();
        let mut s = VersionedStorage::new(inner, shared_node_id("node-b"));
        prefab_org(s.raw_mut());
        let lock: OrgMetaWriteLock = Arc::new(Mutex::new(()));
        let out = OrganizationService::update_record_atomic(&mut s, &lock, "org_01", |_st, rec| {
            rec.name = "改名".to_string();
            rec.updated_at = 1600;
            Ok(true)
        })
        .unwrap();
        assert_eq!(out.name, "改名");
        let meta = crate::sync::get_personal_meta(s.raw(), "org:meta:org_01")
            .unwrap()
            .unwrap();
        assert_eq!(meta.vv.get("node-b"), Some(&1));
    }

    /// 幂等无写：mutate 返回 false → 不落库不 bump。
    #[test]
    fn atomic_section_noop_writes_nothing() {
        let inner = MemoryStorage::new();
        let mut s = VersionedStorage::new(inner, shared_node_id("node-b"));
        prefab_org(s.raw_mut());
        let lock: OrgMetaWriteLock = Arc::new(Mutex::new(()));
        let out =
            OrganizationService::update_record_atomic(&mut s, &lock, "org_01", |_st, _rec| {
                Ok(false)
            })
            .unwrap();
        assert_eq!(out.name, "t");
        assert!(
            crate::sync::get_personal_meta(s.raw(), "org:meta:org_01")
                .unwrap()
                .is_none(),
            "无写 → 无 pmeta"
        );
    }
}
