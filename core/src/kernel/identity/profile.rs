//! 资料与签名 API：密码门控助记词查看、资料更新（密码版/会话版）、根签名、
//! 域身份派生/签名、助记词录入校验与当前身份公开信息。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::Signer as _;

use super::{
    DerivedDomainIdentityInfo, DomainSignatureInfo, MnemonicCheckInfo, ProfileInfo, PublicIdentity,
    RootSignatureInfo, map_identity_decrypt_error, split_mnemonic_input,
};
use crate::identity;
use crate::kernel::Kernel;
use crate::kernel::error::{KernelError, Result};

impl Kernel {
    /// `revealMnemonic`：密码门控的助记词再次查看（解密当前身份文件）。
    pub fn reveal_mnemonic(&self, password: &str) -> Result<String> {
        let target = self.current_root_id()?.ok_or(KernelError::NotInitialized)?;
        let Some(file) = self.read_identity_file(&target)? else {
            return Err(KernelError::NotInitialized);
        };
        let payload =
            identity::file::decrypt_payload(&file, password).map_err(map_identity_decrypt_error)?;
        Ok(payload.mnemonic)
    }

    /// `updateProfile`：更新当前已解锁身份的资料（昵称/头像 + 扩展字段性别/地区/签名）。
    ///
    /// - `nickname`：`Some(n)` 修改；`None` 不变
    /// - `avatar`：`Some(Some(a))` 设置；`Some(None)` 清除（恢复自动头像）；`None` 不变
    /// - `gender`/`region`/`signature`：`Some(非空)` 设置；`Some("")` 清除；`None` 不变
    ///
    /// 内核身份文件把资料字段同时放在加密 payload 内（spec §5），故需要密码重新
    /// 封装；密码错误返回 `InvalidPassword`。
    pub fn update_profile(
        &mut self,
        password: &str,
        nickname: Option<&str>,
        avatar: Option<Option<&str>>,
        gender: Option<&str>,
        region: Option<&str>,
        signature: Option<&str>,
    ) -> Result<ProfileInfo> {
        let root_id = self.require_unlocked_root_id()?;
        let Some(mut file) = self.read_identity_file(&root_id)? else {
            return Err(KernelError::NotInitialized);
        };
        identity::update_profile(&mut file, password, nickname, avatar, gender, region, signature)
            .map_err(map_identity_decrypt_error)?;
        self.write_identity_file(&file)?;
        *self.nickname_shared.lock().unwrap() = file.nickname.clone().unwrap_or_default();
        *self.avatar_shared.lock().unwrap() = file.avatar.clone().unwrap_or_default();
        // pdsync P2：镜像到 sled `profile:self`（bump pmeta，供自设备同步）
        self.sync_profile_to_sled(
            file.nickname.as_deref(),
            file.avatar.as_deref(),
            file.gender.as_deref(),
            file.region.as_deref(),
            file.signature.as_deref(),
        );
        let nickname = self.my_nickname(&root_id);
        // 朋友互推（不含自记录）：展示字段
        self.broadcast_profile_to_friends(
            &nickname,
            file.avatar.as_deref(),
            file.gender.as_deref(),
            file.region.as_deref(),
            file.signature.as_deref(),
            file.updated_at,
        );
        // 自设备同步：完整资料（含隐私字段），仅配对设备间
        self.broadcast_profile_to_self_device(
            &nickname,
            file.avatar.as_deref(),
            file.gender.as_deref(),
            file.region.as_deref(),
            file.signature.as_deref(),
            file.updated_at,
        );
        Ok(ProfileInfo {
            nickname: file.nickname.clone(),
            avatar: file.avatar.clone(),
            gender: file.gender.clone(),
            region: file.region.clone(),
            signature: file.signature.clone(),
        })
    }

    /// `updateProfile` 的会话版（对齐 TS root-id.ts 现行语义：免密码——主进程持有
    /// 解锁会话直接重封资料）。内核按 spec §5 需重封加密 payload，口令取自 unlock
    /// 时缓存的会话态；`lock` 后调用报 `Locked`。
    ///
    /// 参数语义同 [`Kernel::update_profile`]。
    pub fn update_profile_session(
        &mut self,
        nickname: Option<&str>,
        avatar: Option<Option<&str>>,
        gender: Option<&str>,
        region: Option<&str>,
        signature: Option<&str>,
    ) -> Result<ProfileInfo> {
        let (root_id, password) = {
            let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
            (unlocked.root_id(), unlocked.password.clone())
        };
        let Some(mut file) = self.read_identity_file(&root_id)? else {
            return Err(KernelError::NotInitialized);
        };
        identity::update_profile(&mut file, &password, nickname, avatar, gender, region, signature)?;
        self.write_identity_file(&file)?;
        *self.nickname_shared.lock().unwrap() = file.nickname.clone().unwrap_or_default();
        *self.avatar_shared.lock().unwrap() = file.avatar.clone().unwrap_or_default();
        // pdsync P2：镜像到 sled `profile:self`（bump pmeta，供自设备同步）
        self.sync_profile_to_sled(
            file.nickname.as_deref(),
            file.avatar.as_deref(),
            file.gender.as_deref(),
            file.region.as_deref(),
            file.signature.as_deref(),
        );
        // 资料变更后分两个通道尽力推送（失败静默，不阻塞更新）：
        // - 朋友互推（不含自记录）：展示字段；
        // - 自设备同步：完整资料（含隐私字段），仅配对设备间。
        let nickname = self.my_nickname(&root_id);
        self.broadcast_profile_to_friends(
            &nickname,
            file.avatar.as_deref(),
            file.gender.as_deref(),
            file.region.as_deref(),
            file.signature.as_deref(),
            file.updated_at,
        );
        self.broadcast_profile_to_self_device(
            &nickname,
            file.avatar.as_deref(),
            file.gender.as_deref(),
            file.region.as_deref(),
            file.signature.as_deref(),
            file.updated_at,
        );
        Ok(ProfileInfo {
            nickname: file.nickname.clone(),
            avatar: file.avatar.clone(),
            gender: file.gender.clone(),
            region: file.region.clone(),
            signature: file.signature.clone(),
        })
    }

    /// `sign`（TS root-id.ts:725）：以当前已解锁身份的根私钥做 ed25519 签名。
    /// IPC 通道只传字符串，载荷按 UTF-8 字节取；签名 base64，payloadHash 为
    /// 载荷字节的 sha256 hex。
    pub fn sign(&self, payload: &str) -> Result<RootSignatureInfo> {
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        let signature = unlocked.identity.signing_key.sign(payload.as_bytes());
        Ok(RootSignatureInfo {
            root_id: unlocked.root_id(),
            signature: B64.encode(signature.to_bytes()),
            payload_hash: crate::evidence::sha256_hex(payload),
        })
    }

    /// `deriveDomainIdentity`（TS root-id.ts:759）：由会话缓存的 BIP39 种子派生
    /// 域身份（root 路径后追加 `/{idxA}'/{idxB}'`，索引取自 sha256(domain) 前 8
    /// 字节）。域密钥即时派生、不持久化（对齐 TS 安全说明）。
    pub fn derive_domain_identity(&self, domain: &str) -> Result<DerivedDomainIdentityInfo> {
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        if domain.trim().is_empty() {
            return Err(KernelError::Internal("Domain is required".to_string()));
        }
        let derived = identity::derive_domain_identity(&unlocked.seed, domain);
        Ok(DerivedDomainIdentityInfo {
            domain: domain.to_string(),
            domain_id: derived.id(),
            public_key: B64.encode(derived.public_key()),
            derivation_path: derived.path.clone(),
        })
    }

    /// `signWithDomainIdentity`（TS root-id.ts:777）：以域身份私钥做 ed25519
    /// 签名。域密钥由根种子即时派生、仅存在于本方法调用栈内（不持久化、不返回），
    /// 调用方只能拿到签名与公钥；根身份不暴露。
    pub fn sign_with_domain_identity(
        &self,
        domain: &str,
        payload: &str,
    ) -> Result<DomainSignatureInfo> {
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        if domain.trim().is_empty() {
            return Err(KernelError::Internal("Domain is required".to_string()));
        }
        let derived = identity::derive_domain_identity(&unlocked.seed, domain);
        let signature = derived.signing_key.sign(payload.as_bytes());
        Ok(DomainSignatureInfo {
            domain: domain.to_string(),
            domain_id: derived.id(),
            public_key: B64.encode(derived.public_key()),
            signature: B64.encode(signature.to_bytes()),
            payload_hash: crate::evidence::sha256_hex(payload),
        })
    }

    /// `root-mnemonic-check`（ipc/identity.ts:76-80）：录入助记词时逐词校验，
    /// 返回词数组与词表外词下标。纯函数，不需要身份态。
    pub fn check_mnemonic(input: &str) -> MnemonicCheckInfo {
        let words = split_mnemonic_input(input);
        let invalid_indexes = identity::find_invalid_mnemonic_words(&words);
        MnemonicCheckInfo {
            words,
            invalid_indexes,
        }
    }

    /// 当前已解锁身份的公开信息；锁定时返回 `Ok(None)`。
    pub fn current_identity(&self) -> Result<Option<PublicIdentity>> {
        let Some(unlocked) = &self.unlocked else {
            return Ok(None);
        };
        let root_id = unlocked.root_id();
        let file = self
            .read_identity_file(&root_id)?
            .ok_or(KernelError::NotInitialized)?;
        Ok(Some(PublicIdentity {
            public_key_hex: unlocked.identity.public_key_hex(),
            nickname: file.nickname.clone(),
            avatar: file.avatar.clone(),
            gender: file.gender.clone(),
            region: file.region.clone(),
            signature: file.signature.clone(),
            created_at: file.created_at,
            root_id,
        }))
    }
}
