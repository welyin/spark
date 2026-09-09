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

/// 本机设备 UID 的本地存储键（`p2p:*` 前缀不参与 pdsync，纯本地）。
///
/// deviceUid 是「物理设备」的稳定标识：随机 128bit，首次生成后持久化，
/// 重装/清数据才会变更。peerId 因 keypair 丢失/重装而漂移时，凭 deviceUid
/// 识别「同一台设备的新 peerId」，旧 peerId 记录随即墓碑化替换。
/// `pub`：集成测试按此键预置确定性 deviceUid（A1 blob presence 账本）。
pub const DEVICE_UID_KEY: &str = "p2p:device:uid";

/// 读取或创建本机设备 UID（同设备稳定，跨重启/peerId 漂移不变）。
pub fn get_or_create_device_uid<S: StorageBackend>(
    storage: &mut S,
) -> crate::contact::Result<String> {
    if let Some(uid) = storage.get(DEVICE_UID_KEY)? {
        let uid = uid.trim().to_string();
        if !uid.is_empty() {
            return Ok(uid);
        }
    }
    use rand::Rng as _;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    let uid = hex::encode(bytes);
    storage.put(DEVICE_UID_KEY, &uid)?;
    Ok(uid)
}

/// 设备记录（serde camelCase 紧凑 JSON）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRecord {
    /// 设备标识（libp2p peerId；keypair 持久化故 peerId 稳定）。
    pub peer_id: String,
    /// 物理设备稳定标识（随机 128bit hex；旧版本记录缺省为 None）。
    ///
    /// 同一 deviceUid 出现新 peerId（重装/keypair 丢失）时，旧 peerId 记录
    /// 由 upsert_self / apply_remote 墓碑化替换，陈旧 peerId 全账号清除。
    #[serde(default)]
    pub device_uid: Option<String>,
    /// 设备名（桌面=hostname；Android=ro.product.marketname 厂商营销名；采集失败为 "未知设备"）。
    pub device_name: String,
    /// 操作系统友好名（Windows / macOS / Linux / Android / iOS …）。
    pub os: String,
    /// CPU 架构（x86_64 / aarch64 …）。
    pub arch: String,
    /// 物理地址列表（MAC；平台限制采集不到时为空数组）。
    pub macs: Vec<String>,
    /// 本机运行的应用版本（如 0.2.1；旧记录/旧版本对端缺省为空串，前端展示「—」）。
    #[serde(default)]
    pub app_version: String,
    /// 操作系统版本号（如 10.0.22631 / 14.5 / Android 14；采集失败为空串）。
    #[serde(default)]
    pub os_version: String,
    /// 本机信息最近一次采集/变更时间（ms；device-sync 冲突裁决用，新覆盖旧）。
    pub updated_at: i64,
    /// 最近一次收到该设备 device-sync 的时间（ms；本机记录恒等于 updated_at）。
    pub last_seen_at: i64,
    /// 撤销时间戳（ms）。`None` 表示正常设备；被撤销后保留该字段以阻止
    /// 同步/寻址再次将其「洗白」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
    /// 设备 Ed25519 公钥（base64 编码的 32B 原始公钥；M3 epoch 包裹用）。
    /// 旧版本记录/未采集到公钥的对端可为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_pub_key: Option<String>,
}

/// 本机设备信息采集结果（未落库前的瞬态）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalDeviceInfo {
    pub device_name: String,
    pub os: String,
    pub os_version: String,
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

/// 操作系统版本号采集（尽力而为，失败返回空串——前端对空值展示「—」）：
/// - Android：系统属性 `ro.build.version.release`（如 14）；
/// - macOS：`sw_vers -productVersion`（如 14.5）；
/// - Windows：`cmd /c ver` 中提取 `主.次.构建`（10.0.22631 形态，跨语言环境）；
/// - Linux：`/etc/os-release` 的 PRETTY_NAME（如 Ubuntu 22.04.4 LTS）。
pub fn collect_os_version() -> String {
    #[cfg(target_os = "android")]
    {
        if let Some(v) = android_system_property("ro.build.version.release") {
            let v = v.trim();
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let v = run_capture(&["sw_vers", "-productVersion"]);
        if !v.is_empty() {
            return v;
        }
    }
    #[cfg(target_os = "windows")]
    {
        let v = extract_version_token(&run_capture(&["cmd", "/c", "ver"]));
        if !v.is_empty() {
            return v;
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(pretty) = os_release_pretty_name() {
            return pretty;
        }
    }
    String::new()
}

/// 运行外部命令并捕获 stdout 首行（非零退出/启动失败返回空串）。
///
/// 折中说明：采集本机操作系统版本无纯 std 跨平台接口，必须调用系统命令
/// （`sw_vers` / `cmd /c ver`）；采集仅发生在 p2p 启动与本机条目兜底，频率极低。
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn run_capture(args: &[&str]) -> String {
    let Ok(output) = std::process::Command::new(args[0])
        .args(&args[1..])
        .output()
    else {
        return String::new();
    };
    if !output.status.success() {
        return String::new();
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// 从文本中提取首个形如 `主.次.构建`（可带补丁段）的版本号；找不到返回空串。
///
/// Windows `cmd /c ver` 输出随语言变化（"[Version ...]" / "[版本 ...]"），
/// 但版本号形态一致，按数字点段提取即可跨语言环境。
#[cfg(target_os = "windows")]
fn extract_version_token(text: &str) -> String {
    for part in text.split_whitespace() {
        let part = part.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
        let digits: Vec<&str> = part.split('.').filter(|s| !s.is_empty()).collect();
        if digits.len() >= 3 && digits.iter().all(|s| s.bytes().all(|b| b.is_ascii_digit())) {
            return digits[..3].join(".");
        }
    }
    String::new()
}

/// Linux：读 `/etc/os-release` 的 `PRETTY_NAME`（引号剥除；失败返回 None）。
#[cfg(target_os = "linux")]
fn os_release_pretty_name() -> Option<String> {
    let raw = std::fs::read_to_string("/etc/os-release").ok()?;
    for line in raw.lines() {
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            let v = v.trim().trim_matches('"').trim();
            return if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            };
        }
    }
    None
}

/// 设备名采集：
/// - Android：读系统属性 `ro.product.marketname`（厂商营销名，如 "REDMI Turbo 4 Pro"，
///   比 `ro.product.model` 机型号更友好）→ 回退 `ro.product.model`。
///   桌面通用的 env/hostname 文件在 Android 上不可用（app 进程无 `HOSTNAME`，
///   `/proc/sys/kernel/hostname` 被 SELinux 拒绝），必须走 Bionic 系统属性。
/// - 桌面端：Windows `COMPUTERNAME` → unix `HOSTNAME` → `/proc/sys/kernel/hostname`。
/// 全失败返回 "未知设备"。
pub fn collect_device_name() -> String {
    #[cfg(target_os = "android")]
    {
        if let Some(name) = android_system_property("ro.product.marketname") {
            let name = name.trim();
            if !name.is_empty() {
                return name.to_string();
            }
        }
        if let Some(name) = android_system_property("ro.product.model") {
            let name = name.trim();
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
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

/// Android 系统属性读取（零依赖 FFI：Bionic libc 的 `__system_property_get`）。
/// app 进程可读公开 `ro.*` 属性、无需特权；SELinux 对 `/proc` 文件的限制不影响它。
/// 属性不存在/为空返回 `None`。
#[cfg(target_os = "android")]
fn android_system_property(key: &str) -> Option<String> {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int};

    // 系统属性值上限 PROP_VALUE_MAX=92；缓冲给足余量（__system_property_get 要求 ≥92+1）。
    const BUF_LEN: usize = 256;

    // 按名查询系统属性：值写入 value 并以 NUL 结尾，返回实际长度（未找到返回 0）。
    unsafe extern "C" {
        fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
    }

    let key_c = CString::new(key).ok()?;
    let mut buf = [0 as c_char; BUF_LEN];
    let len = unsafe { __system_property_get(key_c.as_ptr(), buf.as_mut_ptr()) };
    if len <= 0 {
        return None;
    }
    let len = (len as usize).min(BUF_LEN);
    let bytes: Vec<u8> = buf[..len].iter().map(|&b| b as u8).collect();
    Some(String::from_utf8_lossy(&bytes).into_owned())
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
    if parts.len() != 6
        || !parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
    {
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
        os_version: collect_os_version(),
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

    /// 远端 device-sync 合入的写入：对**对端**记账（防回声）——语义同
    /// `upsert_pdsync` 但记账到 `remote_node_id`，不推进本机分量。
    fn upsert_remote<S: StorageBackend>(
        storage: &mut S,
        record: &DeviceRecord,
        now_ms: i64,
        remote_node_id: &str,
        local_node_id: &str,
    ) -> crate::contact::Result<()> {
        let key = format!("{DEVICE_PREFIX}{}", record.peer_id);
        let text = serde_json::to_string(record)?;
        crate::sync::personal::apply_snapshot_remote(
            storage,
            remote_node_id,
            local_node_id,
            &key,
            &text,
            now_ms,
        )
        .map_err(crate::contact::sync_err_to_contact)?;
        Ok(())
    }

    /// 读取单台设备记录。
    pub fn get<S: StorageBackend>(
        storage: &S,
        peer_id: &str,
    ) -> crate::contact::Result<Option<DeviceRecord>> {
        let key = format!("{DEVICE_PREFIX}{peer_id}");
        let Some(raw) = storage.get(&key)? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&raw)?))
    }

    /// 按 deviceUid 查找设备记录（用于撤销通知命中）。
    pub fn get_by_device_uid<S: StorageBackend>(
        storage: &S,
        device_uid: &str,
    ) -> crate::contact::Result<Option<DeviceRecord>> {
        for (_key, value) in storage.scan(&ScanOptions::prefix(DEVICE_PREFIX))? {
            if let Ok(record) = serde_json::from_str::<DeviceRecord>(&value) {
                if record.device_uid.as_deref() == Some(device_uid) {
                    return Ok(Some(record));
                }
            }
        }
        Ok(None)
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

    /// 追加本地安全日志（`security:log:{ts}:{kind}:{deviceId}`）。append-only，不进 pdsync。
    pub fn append_security_log<S: StorageBackend>(
        storage: &mut S,
        kind: &str,
        fields: serde_json::Value,
        ts: i64,
    ) -> crate::contact::Result<()> {
        let device_id = fields
            .get("deviceId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let key = if device_id.is_empty() {
            format!("security:log:{ts}:{kind}")
        } else {
            format!("security:log:{ts}:{kind}:{device_id}")
        };
        let mut value = fields;
        value["kind"] = serde_json::json!(kind);
        value["ts"] = serde_json::json!(ts);
        let raw = value.to_string();
        eprintln!("[security-log] {key} = {raw}");
        storage.put(&key, &raw)?;
        Ok(())
    }

    /// 将指定设备标记为撤销。幂等：已撤销返回既有的 revoked_at。
    pub fn mark_revoked<S: StorageBackend>(
        storage: &mut S,
        device_uid: &str,
        revoked_at: i64,
        now_ms: i64,
        node_id: &str,
    ) -> crate::contact::Result<Option<DeviceRecord>> {
        let Some(mut record) = Self::get_by_device_uid(storage, device_uid)? else {
            return Ok(None);
        };
        if record.revoked_at.is_none() {
            record.revoked_at = Some(revoked_at);
            Self::upsert_pdsync(storage, &record, now_ms, node_id)?;
        }
        Ok(Some(record))
    }

    /// 登记/刷新本机设备记录：采集系统信息，按 peerId 落库（updated_at 恒刷
    /// 新——本机采集是权威源；保留首次 seen 语义不需要，本机 last_seen==updated）。
    ///
    /// 落库前先做同 deviceUid 替换：本机 peerId 漂移（keypair 丢失/重装）后，
    /// 旧 peerId 的本机记录墓碑化，设备清单不累积陈旧条目。
    ///
    /// app_version 由调用方注入（kernel 配置的应用版本；升级后随 p2p 启动重采集
    /// 刷新），与系统采集信息（name/os/arch/macs/os_version）区分。
    pub fn upsert_self<S: StorageBackend>(
        storage: &mut S,
        peer_id: &str,
        now_ms: i64,
        node_id: &str,
        app_version: &str,
        device_pub_key: Option<String>,
    ) -> crate::contact::Result<DeviceRecord> {
        let info = collect_local_device_info();
        let device_uid = get_or_create_device_uid(storage)?;
        Self::tombstone_same_device_peers(storage, Some(&device_uid), peer_id, now_ms, node_id)?;
        // 撤销粘性：本地已有本机记录时，保留 revoked_at，防止同步/重采集把
        // 已撤销设备「洗白」。
        let existing = Self::get(storage, peer_id)?;
        let revoked_at = existing.as_ref().and_then(|r| r.revoked_at);
        // 公钥粘性：已有值时不回退（兜底采集可能后补）。
        let device_pub_key = device_pub_key.or_else(|| {
            existing
                .and_then(|r| r.device_pub_key)
                .filter(|s| !s.is_empty())
        });
        // 已存在且系统信息未变：只刷 last_seen/updated（保持单调）
        let record = DeviceRecord {
            peer_id: peer_id.to_string(),
            device_uid: Some(device_uid),
            device_name: info.device_name,
            os: info.os,
            os_version: info.os_version,
            arch: info.arch,
            macs: info.macs,
            app_version: app_version.to_string(),
            updated_at: now_ms,
            last_seen_at: now_ms,
            revoked_at,
            device_pub_key,
        };
        Self::upsert_pdsync(storage, &record, now_ms, node_id)?;
        Ok(record)
    }

    /// 同 deviceUid 替换：清单中同 deviceUid 但 peerId 不同的记录墓碑化
    /// （对端 peerId 漂移后的旧身份；无 deviceUid 的旧版本记录无法归属，不动）。
    fn tombstone_same_device_peers<S: StorageBackend>(
        storage: &mut S,
        device_uid: Option<&str>,
        keep_peer_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> crate::contact::Result<()> {
        let Some(uid) = device_uid else {
            return Ok(());
        };
        for (_key, value) in storage.scan(&ScanOptions::prefix(DEVICE_PREFIX))? {
            let Ok(existing) = serde_json::from_str::<DeviceRecord>(&value) else {
                continue;
            };
            if existing.device_uid.as_deref() == Some(uid) && existing.peer_id != keep_peer_id {
                let stale_key = format!("{DEVICE_PREFIX}{}", existing.peer_id);
                crate::sync::delete_personal(storage, node_id, &stale_key, now_ms)
                    .map_err(crate::contact::sync_err_to_contact)?;
            }
        }
        Ok(())
    }

    /// 应用对端 device-sync：仅当对端 updated_at 更新（或本地无记录）时落库；
    /// last_seen_at 恒刷新为接收时间。返回 `(记录, 是否发生内容变更)`。
    ///
    /// 落库前先做同 deviceUid 替换：对端 peerId 漂移（重装/keypair 丢失）后的
    /// 旧 peerId 记录墓碑化——两端各自执行同一规则，陈旧条目全账号收敛清除。
    pub fn apply_remote<S: StorageBackend>(
        storage: &mut S,
        mut record: DeviceRecord,
        now_ms: i64,
        remote_node_id: &str,
        local_node_id: &str,
    ) -> crate::contact::Result<(DeviceRecord, bool)> {
        Self::tombstone_same_device_peers(
            storage,
            record.device_uid.as_deref(),
            &record.peer_id,
            now_ms,
            remote_node_id,
        )?;
        let existing = Self::get(storage, &record.peer_id)?;
        // 撤销粘性：本地已撤销则保留 revoked_at，不允许远端同步洗白。
        if existing.as_ref().is_some_and(|e| e.revoked_at.is_some()) && record.revoked_at.is_none()
        {
            record.revoked_at = existing.as_ref().and_then(|e| e.revoked_at);
        }
        let changed = existing
            .as_ref()
            .map(|e| record.updated_at > e.updated_at || record.revoked_at != e.revoked_at)
            .unwrap_or(true);
        let base_updated = if changed {
            record.updated_at
        } else {
            existing
                .as_ref()
                .map(|e| e.updated_at)
                .unwrap_or(record.updated_at)
        };
        if !changed {
            // 内容不更新，但 last_seen 推进（设备在线证据）；对端记账防回声
            if let Some(mut e) = existing {
                e.last_seen_at = now_ms;
                Self::upsert_remote(storage, &e, now_ms, remote_node_id, local_node_id)?;
                return Ok((e, false));
            }
        }
        record.updated_at = base_updated;
        record.last_seen_at = now_ms;
        Self::upsert_remote(storage, &record, now_ms, remote_node_id, local_node_id)?;
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
        assert!(is_usable_peer_id(
            "12D3KooWRbyE1L8FJdY1Mq4NdWkHnGbnmuV7VNn5LbbKLkQDMXJm"
        ));
        // 过短
        assert!(!is_usable_peer_id("12D3"));
        // 含非法字符（base58 不含 '0', 'O', 'I', 'l'）
        assert!(!is_usable_peer_id(
            "12D3KooWRbyE1L8FJ0Y1Mq4NdWkHnGbnmuV7VNn5LbbKLkQDMXJm"
        ));
    }

    #[test]
    fn os_display_name_known() {
        // 当前测试平台必然能映射出非空名
        assert!(!os_display_name().is_empty());
    }

    /// 本机 peerId 漂移：同 deviceUid 的旧 peerId 记录被墓碑化替换，
    /// 清单不累积陈旧条目（PC keypair 持久化失效时每重启换 peerId 的场景）。
    #[test]
    fn upsert_self_replaces_stale_peer_id_same_device() {
        let mut storage = crate::storage::MemoryStorage::new();
        let first =
            DeviceService::upsert_self(&mut storage, "peer-old", 100, "node-a", "0.2.1", None)
                .unwrap();
        let uid = first.device_uid.clone().expect("本机记录必带 deviceUid");

        // peerId 漂移（模拟 keypair 重新生成），同库再登记
        let second =
            DeviceService::upsert_self(&mut storage, "peer-new", 200, "node-a", "0.2.1", None)
                .unwrap();
        assert_eq!(
            second.device_uid.as_deref(),
            Some(uid.as_str()),
            "同设备 UID 稳定"
        );

        // 旧 peerId 记录本体已删 + 墓碑 pmeta 保留（供同步传播）
        assert!(DeviceService::get(&storage, "peer-old").unwrap().is_none());
        let meta = crate::sync::get_personal_meta(&storage, "device:peer-old")
            .unwrap()
            .expect("旧记录墓碑保留");
        assert!(crate::sync::is_tombstone(&meta));
        assert!(DeviceService::get(&storage, "peer-new").unwrap().is_some());
    }

    /// 对端 peerId 漂移：apply_remote 收到同 deviceUid 新 peerId 时，
    /// 本地旧 peerId 记录墓碑化。
    #[test]
    fn apply_remote_replaces_stale_peer_id_same_device() {
        let mut storage = crate::storage::MemoryStorage::new();
        let old = DeviceRecord {
            peer_id: "peer-old".to_string(),
            device_uid: Some("uid-x".to_string()),
            device_name: "手机".to_string(),
            os: "Android".to_string(),
            os_version: "14".to_string(),
            arch: "aarch64".to_string(),
            macs: vec![],
            app_version: String::new(),
            updated_at: 100,
            last_seen_at: 100,
            revoked_at: None,
            device_pub_key: None,
        };
        DeviceService::upsert_pdsync(&mut storage, &old, 100, "node-a").unwrap();

        let drifted = DeviceRecord {
            peer_id: "peer-new".to_string(),
            updated_at: 200,
            ..old.clone()
        };
        let (_, changed) =
            DeviceService::apply_remote(&mut storage, drifted, 300, "node-b", "node-a").unwrap();
        assert!(changed);
        assert!(DeviceService::get(&storage, "peer-old").unwrap().is_none());
        assert!(DeviceService::get(&storage, "peer-new").unwrap().is_some());
    }

    /// 无 deviceUid 的旧版本记录不参与替换（无法归属，防误删）。
    #[test]
    fn records_without_device_uid_are_not_replaced() {
        let mut storage = crate::storage::MemoryStorage::new();
        let legacy = DeviceRecord {
            peer_id: "peer-legacy".to_string(),
            device_uid: None,
            device_name: "旧设备".to_string(),
            os: "Android".to_string(),
            os_version: "14".to_string(),
            arch: "aarch64".to_string(),
            macs: vec![],
            app_version: String::new(),
            updated_at: 100,
            last_seen_at: 100,
            revoked_at: None,
            device_pub_key: None,
        };
        DeviceService::upsert_pdsync(&mut storage, &legacy, 100, "node-a").unwrap();

        let incoming = DeviceRecord {
            peer_id: "peer-new".to_string(),
            device_uid: Some("uid-y".to_string()),
            ..legacy.clone()
        };
        DeviceService::apply_remote(&mut storage, incoming, 200, "node-b", "node-a").unwrap();
        assert!(
            DeviceService::get(&storage, "peer-legacy")
                .unwrap()
                .is_some(),
            "无 deviceUid 的旧记录保留（手动清理）"
        );
    }

    #[test]
    fn apply_remote_conflict_by_updated_at() {
        let mut storage = crate::storage::MemoryStorage::new();
        let older = DeviceRecord {
            peer_id: "peer-a".to_string(),
            device_uid: None,
            device_name: "旧名字".to_string(),
            os: "Android".to_string(),
            os_version: "14".to_string(),
            arch: "aarch64".to_string(),
            macs: vec![],
            app_version: String::new(),
            updated_at: 100,
            last_seen_at: 100,
            revoked_at: None,
            device_pub_key: None,
        };
        let (applied, changed) = DeviceService::apply_remote(
            &mut storage,
            older.clone(),
            100,
            "remote-node",
            "local-node",
        )
        .unwrap();
        assert!(changed);
        assert_eq!(applied.device_name, "旧名字");

        // 更旧的 updated_at：内容不覆盖，last_seen 推进
        let stale = DeviceRecord {
            device_name: "更旧".to_string(),
            updated_at: 50,
            ..older.clone()
        };
        let (applied, changed) =
            DeviceService::apply_remote(&mut storage, stale, 200, "remote-node", "local-node")
                .unwrap();
        assert!(!changed);
        assert_eq!(applied.device_name, "旧名字");
        assert_eq!(applied.last_seen_at, 200);

        // 更新的 updated_at：内容覆盖
        let newer = DeviceRecord {
            device_name: "新名字".to_string(),
            updated_at: 300,
            ..older
        };
        let (applied, changed) =
            DeviceService::apply_remote(&mut storage, newer, 300, "remote-node", "local-node")
                .unwrap();
        assert!(changed);
        assert_eq!(applied.device_name, "新名字");
    }

    /// 旧版本记录 JSON（无 app_version/os_version/revokedAt 字段）可反序列化，缺省为空串/None。
    #[test]
    fn legacy_record_json_without_new_fields_deserializes() {
        let legacy_json = r#"{"peerId":"peer-legacy","deviceName":"旧设备","os":"Android","arch":"aarch64","macs":[],"updatedAt":100,"lastSeenAt":100}"#;
        let record: DeviceRecord = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(record.app_version, "");
        assert_eq!(record.os_version, "");
        assert_eq!(record.device_name, "旧设备");
        assert_eq!(record.revoked_at, None);
    }

    /// `cmd /c ver` 输出跨语言环境（Version/版本）都能提取主.次.构建。
    #[cfg(target_os = "windows")]
    #[test]
    fn extract_version_token_parses_ver_output() {
        assert_eq!(
            extract_version_token("Microsoft Windows [版本 10.0.22631.4037]"),
            "10.0.22631"
        );
        assert_eq!(
            extract_version_token("Microsoft Windows [Version 10.0.19045.4894]"),
            "10.0.19045"
        );
        assert_eq!(extract_version_token("no version here"), "");
    }
}
