//! 设备清单模块：同一身份（rootId）下多设备记录的本地存储与本机设备信息采集。
//!
//! 设计背景（wiki/protocol/p2p-messages.md §19.4）：设备配对复用好友协议
//! （rootId==自己 的 FriendRecord 记录对端 peer 寻址），但 FriendRecord 不含
//! 设备元数据。本模块引入独立的设备清单模型——每台设备一条 [`DeviceRecord`]，
//! 以 peerId 为主键，承载设备名/操作系统/MAC 等用户可读的设备情况。
//!
//! 同步链路：设备信息经 dm 直连信封 `device-sync`（from==to==自己 rootId）在
//! 自设备间交换——本机 p2p 启动后向全部已配对设备投递本机记录；收到对端
//! device-sync 时落库并回发本机记录（握手式交换，覆盖「一方离线后上线」场景）。
//!
//! 本模块为纯逻辑层：只操作 [`crate::storage::StorageBackend`] 与 std 系统接口，
//! 不触碰网络。信封装配/投递在 kernel 的 dm_delivery / inbound_dm。

use serde::{Deserialize, Serialize};

use crate::storage::{ScanOptions, StorageBackend};

/// 设备记录键前缀（`device:{peerId}`；存储已是「每身份一个 sled 库」，无需身份前缀）。
pub(crate) const DEVICE_PREFIX: &str = "device:";

/// 设备记录（serde camelCase 紧凑 JSON）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRecord {
    /// 设备标识（libp2p peerId；keypair 持久化故 peerId 稳定）。
    pub peer_id: String,
    /// 设备名（hostname；采集失败为 "未知设备"）。
    pub device_name: String,
    /// 操作系统友好名（Windows / macOS / Linux / Android / iOS …）。
    pub os: String,
    /// CPU 架构（x86_64 / aarch64 …）。
    pub arch: String,
    /// 物理地址列表（MAC；平台限制采集不到时为空数组）。
    pub macs: Vec<String>,
    /// 本机信息最近一次采集/变更时间（ms；device-sync 冲突裁决用，新覆盖旧）。
    pub updated_at: i64,
    /// 最近一次收到该设备 device-sync 的时间（ms；本机记录恒等于 updated_at）。
    pub last_seen_at: i64,
}

/// 本机设备信息采集结果（未落库前的瞬态）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalDeviceInfo {
    pub device_name: String,
    pub os: String,
    pub arch: String,
    pub macs: Vec<String>,
}

/// 操作系统友好名（`std::env::consts::OS` → 展示文案）。
pub fn os_display_name() -> String {
    match std::env::consts::OS {
        "windows" => "Windows".to_string(),
        "macos" => "macOS".to_string(),
        "linux" => "Linux".to_string(),
        "android" => "Android".to_string(),
        "ios" => "iOS".to_string(),
        other => other.to_string(),
    }
}

/// 设备名采集：Windows `COMPUTERNAME` → unix `HOSTNAME` → `/proc/sys/kernel/hostname`
/// （Linux/Android 均可读）；全失败返回 "未知设备"。
pub fn collect_device_name() -> String {
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(name) = std::env::var(var) {
            let name = name.trim();
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    if let Ok(raw) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let name = raw.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    "未知设备".to_string()
}

/// 物理地址（MAC）采集。
///
/// Linux/Android：读 `/sys/class/net/*/address`（跳过 lo 回环、全零与 Android
/// 隐私占位 `02:00:00:00:00:00`；SELinux 限制时目录不可读，返回空数组）。
/// 其余平台（Windows/macOS 等）暂无零依赖采集路径，返回空数组——「允许获取
/// 的才采集」，前端对空数组不展示该行。
pub fn collect_macs() -> Vec<String> {
    let mut macs = sys_class_net_macs();
    macs.sort();
    macs.dedup();
    macs
}

/// `/sys/class/net` 读取（Linux/Android；其他平台恒空）。
fn sys_class_net_macs() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // 跳过回环与虚拟网络接口（容器/网桥/隧道等）
        if name == "lo"
            || name.starts_with("docker")
            || name.starts_with("veth")
            || name.starts_with("br-")
            || name.starts_with("virbr")
        {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(entry.path().join("address")) else {
            continue;
        };
        let mac = raw.trim().to_lowercase();
        if is_usable_mac(&mac) {
            out.push(mac);
        }
    }
    out
}

/// MAC 可用性过滤：形如 6 组 hex、非全零、非 Android 随机化占位。
fn is_usable_mac(mac: &str) -> bool {
    let parts: Vec<&str> = mac.split(':').collect();
    if parts.len() != 6 || !parts.iter().all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit())) {
        return false;
    }
    mac != "00:00:00:00:00:00" && mac != "02:00:00:00:00:00"
}

/// 校验 peerId 格式合法性（libp2p base58btc 编码：长度 20-100，仅允许 base58 字符集）。
pub fn is_usable_peer_id(peer_id: &str) -> bool {
    let len = peer_id.len();
    if len < 20 || len > 100 {
        return false;
    }
    peer_id
        .bytes()
        .all(|b| matches!(b, b'1'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'a'..=b'k' | b'm'..=b'z'))
}

/// 采集本机设备信息（不含 peerId——peerId 由 p2p 层提供）。
pub fn collect_local_device_info() -> LocalDeviceInfo {
    LocalDeviceInfo {
        device_name: collect_device_name(),
        os: os_display_name(),
        arch: std::env::consts::ARCH.to_string(),
        macs: collect_macs(),
    }
}

/// 设备清单服务（纯存储逻辑）。
pub struct DeviceService;

impl DeviceService {
    /// pdsync 感知的写入：落记录 + bump pmeta（P1 设备清单迁入）。
    pub fn upsert_pdsync<S: StorageBackend>(
        storage: &mut S,
        record: &DeviceRecord,
        now_ms: i64,
        node_id: &str,
    ) -> crate::contact::Result<()> {
        let key = format!("{DEVICE_PREFIX}{}", record.peer_id);
        let text = serde_json::to_string(record)?;
        crate::sync::put_personal(storage, node_id, &key, &text, now_ms)
            .map_err(crate::contact::sync_err_to_contact)?;
        Ok(())
    }

    /// 读取单台设备记录。
    pub fn get<S: StorageBackend>(storage: &S, peer_id: &str) -> crate::contact::Result<Option<DeviceRecord>> {
        let key = format!("{DEVICE_PREFIX}{peer_id}");
        let Some(raw) = storage.get(&key)? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&raw)?))
    }

    /// 全部设备记录（含本机），按 last_seen_at 降序。
    pub fn list<S: StorageBackend>(storage: &S) -> crate::contact::Result<Vec<DeviceRecord>> {
        let mut out = Vec::new();
        for (_key, value) in storage.scan(&ScanOptions::prefix(DEVICE_PREFIX))? {
            if let Ok(record) = serde_json::from_str::<DeviceRecord>(&value) {
                out.push(record);
            }
        }
        out.sort_by_key(|r| std::cmp::Reverse(r.last_seen_at));
        Ok(out)
    }

    /// 登记/刷新本机设备记录：采集系统信息，按 peerId 落库（updated_at 恒刷
    /// 新——本机采集是权威源；保留首次 seen 语义不需要，本机 last_seen==updated）。
    pub fn upsert_self<S: StorageBackend>(
        storage: &mut S,
        peer_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> crate::contact::Result<DeviceRecord> {
        let info = collect_local_device_info();
        // 已存在且系统信息未变：只刷 last_seen/updated（保持单调）
        let record = DeviceRecord {
            peer_id: peer_id.to_string(),
            device_name: info.device_name,
            os: info.os,
            arch: info.arch,
            macs: info.macs,
            updated_at: now_ms,
            last_seen_at: now_ms,
        };
        Self::upsert_pdsync(storage, &record, now_ms, node_id)?;
        Ok(record)
    }

    /// 应用对端 device-sync：仅当对端 updated_at 更新（或本地无记录）时落库；
    /// last_seen_at 恒刷新为接收时间。返回 `(记录, 是否发生内容变更)`。
    pub fn apply_remote<S: StorageBackend>(
        storage: &mut S,
        mut record: DeviceRecord,
        now_ms: i64,
        node_id: &str,
    ) -> crate::contact::Result<(DeviceRecord, bool)> {
        let existing = Self::get(storage, &record.peer_id)?;
        let changed = existing
            .as_ref()
            .map(|e| record.updated_at > e.updated_at)
            .unwrap_or(true);
        let base_updated = if changed {
            record.updated_at
        } else {
            existing.as_ref().map(|e| e.updated_at).unwrap_or(record.updated_at)
        };
        if !changed {
            // 内容不更新，但 last_seen 推进（设备在线证据）
            if let Some(mut e) = existing {
                e.last_seen_at = now_ms;
                Self::upsert_pdsync(storage, &e, now_ms, node_id)?;
                return Ok((e, false));
            }
        }
        record.updated_at = base_updated;
        record.last_seen_at = now_ms;
        Self::upsert_pdsync(storage, &record, now_ms, node_id)?;
        Ok((record, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_mac_filter() {
        assert!(is_usable_mac("aa:bb:cc:dd:ee:ff"));
        assert!(!is_usable_mac("00:00:00:00:00:00"));
        assert!(!is_usable_mac("02:00:00:00:00:00"));
        assert!(!is_usable_mac("not-a-mac"));
        assert!(!is_usable_mac("aa:bb:cc"));
    }

    #[test]
    fn usable_peer_id_format() {
        // 典型 libp2p Ed25519 peerId（base58btc ≈ 52 字符）
        assert!(is_usable_peer_id("12D3KooWRbyE1L8FJdY1Mq4NdWkHnGbnmuV7VNn5LbbKLkQDMXJm"));
        // 过短
        assert!(!is_usable_peer_id("12D3"));
        // 含非法字符（base58 不含 '0', 'O', 'I', 'l'）
        assert!(!is_usable_peer_id("12D3KooWRbyE1L8FJ0Y1Mq4NdWkHnGbnmuV7VNn5LbbKLkQDMXJm"));
    }

    #[test]
    fn os_display_name_known() {
        // 当前测试平台必然能映射出非空名
        assert!(!os_display_name().is_empty());
    }

    #[test]
    fn apply_remote_conflict_by_updated_at() {
        let mut storage = crate::storage::MemoryStorage::new();
        let older = DeviceRecord {
            peer_id: "peer-a".to_string(),
            device_name: "旧名字".to_string(),
            os: "Android".to_string(),
            arch: "aarch64".to_string(),
            macs: vec![],
            updated_at: 100,
            last_seen_at: 100,
        };
        let (applied, changed) = DeviceService::apply_remote(&mut storage, older.clone(), 100, "local-node").unwrap();
        assert!(changed);
        assert_eq!(applied.device_name, "旧名字");

        // 更旧的 updated_at：内容不覆盖，last_seen 推进
        let stale = DeviceRecord {
            device_name: "更旧".to_string(),
            updated_at: 50,
            ..older.clone()
        };
        let (applied, changed) = DeviceService::apply_remote(&mut storage, stale, 200, "local-node").unwrap();
        assert!(!changed);
        assert_eq!(applied.device_name, "旧名字");
        assert_eq!(applied.last_seen_at, 200);

        // 更新的 updated_at：内容覆盖
        let newer = DeviceRecord {
            device_name: "新名字".to_string(),
            updated_at: 300,
            ..older
        };
        let (applied, changed) = DeviceService::apply_remote(&mut storage, newer, 300, "local-node").unwrap();
        assert!(changed);
        assert_eq!(applied.device_name, "新名字");
    }
}
