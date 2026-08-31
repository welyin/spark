//! 自设备资料快照的应用：profile-sync 全量快照的 LWW 三向裁决并回写身份
//! 文件（[`KernelDmHandler::apply_self_profile`]），以及 pdsync 合入
//! `profile:self` 后的 sled → 身份文件回写
//! （[`KernelDmHandler::apply_profile_from_sled`]）。两侧成功都会刷新
//! 昵称/头像共享格并向前端发 `SelfProfileSynced`。

use serde_json::Value;

use super::KernelDmHandler;
use crate::storage::StorageBackend;

impl KernelDmHandler {
    /// 自设备 profile-sync 全量快照应用：以会话口令重封身份文件，完成
    /// 「我的资料」跨设备同步（wiki/design/sync-and-evidence.md「个人空间
    /// 同步口径」：个人资料在个人设备间全量同步；identity.md §5「恢复后
    /// 头像经 profile-sync 找回」）。
    ///
    /// LWW 三向裁决（向量时钟为后续专项）：
    /// - 对端较新（`updatedAt` 严格大于本地）：应用快照，刷新共享槽并通知
    ///   前端（SelfProfileSynced）——防离线设备上线后以旧快照回灌；
    /// - 本机较新（严格小于）：返回 true，调用方据此向对端回发本机全量
    ///   快照（握手式交换——对端较旧/残缺时补齐，如 QR 恢复的新设备
    ///   updatedAt=0，其残缺快照不会覆盖本机资料，本机回发使其收敛）；
    /// - 相等：收敛态，不动（也不回发，无 ping-pong）。
    ///
    /// 身份已锁（无口令）/文件缺失/校验失败时静默跳过（返回 false），
    /// 不影响朋友记录已完成的更新。
    pub(super) fn apply_self_profile(&self, root_id: &str, body: &Value) -> bool {
        let password = self
            .password_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(password) = password else {
            return false;
        };
        let Some(updated_at) = body.get("updatedAt").and_then(Value::as_i64) else {
            return false;
        };
        let path = self
            .data_dir
            .join("identities")
            .join(format!("{root_id}.json"));
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return false;
        };
        let Ok(mut file) = crate::identity::IdentityFile::from_json(&raw) else {
            return false;
        };
        let has_session = self
            .password_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        log::info!(
            "[PROFILE_CHAIN] apply_self_profile | incoming.updatedAt={} file.updated_at={} has_password={} sig={:?}",
            updated_at,
            file.updated_at,
            has_session,
            body.get("signature"),
        );
        if updated_at < file.updated_at as i64 {
            // 本机资料较新：提示调用方回发本机快照补齐对端
            return true;
        }
        if updated_at == file.updated_at as i64 {
            return false;
        }
        // 线形三态 → update_profile 参数三态：字符串=设置，显式 null=清除，缺省=不变
        let nickname = body.get("nickname").and_then(Value::as_str);
        let tri_state = |key: &str| -> Option<Option<&str>> {
            match body.get(key) {
                Some(Value::Null) => Some(None),
                Some(Value::String(s)) => Some(Some(s.as_str())),
                _ => None,
            }
        };
        let avatar = tri_state("avatar");
        // gender/region/signature 的内核清除语义是 Some("")（空串=清除）
        let extra = |key: &str| -> Option<&str> {
            match body.get(key) {
                Some(Value::Null) => Some(""),
                Some(Value::String(s)) => Some(s.as_str()),
                _ => None,
            }
        };
        let file_updated_at_before = file.updated_at;
        if crate::identity::update_profile(
            &mut file,
            &password,
            nickname,
            avatar,
            extra("gender"),
            extra("region"),
            extra("signature"),
        )
        .is_err()
        {
            return false;
        }
        // 时钟收敛：内容一致（patch 未推进 updatedAt）而对端时间戳较新时，
        // 采纳对端时间戳。否则双方 updatedAt 永不相等，「较新方回发本机快照」
        // 的握手语义会无限互发（真机连接态 100% CPU 根因之二；内容差异本身
        // 一轮即可收敛，残余循环纯由时钟不等驱动）。
        if file.updated_at == file_updated_at_before
            && updated_at > 0
            && (updated_at as u64) > file.updated_at
        {
            file.updated_at = updated_at as u64;
        }
        let Ok(text) = serde_json::to_string_pretty(&file) else {
            return false;
        };
        if crate::kernel::identity::write_identity_file_atomic(&path, &text).is_err() {
            return false;
        }
        // 共享格刷新（dm 应答/出站口径）+ 前端通知
        *self.nickname_shared.lock().unwrap_or_else(|e| e.into_inner()) =
            file.nickname.clone().unwrap_or_default();
        *self.avatar_shared.lock().unwrap_or_else(|e| e.into_inner()) =
            file.avatar.clone().unwrap_or_default();
        let mut data = serde_json::json!({
            "nickname": file.nickname.clone().unwrap_or_default(),
        });
        if let Some(a) = &file.avatar {
            data["avatar"] = Value::from(a.clone());
        }
        let _ = self.event_tx.send(crate::p2p::P2pEvent::SelfProfileSynced(data));
        false
    }

    /// pdsync 合入 `profile:self` 后回写身份文件（P2）。
    ///
    /// sled 镜像已由 `handle_pdsync_data` LWW 落地；这里把 sled 的最新资料
    /// 同步回身份文件（保证两处一致）。仅解锁态（有口令）可重封身份文件；
    /// 锁定态跳过——下次 unlock 时以 sled 覆盖（见 login）。写失败静默。
    pub(super) fn apply_profile_from_sled(&self, root_id: &str) {
        let Some(password) = self
            .password_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        // 读 sled profile:self
        let Some(raw) = self
            .storage
            .get(crate::kernel::identity::PROFILE_SELF_KEY)
            .ok()
            .flatten()
        else {
            return;
        };
        let Ok(profile) = serde_json::from_str::<crate::kernel::identity::SyncableProfile>(&raw) else {
            return;
        };
        let info = profile.to_profile_info();
        let path = self
            .data_dir
            .join("identities")
            .join(format!("{root_id}.json"));
        let Ok(raw_file) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(mut file) = crate::identity::IdentityFile::from_json(&raw_file) else {
            return;
        };
        let file_ts_before = file.updated_at;
        let sled_ts = crate::sync::personal::get_personal_meta(
            &self.storage,
            crate::kernel::identity::PROFILE_SELF_KEY,
        )
        .ok()
        .flatten()
        .map(|m| m.ts);
        // 以 sled 为源（pdsync 已 LWW 裁决，sled profile:self 是本次合入胜者），
        // 回写身份文件资料字段。
        if crate::identity::update_profile(
            &mut file,
            &password,
            info.nickname.as_deref(),
            match info.avatar.as_deref() {
                Some(a) if !a.is_empty() => Some(Some(a)),
                _ => Some(None),
            },
            Some(info.gender.as_deref().unwrap_or("")),
            Some(info.region.as_deref().unwrap_or("")),
            Some(info.signature.as_deref().unwrap_or("")),
        )
        .is_err()
        {
            return;
        }
        // 时钟收敛（同 apply_self_profile）：内容未变（patch 未推进 updatedAt）
        // 而 sled 胜者时间戳较新时采纳之，防双方 updatedAt 永不相等导致的
        // profile-sync 无限互发。
        if file.updated_at == file_ts_before
            && let Some(ts) = sled_ts
            && ts > 0
            && (ts as u64) > file.updated_at
        {
            file.updated_at = ts as u64;
        }
        log::info!(
            "[PROFILE_CHAIN] apply_profile_from_sled | file.updated_at {} -> {} | sled pmeta.ts={:?} | sig={:?}",
            file_ts_before,
            file.updated_at,
            sled_ts,
            info.signature,
        );
        let Ok(text) = serde_json::to_string_pretty(&file) else {
            return;
        };
        if crate::kernel::identity::write_identity_file_atomic(&path, &text).is_err() {
            return;
        }
        *self.nickname_shared.lock().unwrap_or_else(|e| e.into_inner()) =
            file.nickname.clone().unwrap_or_default();
        *self.avatar_shared.lock().unwrap_or_else(|e| e.into_inner()) =
            file.avatar.clone().unwrap_or_default();
        // 前端通知（与 apply_self_profile 同口径）：我的资料已被自设备同步更新
        let mut data = serde_json::json!({
            "nickname": file.nickname.clone().unwrap_or_default(),
        });
        if let Some(a) = &file.avatar {
            data["avatar"] = Value::from(a.clone());
        }
        let _ = self.event_tx.send(crate::p2p::P2pEvent::SelfProfileSynced(data));
    }
}
