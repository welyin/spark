//! dm_e2e 密钥派生原语：X25519 DH + HKDF-SHA256 + 临时密钥对。
//!
//! 对齐 p2p-dm §19.1.1 与 social-feed §4.1。本文件纯逻辑（密码学原语），
//! 不触碰存储/信封装配。**root 密钥直接转换**（2026-08-11 架构师裁决）：
//! 用「本机 root 私钥 + 对端 root 公钥 X25519」X25519 DH + HKDF 派生 32B
//! AES-256 会话密钥，不使用域身份派生。
//!
//! ## 方向无关（2026-08-11 架构师裁决）
//!
//! HKDF info 用**排序后**的 from/to（字典序小在前）拼接，A→B 与 B→A 共用
//! 同一份会话密钥（密钥表每对 peer 一条记录）。feed 与 chat 通道共用这份
//! 方向无关的会话密钥。

use aes_gcm::KeyInit;
use curve25519_dalek::montgomery::MontgomeryPoint;
use ed25519_dalek::SigningKey;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;

use super::service::{DmE2eError, Result};
use super::types::HKDF_INFO_PREFIX;
use crate::sync::orgsync::ed_sk_to_x25519;

type HmacSha256 = Hmac<Sha256>;

/// HKDF-SHA256（RFC 5869）：`extract(salt=0^32, IKM=shared) + expand` 派生 32B
/// 输出。info 为 `{HKDF_INFO_PREFIX}{from}:{to}`。
///
/// 用项目既有 `hmac`/`sha2` 手写实现（RFC 5869 标准，零新依赖）：
/// - extract：`PRK = HMAC(salt, IKM)`，salt 取全零 32B（RFC 5869 默认无 salt）；
/// - expand：`T(0)=∅`，`T(i)=HMAC(PRK, T(i-1) || info || i)`；32B 输出只需
///   `L=ceil(32/32)=1`，即 `OKM = T(1) = HMAC(PRK, info || 0x01)`。
pub fn hkdf_sha256(shared: &[u8; 32], info: &str) -> [u8; 32] {
    // extract：salt 全零
    let salt = [0u8; 32];
    let mut extract_mac = HmacSha256::new_from_slice(&salt).expect("hmac accepts 32B salt");
    extract_mac.update(shared);
    let prk = extract_mac.finalize().into_bytes();

    // expand：T(1) = HMAC(PRK, info || 0x01)
    let mut expand_mac = HmacSha256::new_from_slice(&prk).expect("hmac accepts 32B prk");
    expand_mac.update(info.as_bytes());
    expand_mac.update(&[0x01u8]);
    let okm = expand_mac.finalize().into_bytes();
    okm.into()
}

/// 由共享密钥经 HKDF 派生 32B AES-256 会话密钥（**方向无关**）。
///
/// 公共收尾：拒低阶点（全零共享）→ HKDF（info 用排序后的 from/to 拼接，
/// 字典序小在前，A→B 与 B→A 派生同一份会话密钥）。root 直接转换 DH 与临时
/// 交换两条派生路径共用本函数，保证两种路径产出的会话密钥写入同一份密钥
/// 表、管理一致（p2p-dm §19.1.1 密钥轮换）。
fn derive_from_shared(shared_bytes: [u8; 32], from: &str, to: &str) -> Result<[u8; 32]> {
    // H1a：拒低阶点（X25519 低阶点乘积为全零/低阶共享，密钥可预测）
    if shared_bytes.iter().all(|&b| b == 0) {
        return Err(DmE2eError::LowOrderShared);
    }
    // 方向无关：字典序小者在前拼接（A→B 与 B→A 共用同一份会话密钥）。
    let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
    let info = format!("{HKDF_INFO_PREFIX}{lo}:{hi}");
    let key = hkdf_sha256(&shared_bytes, &info);
    // H1a 双保险：恒零派生密钥拒
    if key.iter().all(|&b| b == 0) {
        return Err(DmE2eError::LowOrderShared);
    }
    Ok(key)
}

/// 密钥协商（**root 密钥直接转换**，2026-08-11 架构师裁决）：X25519 DH +
/// HKDF 派生 32B AES-256 会话密钥。
///
/// - `my_signing_key`：本机 **root** 签名私钥（`SigningKey`，解锁态持有，
///   私钥不离开内核）→ X25519 私钥（`ed_sk_to_x25519(signing_key.to_bytes())`）；
/// - `peer_x25519_pub`：对端 **root 公钥**的 X25519 形式
///   （`ed_pk_to_x25519(peer_root_pub_bytes)`，由接线层从密钥表
///   `peer_root_pub` 读取并转换）；
/// - `from`/`to`：信封方向。**方向无关**——HKDF info 用排序后的 from/to
///   （字典序小在前）拼接，A→B 与 B→A 派生同一会话密钥（防跨对端密钥
///   混淆，同时让同对 rootId 双向共用一份密钥）。
///
/// 不使用域身份派生（dm-e2e 域身份导致对端公钥不可从 root 公钥推导）。
/// 双端各自用「本机 root 私钥」与「对端 root 公钥」计算，DH 共享密钥对称，
/// 得到同一会话密钥。该路径用于对端未升级（信封无 ephPub）时的回退。
pub fn derive_session_key(
    my_signing_key: &SigningKey,
    peer_x25519_pub: &[u8; 32],
    from: &str,
    to: &str,
) -> Result<[u8; 32]> {
    let my_x25519 = ed_sk_to_x25519(&my_signing_key.to_bytes());
    let shared = MontgomeryPoint(*peer_x25519_pub).mul_clamped(my_x25519);
    derive_from_shared(shared.to_bytes(), from, to)
}

/// 生成临时 X25519 密钥对（发送方临时交换，p2p-dm §19.1.1 密钥轮换）。
///
/// 返回 `(priv, pub)`：
/// - `priv`：随机 32 字节经 clamp 的 X25519 私钥标量（与
///   [`crate::sync::orgsync::ed_sk_to_x25519`] 同口径 clamp，非域身份派生，
///   一次性临时私钥）；
/// - `pub`：对应 X25519 临时公钥（Montgomery u 坐标，32 字节），随信封外层
///   可选字段 `ephPub` 携带。
///
/// 临时私钥**不落盘、不扩散**（防泄露降级），仅存活于一次会话派生的内存中。
pub fn generate_ephemeral_keypair() -> ([u8; 32], [u8; 32]) {
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    let priv_scalar = ed_sk_to_x25519(&seed);
    let pub_point = MontgomeryPoint::mul_base_clamped(priv_scalar);
    (priv_scalar, pub_point.to_bytes())
}

/// 发送方临时交换派生（p2p-dm §19.1.1 密钥轮换）：**我方临时私钥** +
/// 对端 root 公钥 X25519 → 共享密钥 → HKDF 派生会话密钥。
///
/// - `eph_priv`：[`generate_ephemeral_keypair`] 返回的临时私钥标量；
/// - `peer_x25519_pub`：对端 root 公钥的 X25519 形式
///   （`ed_pk_to_x25519(peer_root_pub_bytes)`，接线层从密钥表转换）。
/// - `from`/`to`：信封方向（HKDF info 方向无关排序）。
///
/// 与接收方 [`derive_session_key_from_eph_pub`] 的 DH 相等（X25519 交换性），
/// 两路径派生同一会话密钥。
pub fn derive_session_key_ephemeral(
    eph_priv: &[u8; 32],
    peer_x25519_pub: &[u8; 32],
    from: &str,
    to: &str,
) -> Result<[u8; 32]> {
    let shared = MontgomeryPoint(*peer_x25519_pub).mul_clamped(*eph_priv);
    derive_from_shared(shared.to_bytes(), from, to)
}

/// 接收方临时交换派生（p2p-dm §19.1.1 密钥轮换）：**我方 root 私钥** +
/// 对端 `ephPub` → 共享密钥 → HKDF 派生会话密钥。
///
/// - `my_signing_key`：本机 **root** 签名私钥（`SigningKey`）→ X25519 私钥；
/// - `eph_pub`：对端信封外层 `ephPub` 字段（临时 X25519 公钥，32 字节
///   Montgomery）。
///
/// 与发送方 [`derive_session_key_ephemeral`] 的 DH 相等（X25519 交换性），
/// 两路径派生同一会话密钥。
pub fn derive_session_key_from_eph_pub(
    my_signing_key: &SigningKey,
    eph_pub: &[u8; 32],
    from: &str,
    to: &str,
) -> Result<[u8; 32]> {
    let my_x25519 = ed_sk_to_x25519(&my_signing_key.to_bytes());
    let shared = MontgomeryPoint(*eph_pub).mul_clamped(my_x25519);
    derive_from_shared(shared.to_bytes(), from, to)
}
