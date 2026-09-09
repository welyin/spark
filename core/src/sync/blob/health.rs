//! 副本健康度聚合（A3，personal-data §4.5）：域级摘要——
//! **确定性计算，任何节点凭相同 presence 账本 + manifest 集合复算一致**。
//!
//! 只提醒不处置（Q06/Q01 口径）：本模块只产出事实，不做任何机制性干预。

use serde::{Deserialize, Serialize};

use super::evict::list_local_blobs;
use super::quota::{self, QuotaStatus};
use super::{SyncResult, get_manifest, replica_summary};
use crate::storage::StorageBackend;

/// 域级副本健康度摘要（serde camelCase，壳层/前端直通）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobHealth {
    /// 域内未撤销设备数（核心数据全量副本数即此值）。
    pub device_count: usize,
    /// 副本目标 K = min(3, deviceCount)——设备 ≤3 台退化为全量
    /// （副本上限自然等于设备数）。
    pub k_target: usize,
    /// 本机有 manifest 的 blob 数（域级摘要的统计全集——presence 账本
    /// 以本机 manifest 为块数来源，无 manifest 的 blob 本机不可计数）。
    pub total_blobs: usize,
    /// 上述 blob 的内容字节总量（manifest.size 求和）。
    pub total_bytes: u64,
    /// 完整副本数 <K 的 blob 数（健康度短板计数）。
    pub under_k_blobs: usize,
    /// 全部 blob 的完整副本数最小值（最差副本水位；无 blob 时 None）。
    pub min_full_replicas: Option<usize>,
    /// 本机配额水位。
    pub quota: QuotaStatusView,
}

/// QuotaStatus 的 serde 视图（camelCase）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaStatusView {
    /// 生效配额（字节）。
    pub quota_bytes: u64,
    /// 当前水位（字节）。
    pub used_bytes: u64,
    /// 超出量（未超为 0）。
    pub over_bytes: u64,
}

impl From<QuotaStatus> for QuotaStatusView {
    fn from(s: QuotaStatus) -> Self {
        Self {
            quota_bytes: s.quota_bytes,
            used_bytes: s.used_bytes,
            over_bytes: s.over_bytes,
        }
    }
}

/// 聚合域级副本健康度。
pub fn blob_health<S: StorageBackend>(storage: &S) -> SyncResult<BlobHealth> {
    let device_count = quota::active_device_count(storage);
    let k = quota::k_target(device_count);
    let quota = quota::quota_status(storage)?;
    let mut health = BlobHealth {
        device_count,
        k_target: k,
        total_blobs: 0,
        total_bytes: 0,
        under_k_blobs: 0,
        min_full_replicas: None,
        quota: quota.into(),
    };
    for cid in list_local_blobs(storage)? {
        // replica_summary 依赖 manifest（块数来源），list_local_blobs 保证其在
        let Some((full, _per_chunk)) = replica_summary(storage, &cid)? else {
            continue;
        };
        let Some(manifest) = get_manifest(storage, &cid)? else {
            continue;
        };
        health.total_blobs += 1;
        health.total_bytes += manifest.size;
        if full < k {
            health.under_k_blobs += 1;
        }
        health.min_full_replicas = Some(
            health
                .min_full_replicas
                .map_or(full, |m: usize| m.min(full)),
        );
    }
    Ok(health)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use crate::sync::blob::presence::{PresenceRecord, bitmap_encode, presence_key};
    use crate::sync::blob::save_blob;

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn seed_devices(s: &mut MemoryStorage, n: usize) {
        for i in 0..n {
            let peer = format!("peer-{i}");
            let record = crate::device::DeviceRecord {
                peer_id: peer.clone(),
                device_uid: Some(format!("uid-{i}")),
                device_name: peer,
                os: "Windows".to_string(),
                arch: "x86_64".to_string(),
                macs: Vec::new(),
                app_version: String::new(),
                os_version: String::new(),
                updated_at: 1000,
                last_seen_at: 1000,
                revoked_at: None,
                device_pub_key: None,
            };
            s.put(&format!("device:{}", record.peer_id), &serde_json::to_string(&record).unwrap())
                .unwrap();
        }
    }

    fn write_presence(s: &mut MemoryStorage, cid: &str, uid: &str, chunks: usize) {
        let record = PresenceRecord {
            v: 1,
            cid: cid.to_string(),
            device_uid: uid.to_string(),
            chunks: bitmap_encode(&vec![true; chunks]),
        };
        s.put(&presence_key(cid, uid), &record.to_json()).unwrap();
    }

    #[test]
    fn empty_domain_health() {
        let mut s = MemoryStorage::new();
        seed_devices(&mut s, 1);
        let h = blob_health(&s).unwrap();
        assert_eq!(h.device_count, 1);
        assert_eq!(h.k_target, 1, "单设备 K=1（退化全量）");
        assert_eq!(h.total_blobs, 0);
        assert_eq!(h.min_full_replicas, None);
    }

    /// 固定账本 → 固定摘要（确定性）：5 设备（K=3），两个 blob 分别
    /// 3 副本（达标）与 2 副本（短板）。
    #[test]
    fn deterministic_summary_with_mixed_replicas() {
        let mut s = MemoryStorage::new();
        seed_devices(&mut s, 5);
        let m1 = save_blob(&mut s, "node-0", "uid-0", &pattern(1000), 1000).unwrap();
        let m2 = save_blob(&mut s, "node-0", "uid-0", &pattern(2000), 1001).unwrap();
        // m1：uid-0/1/2 三副本（=K）；m2：uid-0/1 两副本（<K）
        for uid in ["uid-1", "uid-2"] {
            write_presence(&mut s, &m1.cid, uid, 1);
        }
        write_presence(&mut s, &m2.cid, "uid-1", 1);
        let h = blob_health(&s).unwrap();
        assert_eq!(h.device_count, 5);
        assert_eq!(h.k_target, 3);
        assert_eq!(h.total_blobs, 2);
        assert_eq!(h.total_bytes, 3000);
        assert_eq!(h.under_k_blobs, 1, "仅 m2 副本不足");
        assert_eq!(h.min_full_replicas, Some(2));
        // 确定性：再算一遍逐字段一致
        assert_eq!(blob_health(&s).unwrap(), h);
    }

    /// K 语义域级口径：同一份 2 副本 blob，1/2/3/5 台下 under-K 判定随 K 变。
    #[test]
    fn k_semantics_at_domain_level() {
        for (devices, expected_k, expect_under_k) in [(1usize, 1usize, false), (2, 2, false), (3, 3, true), (5, 3, true)] {
            let mut s = MemoryStorage::new();
            seed_devices(&mut s, devices);
            let m = save_blob(&mut s, "node-0", "uid-0", &pattern(100), 1000).unwrap();
            write_presence(&mut s, &m.cid, "uid-1", 1); // 共 2 副本（uid-0 自己 + uid-1）
            let h = blob_health(&s).unwrap();
            assert_eq!(h.k_target, expected_k, "{devices} 台设备");
            assert_eq!(
                h.under_k_blobs, usize::from(expect_under_k),
                "{devices} 台（K={expected_k}）下 2 副本是否短板"
            );
        }
    }
}
