//! 登录链路：注册（initialize）/解锁/锁定/切换活动身份/助记词与备份码恢复，
//! 以及登录成功后的 p2p 兜底启动（"登录即在线"）。

use super::{
    InitIdentityResult, check_password, map_identity_decrypt_error, normalize_mnemonic_input,
};
use crate::identity::{self, IdentityFile};
use crate::kernel::Kernel;
use crate::kernel::error::{KernelError, Result};
use crate::pw;
use crate::storage::StorageBackend;

impl Kernel {
    /// 登录链路收尾：配置含 p2p 时尽力启动（"登录即在线"，对齐 TS
    /// `ensureCoreServicesStarted` 的兜底语义）。失败仅记录到
    /// `p2p_start_error`，不使登录失败；已在运行则不动。
    fn ensure_p2p_after_login(&mut self) {
        if self.config.p2p.is_none() || self.p2p.is_some() {
            return;
        }
        if let Err(e) = self.start_p2p() {
            eprintln!("[kernel] login auto start p2p failed: {e}");
            self.p2p_start_error = Some(e.to_string());
        }
    }

    /// `initialize`（注册）：生成 24 词中文助记词 → root 派生 → v2 加密落盘，
    /// 设为活动身份并解锁；存储目录对齐到该身份。
    ///
    /// 返回明文助记词（仅此一次展示）。
    pub fn init_identity(
        &mut self,
        password: &str,
        nickname: &str,
        avatar: Option<&str>,
    ) -> Result<InitIdentityResult> {
        check_password(password)?;
        let mnemonic = identity::generate_mnemonic()?;
        let (file, identity, key) =
            identity::recover_identity_and_key(&mnemonic, password, nickname, avatar)?;
        let seed = identity::parse_mnemonic(&mnemonic)?.seed;
        self.write_identity_file(&file)?;
        self.write_active_root_id(&file.root_id)?;
        self.align_storage(&file.root_id)?;
        // §13.9：注册时即持久化设备身份，避免首次 unlock 前 ack 用 local-node。
        self.ensure_p2p_identity_ready()?;
        let root_id = file.root_id.clone();
        self.set_unlocked(identity, seed, password, Some(key));
        if let Err(e) = self.on_unlock_password_ops(password) {
            log::error!("[init-identity] password ops failed: {e}");
        }
        self.ensure_p2p_after_login();
        Ok(InitIdentityResult { root_id, mnemonic })
    }

    /// `unlock`：密码解锁指定身份（缺省为活动身份），设为当前并触发存储对齐。
    /// v1 遗留文件解锁成功后按 spec §5 迁移为 v2。
    pub fn unlock(&mut self, password: &str, root_id: Option<&str>) -> Result<String> {
        let target = match root_id {
            Some(rid) => rid.to_string(),
            None => self
                .read_active_root_id()?
                .ok_or(KernelError::NotInitialized)?,
        };
        let Some(mut file) = self.read_identity_file(&target)? else {
            return Err(KernelError::Internal("该账号不在本设备上".to_string()));
        };
        if file.version == identity::file::FILE_VERSION_V1 {
            file =
                identity::migrate_v1_to_v2(&file, password).map_err(map_identity_decrypt_error)?;
            self.write_identity_file(&file)?;
        }
        let (payload, identity, session_key) =
            identity::unlock_identity_and_key(&file, password).map_err(map_identity_decrypt_error)?;
        if identity.id() != file.root_id {
            return Err(KernelError::Internal(
                "Root identity verification failed".to_string(),
            ));
        }
        let seed = identity::parse_mnemonic(&payload.mnemonic)?.seed;
        self.write_active_root_id(&file.root_id)?;
        self.align_storage(&file.root_id)?;
        // P2 反向回写（§5.5）：锁定期间 pdsync 可能已把更新的资料合入 sled
        // `profile:self` 镜像——sled 较身份文件新时以 sled 覆盖身份文件资料
        // （会话密钥重封，沿用既有 salt，会话密钥保持有效）。须在 set_unlocked
        // 前完成，保证共享格读到最新资料。
        self.apply_sled_profile_to_identity(&mut file, password, session_key.as_ref());
        let root_id = file.root_id.clone();
        // §13.9：unlock 入口同其他「首次解锁」路径，确保真实 peerId 已就绪。
        self.ensure_p2p_identity_ready()?;
        self.set_unlocked(identity, seed, password, session_key);
        if let Err(e) = self.on_unlock_password_ops(password) {
            log::error!("[unlock] password ops failed: {e}");
        }
        self.ensure_p2p_after_login();
        // P2：存量资料迁移——sled `profile:self` 为空但身份文件有资料时，首次
        // unlock 一次性写入 sled（幂等）。此后以 sled 为 pdsync 读源。
        // 置于 p2p 兜底启动之后：nodeId 取真实 peerId（或持久化身份派生值），
        // 避免迁移写入的 pmeta 恒为 {local-node:1} 导致跨设备不收敛。
        self.migrate_profile_to_sled(&file);
        // 存量迁移：旧版整域单 key 的联系人标签/分组拆分为独立记录（幂等；
        // 读路径已用新前缀，不迁移则老用户升级后标签/分组不可见）
        self.migrate_contact_items_to_records();
        Ok(root_id)
    }

    /// P2 反向回写（§5.5）：锁定态下 pdsync 合入只更新 sled `profile:self`
    /// 镜像；unlock 时若 sled 资料较身份文件新（pmeta ts > 文件 updatedAt），
    /// 以 sled 覆盖身份文件资料字段（优先以会话密钥重封——沿用既有 salt、免
    /// 重跑 scrypt，且保持缓存密钥有效；v1 遗留无密钥时退回口令重封，原子
    /// 落盘）。sled 缺失/不更新/任一步失败均静默跳过（身份文件保持原样，
    /// 后续 pdsync 再收敛）。
    fn apply_sled_profile_to_identity(
        &self,
        file: &mut IdentityFile,
        password: &str,
        session_key: Option<&[u8; identity::crypto::KEY_LEN]>,
    ) {
        let Ok(storage) = self.require_storage() else {
            return;
        };
        let Some(raw) = storage.get(super::PROFILE_SELF_KEY).ok().flatten() else {
            return;
        };
        let Ok(profile) = serde_json::from_str::<super::SyncableProfile>(&raw) else {
            return;
        };
        // sled 侧资料时间取 pmeta ts（pdsync LWW 裁决水印）；无 pmeta 视为不新
        let sled_ts = crate::sync::personal::get_personal_meta(storage, super::PROFILE_SELF_KEY)
            .ok()
            .flatten()
            .map(|meta| meta.ts)
            .unwrap_or(0);
        if sled_ts <= file.updated_at as i64 {
            return;
        }
        let info = profile.to_profile_info();
        let avatar = match info.avatar.as_deref() {
            Some(a) if !a.is_empty() => Some(Some(a)),
            _ => Some(None),
        };
        let gender = Some(info.gender.as_deref().unwrap_or(""));
        let region = Some(info.region.as_deref().unwrap_or(""));
        let signature = Some(info.signature.as_deref().unwrap_or(""));
        let applied = match session_key {
            Some(key) => identity::update_profile_with_key(
                file,
                key,
                info.nickname.as_deref(),
                avatar,
                gender,
                region,
                signature,
            ),
            None => identity::update_profile(
                file,
                password,
                info.nickname.as_deref(),
                avatar,
                gender,
                region,
                signature,
            ),
        };
        if applied.is_err() {
            return;
        }
        let Ok(text) = serde_json::to_string_pretty(file) else {
            return;
        };
        let path = self.identity_file_path(&file.root_id);
        let _ = super::write_identity_file_atomic(&path, &text);
    }

    /// 存量迁移：旧版整域单 key 的联系人标签/分组（`ct:tags`/`ct:groups`）
    /// 拆分为独立记录（`ct:tag:{id}`/`ct:group:{id}`，随 pdsync 同步）。幂等；
    /// 失败仅记录日志，不阻塞登录。
    fn migrate_contact_items_to_records(&mut self) {
        let node_id = self.sync_node_id();
        let now = crate::p2p::node::system_now_ms();
        let Ok(storage) = self.require_storage_mut() else {
            return;
        };
        if let Err(e) =
            crate::contact::ContactService::migrate_tags_to_items(storage, &node_id, now)
        {
            eprintln!("[kernel] migrate ct:tags to items failed: {e}");
        }
        if let Err(e) =
            crate::contact::ContactService::migrate_groups_to_items(storage, &node_id, now)
        {
            eprintln!("[kernel] migrate ct:groups to items failed: {e}");
        }
    }

    /// P2 存量迁移：若 sled 尚无 `profile:self`，把身份文件资料写入 sled
    /// （bump pmeta）。幂等——sled 已有则跳过（避免覆盖 pdsync 已同步到
    /// sled 的更新版）。
    fn migrate_profile_to_sled(&mut self, file: &crate::identity::IdentityFile) {
        let key = super::PROFILE_SELF_KEY;
        let profile = super::SyncableProfile::from_options(
            file.nickname.as_deref(),
            file.avatar.as_deref(),
            file.gender.as_deref(),
            file.region.as_deref(),
            file.signature.as_deref(),
        );
        let now = crate::p2p::node::system_now_ms();
        let node_id = self.sync_node_id();
        let json = serde_json::to_string(&profile).unwrap_or_default();
        let Ok(storage) = self.require_storage_mut() else {
            return;
        };
        if storage.get(key).ok().flatten().is_some() {
            return; // sled 已有，跳过
        }
        let _ = crate::sync::put_personal(storage, &node_id, key, &json, now);
    }

    /// `lock`：锁定当前身份（活动指针不变）；会话私钥同步清除，P2P 一并停止
    ///（登出即离线，也避免切换用户后仍挂着旧身份的网络节点）。
    pub fn lock(&mut self) {
        let _ = self.stop_p2p();
        self.unlocked = None;
        *self.signing_key_shared.lock().unwrap() = None;
        *self.password_shared.lock().unwrap() = None;
        *self.seed_shared.lock().unwrap() = None;
        *self.nickname_shared.lock().unwrap() = String::new();
        *self.avatar_shared.lock().unwrap() = String::new();
        if let Ok(active) = self.read_active_root_id() {
            *self.current_root_id_shared.lock().unwrap() = active;
        }
    }

    /// `setActive`：切换登录目标用户——仅改活动指针，不解锁、不迁移存储；
    /// 下次 `unlock`（缺省 rootId）以新活动身份为目标（对齐 TS root-id.ts
    /// `setActive` 的"仅改指针，解锁时生效"语义）。
    pub fn set_active_identity(&self, root_id: &str) -> Result<()> {
        if self.read_identity_file(root_id)?.is_none() {
            return Err(KernelError::Internal("该账号不在本设备上".to_string()));
        }
        self.write_active_root_id(root_id)
    }

    /// `recoverFromMnemonic`：助记词恢复（最高权限，无需旧密码），
    /// 以新密码重新加密存储并解锁。中文连续书写/空格分隔、英文词表均可。
    pub fn recover_mnemonic(
        &mut self,
        mnemonic_input: &str,
        new_password: &str,
        nickname: &str,
        avatar: Option<&str>,
    ) -> Result<String> {
        check_password(new_password)?;
        let normalized = normalize_mnemonic_input(mnemonic_input);
        let (file, identity, key) =
            identity::recover_identity_and_key(&normalized, new_password, nickname, avatar).map_err(
                |e| match e {
                    identity::IdentityError::InvalidMnemonic(_) => KernelError::Internal(
                        "助记词校验失败：请检查是否有错别字、漏字或顺序错误".to_string(),
                    ),
                    other => KernelError::Identity(other),
                },
            )?;
        if self.read_identity_file(&file.root_id)?.is_some() {
            return Err(KernelError::Internal(
                "该账号已在本设备上，请直接登录".to_string(),
            ));
        }
        let seed = identity::parse_mnemonic(&normalized)?.seed;
        self.write_identity_file(&file)?;
        self.write_active_root_id(&file.root_id)?;
        self.align_storage(&file.root_id)?;
        // §13.9：助记词恢复与 QR 恢复同根，先确保设备身份就绪再 ack。
        self.ensure_p2p_identity_ready()?;
        let root_id = file.root_id.clone();
        self.set_unlocked(identity, seed, new_password, Some(key));
        if let Err(e) = self.on_unlock_password_ops(new_password) {
            log::error!("[recover-mnemonic] password ops failed: {e}");
        }
        self.ensure_p2p_after_login();
        Ok(root_id)
    }

    /// `getEncryptedBackupPayload`：导出加密备份载荷（QR 备份码内容），
    /// 即当前身份密文记录的紧凑 JSON；恢复时必须配合原登录密码。
    pub fn backup_payload(&self) -> Result<String> {
        let target = self.current_root_id()?.ok_or(KernelError::NotInitialized)?;
        let Some(file) = self.read_identity_file(&target)? else {
            return Err(KernelError::NotInitialized);
        };
        Ok(file.to_json()?)
    }

    /// `getQrBackupPayload`：二维码备份载荷。验密取出 payload，剔除 avatar
    /// （payload 内与文件外层）及其他可选大字段后同口令重新加密（新 salt/iv），
    /// 产出紧凑 IdentityFile JSON（QR 码容量约 3KB，完整文件备份载荷实测
    /// 远超上限无法扫码恢复）。完整文件备份见 `backup_payload`。
    ///
    /// P2P 已运行时额外附加本机节点名片（peerId + 监听地址），恢复端扫码
    /// 恢复身份后可据此自动完成设备配对，无需手动互相添加地址。
    pub fn backup_payload_qr(&self, password: &str) -> Result<String> {
        let target = self.current_root_id()?.ok_or(KernelError::NotInitialized)?;
        let Some(file) = self.read_identity_file(&target)? else {
            return Err(KernelError::NotInitialized);
        };
        let payload =
            identity::file::decrypt_payload(&file, password).map_err(map_identity_decrypt_error)?;
        let compact = identity::file::seal_compact_backup(&file, &payload, password)?;
        let compact_value: serde_json::Value =
            serde_json::to_value(&compact).map_err(|e| KernelError::Internal(e.to_string()))?;
        // QR-F1（§13.8）：把当前 `pwv:self`（若有）编入载荷顶层可选字段 `pwv`——
        // 使恢复设备 B 继承 A 的 V（而非恢复时新建分叉 V），从而 B 首次 unlock
        // 能验证并 ack A 的 V，完成 D′ 收敛。pwv 是明文豁免记录（无密钥），随
        // 载荷携带安全；无 pwv（老账号）则省略字段，恢复端走懒发布兜底。
        let pwv_value = self
            .require_storage()
            .ok()
            .and_then(|storage| pw::get_pwv(storage).ok().flatten())
            .and_then(|pwv| serde_json::to_value(&pwv).ok());
        // 附加本机 P2P 节点信息，便于恢复端扫码后自动完成设备配对
        if let Some(p2p_info) = self.p2p_status().ok().flatten() {
            if let Some(ref peer_id) = p2p_info.peer_id {
                if !p2p_info.addresses.is_empty() {
                    let mut obj = serde_json::json!({
                        "v": 1,
                        "i": compact_value,
                        "p": peer_id,
                        "a": p2p_info.addresses,
                    });
                    if let Some(pwv_value) = pwv_value {
                        obj["pwv"] = pwv_value;
                    }
                    return Ok(obj.to_string());
                }
            }
        }
        // 无 p2p 运行时（未建连）：恢复端无法自动配对、收敛本就不发生；且注入
        // pwv 会让无头像紧凑载荷逼近 QR 预算上限。保持紧凑身份 JSON 顶层原样
        // （旧版兼容），不注入 pwv。
        Ok(serde_json::to_string(&compact_value)
            .map_err(|e| KernelError::Internal(e.to_string()))?)
    }

    /// `recoverFromBackup`：备份码恢复。载荷即身份密文记录，解密口令为原登录密码；
    /// 结构无效与密码错误分别报错；写入前 sanitize 外部资料字段。
    ///
    /// 支持 v1 封装格式（`{"v":1,"i":"<IdentityFile JSON>","p":"<peerId>","a":[...]}`），
    /// 自动提取生成端 P2P 名片并完成设备配对。
    pub fn recover_backup(&mut self, payload_json: &str, password: &str) -> Result<String> {
        // 解包：v1 格式为 `{"v":1,"i":{<IdentityFile>},"p":"...","a":[...],"pwv":{...}}`，
        // "i" 是 JSON 对象（v1.1）或 JSON 字符串（v1.0 兼容）；"pwv" 为 QR-F1 注入的
        // 口令校验器（可选，旧版 QR 无此字段）。
        let (file_json, qr_peer, injected_pwv): (
            String,
            Option<(String, Vec<String>)>,
            Option<crate::pw::PasswordVerifier>,
        ) = {
            match serde_json::from_str::<serde_json::Value>(payload_json) {
                Ok(w) if w.get("v").and_then(|v| v.as_u64()) == Some(1) => {
                    let inner = match w.get("i") {
                        Some(serde_json::Value::Object(_)) => {
                            serde_json::to_string(w.get("i").unwrap())
                                .unwrap_or_else(|_| payload_json.to_string())
                        }
                        Some(serde_json::Value::String(s)) => s.clone(),
                        _ => payload_json.to_string(),
                    };
                    let pid = w.get("p").and_then(|v| v.as_str()).map(String::from);
                    let addrs: Option<Vec<String>> = w
                        .get("a")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|a| a.as_str().map(String::from))
                                .collect()
                        })
                        .filter(|v: &Vec<String>| !v.is_empty());
                    let peer = pid.zip(addrs);
                    // QR-F1 注入的 pwv：解析失败视为损坏载荷（fail-closed）。
                    let injected = w
                        .get("pwv")
                        .cloned()
                        .map(|v| {
                            serde_json::from_value::<crate::pw::PasswordVerifier>(v).map_err(|e| {
                                KernelError::Internal(format!("备份载荷 pwv 无效: {e}"))
                            })
                        })
                        .transpose()?;
                    (inner, peer, injected)
                }
                // 非 v1 封装（纯 IdentityFile JSON）也兼容：无 pwv 字段则 None。
                Ok(w) if w.get("pwv").is_some() => {
                    let injected = w
                        .get("pwv")
                        .cloned()
                        .map(|v| {
                            serde_json::from_value::<crate::pw::PasswordVerifier>(v).map_err(|e| {
                                KernelError::Internal(format!("备份载荷 pwv 无效: {e}"))
                            })
                        })
                        .transpose()?;
                    (payload_json.to_string(), None, injected)
                }
                Ok(_) | Err(_) => (payload_json.to_string(), None, None),
            }
        };

        let file = IdentityFile::from_json(&file_json)
            .map_err(|_| KernelError::Internal("备份数据无效或已损坏".to_string()))?;
        let (payload, identity, session_key) =
            identity::unlock_identity_and_key(&file, password).map_err(|e| match e {
                identity::IdentityError::DecryptionFailed => {
                    KernelError::Internal("密码不正确".to_string())
                }
                identity::IdentityError::InvalidMnemonic(_) | identity::IdentityError::Json(_) => {
                    KernelError::Internal("备份数据无效或已损坏".to_string())
                }
                other => KernelError::Identity(other),
            })?;
        if identity.id() != file.root_id {
            return Err(KernelError::Internal(
                "备份数据校验失败：rootId 不匹配".to_string(),
            ));
        }
        if self.read_identity_file(&file.root_id)?.is_some() {
            return Err(KernelError::Internal(
                "该账号已在本设备上，请直接登录".to_string(),
            ));
        }
        let seed = identity::parse_mnemonic(&payload.mnemonic)?.seed;
        // 备份载荷即身份记录本身；资料字段清洗后落库（非法值静默剔除）
        let (nickname, avatar) =
            identity::sanitize_profile(file.nickname.as_deref(), file.avatar.as_deref());
        let file = IdentityFile {
            nickname,
            avatar,
            ..file
        };
        self.write_identity_file(&file)?;
        self.write_active_root_id(&file.root_id)?;
        self.align_storage(&file.root_id)?;
        // §13.9：确保设备身份就绪——否则 `sync_node_id` 回退 "local-node"，
        // ack/自锚用错设备标识导致 A 侧无法按 B 真实 peerId 锚定（收敛链断）。
        self.ensure_p2p_identity_ready()?;
        // QR-F2（§13.8）：若载荷携带生成端 A 的 pwv，经守卫注入 `pwv:self`（不推进
        // appliedVTs——保持 0，使 B 首次 unlock 验证口令后能 ack A 的 V）。随后
        // `on_unlock_password_ops` 的 `maybe_ack_on_unlock` 会验证注入的 V 并自锚+ack，
        // 完成 D′ 收敛。守卫由 `pw::inject_pwv` 承担（未来 ts 拒收+水位单调）。
        if let Some(injected) = injected_pwv {
            let node_id = self.sync_node_id();
            let now_ms = crate::p2p::node::system_now_ms();
            let storage = self.require_storage_mut()?;
            pw::inject_pwv(storage.raw_mut(), &node_id, &injected, now_ms)?;
        }
        let root_id = file.root_id.clone();
        self.set_unlocked(identity, seed, password, session_key);
        if let Err(e) = self.on_unlock_password_ops(password) {
            log::error!("[recover-backup] password ops failed: {e}");
        }
        self.ensure_p2p_after_login();
        // 若载荷含生成端节点信息，落单向设备记录并尝试 friend-request
        if let Some((gen_peer_id, gen_addresses)) = qr_peer {
            self.recover_backup_pair_peer(&root_id, &gen_peer_id, &gen_addresses);
        }
        Ok(root_id)
    }

    /// QR 恢复后配对：落一条单向设备配对记录（覆盖网保活能找到生成端），
    /// 并尝试发 friend-request 完成双向配对——生成端在线则自动接受。
    fn recover_backup_pair_peer(
        &mut self,
        my_root_id: &str,
        gen_peer_id: &str,
        gen_addresses: &[String],
    ) {
        use crate::contact::{ContactService, FriendRecord};
        use crate::message::PeerRef;
        use crate::p2p::node::system_now_ms;
        use super::super::SendFriendRequestInput;

        let now = system_now_ms();
        let nickname = self.my_nickname(my_root_id);

        // 1. 查询已有设备记录（不存在则后续新建）。
        let existing = self
            .require_storage()
            .ok()
            .and_then(|s| ContactService::get_friend(s, my_root_id).ok())
            .flatten();

        // 2. 落单向设备配对记录，已有条目时更新地址。
        let node_id = self.sync_node_id();
        if let Ok(storage) = self.require_storage_mut() {
            let base = existing.unwrap_or_else(|| FriendRecord {
                root_id: my_root_id.to_string(),
                nickname,
                avatar: None,
                signature: String::new(),
                gender: None,
                added_at: now,
                peer: None,
                remark: String::new(),
                phones: Vec::new(),
                tag_ids: Vec::new(),
                group_id: String::new(),
                memo: "QR recovery paired device".to_string(),
                photos: Vec::new(),
                permission: "open".to_string(),
                blocked: false,
                updated_at: now,
            });
            let mut friend = base;
            // 写侧自指防护：自 FriendRecord 的 peer 是设备相对值（应指向生成端
            // 设备）。生成端 peerId 若 == 本机节点 id（`sync_node_id()`，p2p
            // 运行中即本机 peerId），即自指污染——拒绝落该 peer，保留原值/留空。
            let peer_id = gen_peer_id.to_string();
            if peer_id != node_id {
                friend.peer = Some(PeerRef {
                    peer_id,
                    addresses: gen_addresses.to_vec(),
                });
            } else {
                eprintln!(
                    "[login] self-pointing peer rejected on QR recover | node_id={node_id} gen_peer_id={gen_peer_id}"
                );
            }
            let _ = ContactService::upsert_friend_pdsync(storage, &friend, now, &node_id);
        }

        // 3. 发起 friend-request（含本机节点信息，对端自动接受完成双向配对）。
        let _ = self.contact_send_request(SendFriendRequestInput {
            id: format!(
                "qr-recover-{}-{}",
                now,
                &my_root_id[..my_root_id.len().min(8)]
            ),
            root_id: my_root_id.to_string(),
            raw: String::new(),
            peer_id: Some(gen_peer_id.to_string()),
            addresses: Some(gen_addresses.to_vec()),
            source: String::new(),
            message: String::new(),
        });
    }
}
