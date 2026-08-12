//! dm_e2e：DM 信封 body 的端到端加密模块（S2，纯逻辑层）。
//!
//! 对应 [wiki/architecture/plugins/social-feed.md §3/§4.1/§12-S2] 与
//! [wiki/protocol/p2p/p2p-dm.md §19.1.1]。
//!
//! ## 职责
//!
//! - **密钥协商**：X25519 DH（复用 orgsync 的 `ed_pk_to_x25519` /
//!   `ed_sk_to_x25519`）+ HKDF-SHA256（info 绑定 `"spark-dm-e2e-v1:{min}:{max}"`）
//!   派生 32B AES-256 会话密钥。**2026-08-11 架构师裁决：root 密钥直接转换**
//!   ——发送方用接收方 root 公钥（信封 `pubKey`，`ed_pk_to_x25519`）+ 自己
//!   root 私钥派生共享密钥，不使用域身份派生（dm-e2e 域身份导致对端公钥
//!   不可从 root 公钥推导）；
//! - **加解密**：AES-256-GCM，nonce 12B 随机，AAD 绑定 `{kind}:{from}:{to}:{ts}`；
//! - **密钥表管理**：会话密钥表 `dm:e2e:key:{peerRootId}`，历史密钥保留供
//!   离线密文解密，断联 24h 后重协商；记录含对端 root 公钥
//!   `peerRootPub`（入站验签时 [`record_inbound_peer_root_pub`] 写入、出站
//!   [`encrypt_outbound_body`] 读取转换）；
//! - **临时密钥对交换**（p2p-dm §19.1.1 密钥轮换）：发送方
//!   [`generate_ephemeral_keypair`] + [`derive_session_key_ephemeral`]，接收方
//!   [`derive_session_key_from_eph_pub`]；
//! - **自设备扩散**：密钥表记录经 pdsync（`put_personal` + `dm:e2e` category）
//!   共享到同一 rootId 的多台设备。
//!
//! ## 纪律
//!
//! 纯逻辑层：只操作 `StorageBackend` 泛型，`now_ms` 时间注入，不碰网络、
//! 不依赖 Tauri。加解密入口接收「本机 root 签名私钥」（`SigningKey`）与
//! 「对端 root 公钥 X25519」作为输入，私钥永不离开内核（对齐 orgsync
//! access.rs 与 identity:sign 的派生模式）。
//!
//! ## 方向性
//!
//! 会话密钥**方向无关**（2026-08-11 架构师裁决）：同一对 rootId 的 A→B 与
//! B→A 共用同一份会话密钥，HKDF info 用排序后的 from/to（字典序小在前）拼接。
//! 本模块经 `dm:e2e:key:{peerRootId}` 各存一份（值相同，各自经 pdsync 扩散到
//! 本方自设备）；feed 与 chat 通道共用这份方向无关的会话密钥。
//!
//! ## 密钥轮换：临时密钥对交换（X3DH 极简版）
//!
//! 纯 root DH 是确定性的——「24h 重协商」若只重跑 DH 不产生新密钥、无前向
//! 保密。轮换引入临时 X25519 密钥对，线形仅加一个可选信封外层字段 `ephPub`
//! （base64，32 字节临时公钥，与 `pubKey`/`sig` 并列且**参与签名**，防中间人
//! 替换临时公钥后注入已知会话）。
//!
//! 临时交换派生（双方均已升级，root 密钥直接转换）：
//! - 发送方 [`generate_ephemeral_keypair`] 生成临时密钥对，临时公钥随信封
//!   `ephPub` 携带；用「我方临时私钥 + 对端 root 公钥 X25519」经
//!   [`derive_session_key_ephemeral`] 派生；
//! - 接收方用「我方 root 私钥 + 对端 `ephPub`」经 [`derive_session_key_from_eph_pub`]
//!   派生；
//! - 二者 DH 相等（X25519 交换性），得到同一会话密钥。
//!
//! 回退规则（规格定死）：本期发送方总是生成临时密钥对并携带 `ephPub`。接收方
//! 总是用「我方 root 私钥 + 对端 `ephPub`」派生。仅当对端未升级（信封无
//! `ephPub`）时回退 root 直接转换 DH（[`derive_session_key`]，无前向保密但保持
//! 跨版本兼容）。两条路径产出的会话密钥都写入同一份会话密钥表（每对 peer
//! 一条，方向无关），后续历史密钥管理一致。
//!
//! **长期会话密钥恒等不轮换（2026-08-11 二轮修订）**：root DH 派生确定性，长期
//! 会话密钥恒等、不轮换，24h 重协商只重跑 DH 得到同值密钥、无安全增益，已去除。
//! 密钥表 `currentKey` 协商一次后原样复用，不再新增 `historyKeys` 条目（仅存量
//! 簿记）；`lastSeen` 记录**上次写盘时间**（非成功加解密），不触发任何轮换。
//! 前向保密由 per-message 临时密钥（信封 `ephPub`）承担。
//!
//! ## 接线契约（供 S6 feed 编排 + DM 链路 E2E 接线使用）
//!
//! 本模块只交付原语 + 密钥表管理，**不接收发链路**。S6 接线按此契约调用：
//!
//! - **出站加密（携带 ephPub）**：[`encrypt_outbound_body`] 一条龙完成——
//!   读密钥表对端 root 公钥 → X25519 转换 → `ensure_session_key`（无记录/占位
//!   时协商补全写盘一次，已有 current 则原样复用不写盘）→ [`generate_ephemeral_keypair`]
//!   生成临时密钥对 → [`derive_session_key_ephemeral`]（我方临时私钥 + 对端
//!   root 公钥 X25519）派生**本次临时会话密钥** → `encrypt_body_with_key` 用该
//!   临时密钥加密 → 返回 `(encrypted_body, eph_pub_b64)`，接线层
//!   `dm_envelope::build_envelope_with_eph` 携带 `ephPub`（临时公钥，参与签名）。
//!   **本次临时密钥不回写密钥表**（仅 current/history 存 root DH 派生的会话
//!   密钥）。
//! - **入站解密（验签后）**：信封携带 `ephPub` → `verify_envelope` 验签（含
//!   ephPub 参与签名）→ [`derive_session_key_from_eph_pub`]（我方 root 私钥 +
//!   对端 ephPub）派生临时会话密钥 → `decrypt_body_with_key` 用该临时密钥解密；
//!   同时入站验签通过后 [`record_inbound_peer_root_pub`] 把信封 `pubKey` 写入
//!   密钥表 `peerRootPub`（供本方后续出站 E2E 读取对端 root 公钥）。
//! - **回退**：入站信封无 `ephPub`（对端未升级）→ 用密钥表 current/history
//!   走 `decrypt_body`（root 直接转换 DH 会话密钥），回退路径在 S6 接线层判断。
//! - **先验签后解密**：`verify_envelope`（dm_envelope 层）验签不依赖解密；
//!   解密由 S6 用本地会话密钥完成，保持既有入站校验顺序（防篡改 + 防重放）。
//!
//! ## S6 显式密钥扩展
//!
//! [`encrypt_body`]/[`decrypt_body`] 当前按密钥表读 current/history。S6 接线
//! 扩展 [`encrypt_body_with_key`]/[`decrypt_body_with_key`] 支持**显式密钥
//! 传入**：出站用临时派生密钥、入站按信封 ephPub 派生的密钥，密钥表
//! current/history 路径保留给无 ephPub 的回退。
//!
//! ## S2 边界
//!
//! 本步骤只交付加解密原语 + 密钥表管理（纯逻辑 + 单测），不接 DM 收发链路
//! （出站加密、入站解密、先验签后解密顺序）——链路接线属 S6 前独立步骤。

mod crypto;
mod derive;
mod service;
mod types;

pub use crypto::{decrypt_body_with_key, encrypt_body_with_key};
pub use derive::{
    derive_session_key, derive_session_key_ephemeral, derive_session_key_from_eph_pub,
    generate_ephemeral_keypair, hkdf_sha256,
};
pub use service::{
    DmE2eError, decrypt_body, e2e_key_key, encrypt_body, encrypt_outbound_body, ensure_session_key,
    read_session_key_record, record_inbound_peer_root_pub, select_key_for_ts,
    write_session_key_record,
};
pub use types::{
    DM_E2E_DOMAIN, E2E_KEY_PREFIX, HISTORY_KEY_CAP, HKDF_INFO_PREFIX, HistoryKey,
    SessionKeyRecord,
};

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
