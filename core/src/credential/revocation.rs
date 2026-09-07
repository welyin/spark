//! 注销列表：验证人 append-only 签名链 + 头承诺（「未注销」证明）验证
//! （credential §3；链语义 = sync-evidence §2 迷你版，作用域 = issuer）。
//!
//! 注销只影响新操作：已签发凭证的静态有效性（结构/签名）不因注销改变——
//! 消费方对历史行为的核验不应经过本文件的检查（见 credential.rs
//! [`verify_credential_static`](crate::credential::verify_credential_static) 注释）。

use super::credential::{
    canonical_sans, identity_from_public_key, is_fresh, is_valid_hash, is_valid_identity_id,
    sha256_hex_str,
};
use super::error::{CredentialError, Result};
use super::types::{RevocationEntry, RevocationHead};
use crate::identity::verify_ed25519_signature;

/// 注销条目签名载荷：`canonical(条目剔除 sig)`。
pub fn revocation_sign_payload(entry: &RevocationEntry) -> Result<String> {
    canonical_sans(entry, "sig")
}

/// `entryHash = sha256hex(normalizeObject(条目剔除 sig))`。
pub fn revocation_entry_hash(entry: &RevocationEntry) -> Result<String> {
    Ok(sha256_hex_str(&revocation_sign_payload(entry)?))
}

/// 头承诺签名载荷：`canonical(头剔除 sig)`。
pub fn revocation_head_payload(head: &RevocationHead) -> Result<String> {
    canonical_sans(head, "sig")
}

/// 链式校验（§3.1/§3.2）：`seq` 从 1 递增、`prevHash` 链接、逐条重算 entryHash
/// 并验签。作用域 = issuer：全部条目须属同一验证人，`issuer_pubkey` 由调用方
/// 经可信途径取得（呈现场景取凭证 issuer.publicKey——已被凭证签名与
/// identity 绑定钉死）。
///
/// 空链合法（无任何注销），返回 Ok。
pub fn verify_revocation_chain(entries: &[RevocationEntry], issuer_pubkey: &str) -> Result<()> {
    let mut expected_seq = 1u64;
    let mut expected_prev: Option<String> = None;
    let mut expected_issuer: Option<&str> = None;
    for entry in entries {
        if entry.rev_v != 1 {
            return Err(CredentialError::RevocationChainBroken("revV must be 1"));
        }
        if let Some(scope) = expected_issuer {
            if entry.issuer != scope {
                return Err(CredentialError::RevocationChainBroken("issuer scope mixed"));
            }
        } else {
            if !is_valid_identity_id(&entry.issuer) {
                return Err(CredentialError::RevocationChainBroken("issuer shape"));
            }
            expected_issuer = Some(&entry.issuer);
        }
        if entry.seq != expected_seq {
            return Err(CredentialError::RevocationChainBroken(
                "seq not incremental",
            ));
        }
        if entry.prev_hash != expected_prev {
            return Err(CredentialError::RevocationChainBroken(
                "prevHash link broken",
            ));
        }
        if !is_valid_hash(&entry.cred_id) {
            return Err(CredentialError::RevocationChainBroken("credId shape"));
        }
        if !verify_ed25519_signature(&revocation_sign_payload(entry)?, &entry.sig, issuer_pubkey) {
            return Err(CredentialError::InvalidSignature);
        }
        expected_prev = Some(revocation_entry_hash(entry)?);
        expected_seq += 1;
    }
    Ok(())
}

/// 头承诺校验（§3.2）：验签 + 与全量链一致（`headSeq == 条数`、`headHash ==
/// 末条 entryHash`）。空链（headSeq == 0）的头形态规格未定义，fail-closed 拒绝。
pub fn verify_revocation_head(
    head: &RevocationHead,
    entries: &[RevocationEntry],
    issuer_pubkey: &str,
    now_ms: i64,
) -> Result<()> {
    if head.rev_head_v != 1 {
        return Err(CredentialError::RevocationHeadInvalid);
    }
    // asOf 新鲜度：持旧头属正常滞后，消费方如需放宽应自行取舍后绕过本函数；
    // 默认 fail-closed 按总约 ±10 min。
    if !is_fresh(head.as_of, now_ms) {
        return Err(CredentialError::StaleTimestamp);
    }
    if !verify_ed25519_signature(&revocation_head_payload(head)?, &head.sig, issuer_pubkey) {
        return Err(CredentialError::InvalidSignature);
    }
    if head.head_seq == 0 || head.head_seq != entries.len() as u64 {
        return Err(CredentialError::RevocationHeadInvalid);
    }
    if let Some(scope) = entries.first() {
        if head.issuer != scope.issuer {
            return Err(CredentialError::RevocationHeadInvalid);
        }
    }
    let tail_hash = revocation_entry_hash(&entries[entries.len() - 1])?;
    if head.head_hash != tail_hash {
        return Err(CredentialError::RevocationHeadInvalid);
    }
    Ok(())
}

/// 「未注销」证明（§3.2 + §6 第 5 步）：头承诺 + `seq = 1..=headSeq` 全量链
/// 逐条复算，且链中不含 `cred_id`。任一环节缺失/不符即失败（fail-closed）。
pub fn verify_not_revoked(
    cred_id: &str,
    entries: &[RevocationEntry],
    head: &RevocationHead,
    issuer_pubkey: &str,
    now_ms: i64,
) -> Result<()> {
    verify_revocation_chain(entries, issuer_pubkey)?;
    verify_revocation_head(head, entries, issuer_pubkey, now_ms)?;
    // 纵深加固：头承诺（进而全链，verify_revocation_head 已钉 entries[0].issuer
    // == head.issuer）的 issuer identity 必须与验签公钥派生的 identity 显式
    // 相等——验签只隐式绑定公钥，此行把「链属于该 issuer」落成显式断言。
    let expected_issuer = identity_from_public_key(issuer_pubkey).ok_or(
        CredentialError::RevocationChainBroken("issuer pubkey shape"),
    )?;
    if head.issuer != expected_issuer {
        return Err(CredentialError::RevocationChainBroken(
            "issuer identity scope",
        ));
    }
    if entries.iter().any(|e| e.cred_id == cred_id) {
        return Err(CredentialError::Revoked);
    }
    Ok(())
}
