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
        let (payload, identity, session_key) = identity::unlock_identity_and_key(&file, password)
            .map_err(map_identity_decrypt_error)?;
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
        // F7 存量迁移（org-invite-scope-fix §2.3）：org:invites 退出 orgsync——
        // 清空 org:inv:in:* 入站邀请记录（含 pmeta，自有/泄漏不可区分，恢复靠
        // 邀请人重发）+ 存量 org:invites 声明墓碑化。幂等；失败仅记录日志。
        // 走 raw 句柄：记录清除是数据治理而非同步写（不墓碑、不进 dlog）。
        {
            let now = crate::p2p::node::system_now_ms();
            if let Ok(storage) = self.require_storage_raw_mut()
                && let Err(e) =
                    crate::org::service::migrate_org_invites_out_of_orgsync(storage, now)
            {
                eprintln!("[kernel] migrate org:inv:in cleanup failed: {e}");
            }
        }
        // 阶段四A P1 存量迁移（org-member-split §2.4）：org:meta 成员段逐成员
        // 写 org:member: 条目（已存在跳过 = 幂等扫尾；不动 org:meta 的
        // members 段）。走**版本化句柄**（迁移产出本机 bump，首轮 orgsync 判
        // Concurrent 收敛，与 F7 raw 治理口径各自不同）；失败仅记录日志。
        if let Ok(storage) = self.require_storage_mut()
            && let Err(e) = crate::org::service::migrate_org_members_split(storage)
        {
            eprintln!("[kernel] migrate org:member split failed: {e}");
        }
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
            identity::recover_identity_and_key(&normalized, new_password, nickname, avatar)
                .map_err(|e| match e {
                    identity::IdentityError::InvalidMnemonic(_) => KernelError::Internal(
                        "助记词校验失败：请检查是否有错别字、漏字或顺序错误".to_string(),
                    ),
                    other => KernelError::Identity(other),
                })?;
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
        // 方案 Y 主从：助记词恢复与 QR 恢复同为「恢复加入方」——账号可能已在
        // 其他设备上存在主导 epoch。打本地标记使 `maybe_init_epoch_state` 跳过
        // init，被动等主导的 epoch:state + ikey（避免两端各自 init 生成不同密钥
        // → 互解不开 → Concurrent 风暴）。主导不存在时由 RECOVERED_ESCAPE_MS
        // 超时兜底自升（见 m3 设计 §5.11）。
        if let Ok(storage) = self.require_storage_mut() {
            let _ = storage.put(crate::epoch::RECOVERED_KEY, "1");
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

    /// `getQrBackupPayload`：二维码备份载荷。验密取出 payload + 派生身份，剔除
    /// avatar（payload 内与文件外层）及其他可选大字段后同口令重新加密（新
    /// salt/iv），产出紧凑 [`identity::file::CompactBackupFile`]（base64、无
    /// publicKeyHex，QR 码容量约 3KB，完整文件备份载荷实测远超上限无法扫码
    /// 恢复）。完整文件备份见 `backup_payload`。
    ///
    /// P2P 已运行时以 `{v:2,i,p,a,pwv?}` 封装并附加本机节点名片（peerId +
    /// 监听地址），恢复端扫码恢复身份后可据此自动完成设备配对。
    pub fn backup_payload_qr(&self, password: &str) -> Result<String> {
        let target = self.current_root_id()?.ok_or(KernelError::NotInitialized)?;
        let Some(file) = self.read_identity_file(&target)? else {
            return Err(KernelError::NotInitialized);
        };
        let (payload, identity) =
            identity::unlock_identity(&file, password).map_err(map_identity_decrypt_error)?;
        let compact = identity::file::seal_compact_backup(&file, &identity, &payload, password)?;
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
        // 附加本机 P2P 节点信息，便于恢复端扫码后自动完成设备配对。
        // P2P 运行时一律产出 v2 封装（即使地址被裁剪为空也保留 peerId/pwv）：
        // QR-F4 要求 pwv 注入与地址有无解耦——否则中继-only 设备（地址全被
        // 裁剪掉）会退回纯紧凑 JSON、丢 pwv，恢复端无法继承 V、D′ 收敛断裂。
        if let Some(p2p_info) = self.p2p_status().ok().flatten() {
            if let Some(ref peer_id) = p2p_info.peer_id {
                let mut obj = serde_json::json!({
                    "v": 2,
                    "i": compact_value,
                    "p": peer_id,
                    // 二维码备份专用地址裁剪：收敛到最小可拨子集（见 trim_qr_addresses），
                    // 控制 QR 版本与密度——完整监听地址含多条近百字符的中继电路地址，
                    // 全量携带会把载荷推高、密度过大难以扫码。
                    "a": Self::trim_qr_addresses(&p2p_info.addresses),
                });
                if let Some(pwv_value) = pwv_value {
                    obj["pwv"] = pwv_value;
                }
                return Ok(obj.to_string());
            }
        }
        // 无 p2p 运行时（未建连）：恢复端无法自动配对、收敛本就不发生；且注入
        // pwv 会让无头像紧凑载荷逼近 QR 预算上限。保持紧凑身份 JSON 顶层原样
        // （旧版兼容），不注入 pwv。
        Ok(serde_json::to_string(&compact_value)
            .map_err(|e| KernelError::Internal(e.to_string()))?)
    }

    /// 二维码备份载荷专用地址裁剪：把完整监听地址列表收敛成 QR 可承载的最小可拨子集。
    ///
    /// 完整 `LocalP2PNodeInfo::addresses`（`listen_addr_strings`）可能含：多条本机网卡
    /// IPv4/IPv6、external 地址、以及中继电路地址（`/ip4/<relayIP>/tcp/<port>/p2p/
    /// <52 字符 peerId>/p2p-circuit`，每条近百字符）。全量携带会显著拉高 QR 载荷体积
    /// → QR 版本升高、模块密度过大，手机摄像头难以对焦识别。
    ///
    /// 备份二维码通常在恢复设备与被备份设备物理相邻时扫描（同局域网），一条直连
    /// 局域网 IPv4 即可完成自动配对；中继地址既最长又最不可靠（依赖 relay 可达性）。
    /// 裁剪规则：
    /// - 剔除中继电路地址（含 `/p2p-circuit`）与通配/未解析地址（`/ip4/0.0.0.0`、`/ip6/::`）；
    /// - 排序：IPv4 直连优先（局域网/公网可拨）、同档按长度取短（体积敏感）；
    /// - 总量封顶 [`QR_MAX_ADDRS`]，超限取最短的若干条。
    ///
    /// 无直连地址时返回空列表——v1 封装仍带 `peerId`/`pwv`（QR-F4 不丢），恢复端
    /// 经 announce/DHT 兜底仍可寻回生成端。
    pub(crate) fn trim_qr_addresses(addresses: &[String]) -> Vec<String> {
        const QR_MAX_ADDRS: usize = 3;
        let mut kept: Vec<&str> = addresses
            .iter()
            .map(String::as_str)
            .filter(|a| {
                !a.contains("/p2p-circuit")
                    && !a.contains("/ip4/0.0.0.0")
                    && !a.contains("/ip6/::")
                    && !a.contains("/ip6/::1")
            })
            .collect();
        kept.sort_by(|a, b| {
            let a_ipv4 = a.starts_with("/ip4/");
            let b_ipv4 = b.starts_with("/ip4/");
            // IPv4 直连在前；同档内短地址在前（短 = 更大概率是可拨内网/公网直连）
            b_ipv4.cmp(&a_ipv4).then_with(|| a.len().cmp(&b.len()))
        });
        kept.truncate(QR_MAX_ADDRS);
        kept.into_iter().map(str::to_string).collect()
    }

    /// `recoverFromBackup`：备份码恢复。载荷即身份密文记录，解密口令为原登录密码；
    /// 结构无效与密码错误分别报错；写入前 sanitize 外部资料字段。
    ///
    /// 支持 v1（`i` = 磁盘 IdentityFile JSON，hex + publicKeyHex）与 v2（`i` =
    /// [`identity::file::CompactBackupFile`]，base64 + 无 publicKeyHex）两种封装格式，
    /// 按顶层 `v` 分派；恢复端重建磁盘 IdentityFile 时以派生公钥补全 publicKeyHex。
    /// 自动提取生成端 P2P 名片并完成设备配对。
    pub fn recover_backup(&mut self, payload_json: &str, password: &str) -> Result<String> {
        // 解包：按顶层 `v` 分派，产出磁盘 `IdentityFile` + 可选 peer/pwv。
        // - v1：`i` = 磁盘 IdentityFile JSON（hex + publicKeyHex），走 from_json；
        // - v2：`i` = 紧凑 `CompactBackupFile`（base64，无 publicKeyHex），
        //   decode 重建磁盘 IdentityFile（publicKeyHex 待解锁派生后补全）；
        // - 无 `v`：纯 IdentityFile JSON（旧版遗留形态）也兼容。
        // "i" 是 JSON 对象（v1.1）或 JSON 字符串（v1.0 兼容）；"pwv" 为 QR-F1
        // 注入的口令校验器（可选，旧版 QR 无此字段）。
        let (file, qr_peer, injected_pwv): (
            IdentityFile,
            Option<(String, Vec<String>)>,
            Option<crate::pw::PasswordVerifier>,
        ) = {
            let parse_err = |_e: &dyn std::fmt::Debug| {
                KernelError::Internal("备份数据无效或已损坏".to_string())
            };
            let peer_and_pwv = |w: &serde_json::Value| -> Result<(
                Option<(String, Vec<String>)>,
                Option<crate::pw::PasswordVerifier>,
            )> {
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
                        serde_json::from_value::<crate::pw::PasswordVerifier>(v)
                            .map_err(|e| KernelError::Internal(format!("备份载荷 pwv 无效: {e}")))
                    })
                    .transpose()?;
                Ok((peer, injected))
            };
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
                    let file = IdentityFile::from_json(&inner).map_err(|e| parse_err(&e))?;
                    let (peer, injected) = peer_and_pwv(&w)?;
                    (file, peer, injected)
                }
                // 新紧凑格式：i = CompactBackupFile（base64，无 publicKeyHex）。
                Ok(w) if w.get("v").and_then(|v| v.as_u64()) == Some(2) => {
                    let inner = match w.get("i") {
                        Some(serde_json::Value::Object(_)) => {
                            serde_json::to_string(w.get("i").unwrap())
                                .unwrap_or_else(|_| payload_json.to_string())
                        }
                        Some(serde_json::Value::String(s)) => s.clone(),
                        _ => payload_json.to_string(),
                    };
                    let compact: identity::file::CompactBackupFile =
                        serde_json::from_str(&inner).map_err(|e| parse_err(&e))?;
                    let file = identity::file::decode_compact_backup(&compact)
                        .map_err(|e| KernelError::Internal(format!("备份数据无效或已损坏: {e}")))?;
                    let (peer, injected) = peer_and_pwv(&w)?;
                    (file, peer, injected)
                }
                // 非 v1/v2 封装（纯 IdentityFile JSON）也兼容：无 pwv 字段则 None。
                Ok(w) if w.get("pwv").is_some() => {
                    let file = IdentityFile::from_json(payload_json).map_err(|e| parse_err(&e))?;
                    let (_, injected) = peer_and_pwv(&w)?;
                    (file, None, injected)
                }
                Ok(_) | Err(_) => {
                    let file = IdentityFile::from_json(payload_json).map_err(|e| parse_err(&e))?;
                    (file, None, None)
                }
            }
        };

        let (payload, identity, session_key) = identity::unlock_identity_and_key(&file, password)
            .map_err(|e| match e {
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
        // 用派生公钥补全磁盘必填 publicKeyHex（v2 紧凑码不带它；v1 幂等）。
        let file = IdentityFile {
            public_key_hex: identity.public_key_hex(),
            ..file
        };
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
        // 方案 Y 主从：本机是恢复加入方（扫描备份二维码加入），打本地标记。
        // 后续 p2p 启动的 `maybe_init_epoch_state` 据此跳过 init，被动等主导设备
        // 的 epoch:state + ikey（避免两端各自 init 生成不同密钥 → 互解不开 → 风暴）。
        if let Ok(storage) = self.require_storage_mut() {
            let _ = storage.put(crate::epoch::RECOVERED_KEY, "1");
        }
        self.ensure_p2p_after_login();
        // 若载荷含生成端节点信息，落单向设备记录并尝试 friend-request
        if let Some((gen_peer_id, gen_addresses)) = qr_peer {
            self.recover_backup_pair_peer(&root_id, &gen_peer_id, &gen_addresses);
        }
        Ok(root_id)
    }

    /// 开发/自动化测试挂钩（仅 debug 构建）：等价 QR 恢复配对步骤——落单向
    /// 设备记录并向目标 peer 发 friend-request（同账号自设备自动接受）。
    /// 供 dev_harness / 双端联调脚本免扫码完成设备配对。
    #[cfg(debug_assertions)]
    pub fn dev_pair_peer(&mut self, peer_id: &str, addresses: &[String]) -> Result<()> {
        let root_id = self.require_unlocked_root_id()?;
        self.recover_backup_pair_peer(&root_id, peer_id, addresses);
        Ok(())
    }

    /// QR 恢复后配对：落一条单向设备配对记录（覆盖网保活能找到生成端），
    /// 并尝试发 friend-request 完成双向配对——生成端在线则自动接受。
    fn recover_backup_pair_peer(
        &mut self,
        my_root_id: &str,
        gen_peer_id: &str,
        gen_addresses: &[String],
    ) {
        use super::super::SendFriendRequestInput;
        use crate::contact::{ContactService, FriendRecord};
        use crate::message::PeerRef;
        use crate::p2p::node::system_now_ms;

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
                peers: Vec::new(),
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
                // 多设备寻址：QR 恢复配对写入首台设备（已有同 peerId 不重复）
                if !friend.peers.iter().any(|p| p.peer_id == peer_id) {
                    friend.peers.push(PeerRef {
                        peer_id,
                        addresses: gen_addresses.to_vec(),
                        ..Default::default()
                    });
                }
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

#[cfg(test)]
mod tests {
    use super::Kernel;

    fn full_addresses() -> Vec<String> {
        [
            // 中继电路地址——必须剔除（最长且不可靠，实测每条近百字符）
            "/ip4/198.51.100.7/tcp/4001/p2p/12D3KooWAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/p2p-circuit"
                .to_string(),
            "/ip4/198.51.100.9/tcp/4001/p2p/12D3KooWBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB/p2p-circuit"
                .to_string(),
            // 通配/回环通配地址——不可拨，剔除
            "/ip4/0.0.0.0/tcp/4567".to_string(),
            "/ip6/::/tcp/4567".to_string(),
            "/ip6/::1/tcp/4567".to_string(),
            // 可直连地址
            "/ip4/127.0.0.1/tcp/4567".to_string(),
            "/ip4/192.168.1.23/tcp/4567".to_string(),
            "/ip6/240e:390:c901:0::1/tcp/4567".to_string(),
        ]
        .to_vec()
    }

    #[test]
    fn trim_qr_addresses_drops_relay_wildcard_and_caps() {
        let trimmed = Kernel::trim_qr_addresses(&full_addresses());
        assert!(
            trimmed.iter().all(|a| !a.contains("/p2p-circuit")),
            "不得保留中继电路地址：{trimmed:?}"
        );
        assert!(
            trimmed
                .iter()
                .all(|a| !a.contains("0.0.0.0") && !a.contains("/ip6::")),
            "不得保留通配地址：{trimmed:?}"
        );
        assert!(
            trimmed.len() <= 3,
            "封顶 3 条，实际 {}：{trimmed:?}",
            trimmed.len()
        );
        // IPv4 直连优先，且同档内短地址在前
        assert!(
            trimmed[0].starts_with("/ip4/"),
            "IPv4 直连优先：{trimmed:?}"
        );
        // 空 / 全中继 / 全通配输入不 panic，返回空（v1 封装仍带 peerId/pwv）
        assert!(Kernel::trim_qr_addresses(&[]).is_empty());
        assert!(
            Kernel::trim_qr_addresses(&full_addresses()[..2].to_vec()).is_empty(),
            "仅中继地址裁剪为空"
        );
        assert!(Kernel::trim_qr_addresses(&["/ip4/0.0.0.0/tcp/1".to_string()].to_vec()).is_empty());
    }
}
