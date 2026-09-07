//! `affair_profile_ops.rs` 的内联单元测试（affair_ops_tests.rs 同款夹具模式：
//! 固定密钥/时间戳构造创世 + 操作 + 存证锚定，直写存储键绕过复制面入站；
//! 关注簿记走 affairsync 公开入口）。
//!
//! 覆盖：跨事务聚合装配（账号年龄/提议/采纳/投票历史）、无效身份 fail-closed、
//! 未锚定操作不进时间推导、乱序/重放等价（同一副本集合重复读路径结果一致）。

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::affair::{affair_op_key, compute_op_hash, op_sign_payload};
use crate::evidence::{EvidenceOp, NewEvidenceEntry, append_evidence};
use crate::kernel::KernelConfig;
use crate::storage::StorageBackend;

const PASSWORD: &str = "correct-horse-battery";
/// 固定历史时刻（真实 system_now_ms 远大于此 → delayed-veto 窗口必满）。
const T0: i64 = 1_720_000_000_000;
const DAY: i64 = 24 * 60 * 60 * 1000;

struct FixedKey {
    signing_key: SigningKey,
    public_key: String,
    identity: String,
}

fn fixed_key(seed: u8) -> FixedKey {
    let signing_key = SigningKey::from_bytes(&[seed; 32]);
    let public_key_bytes = signing_key.verifying_key().to_bytes();
    FixedKey {
        signing_key,
        public_key: base64::engine::general_purpose::STANDARD.encode(public_key_bytes),
        identity: hex::encode(Sha256::digest(public_key_bytes)),
    }
}

fn sign(key: &FixedKey, payload: &str) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(key.signing_key.sign(payload.as_bytes()).to_bytes())
}

fn make_op(
    affair_id: &str,
    key: &FixedKey,
    op_type: &str,
    payload: Value,
    prev_op_hash: &str,
    declared_at: i64,
) -> (Value, String) {
    let mut op = json!({
        "opV": 1, "affairId": affair_id, "prevOpHash": prev_op_hash,
        "opType": op_type, "payload": payload,
        "actor": { "kind": "person", "identity": key.identity, "publicKey": key.public_key },
        "declaredAt": declared_at,
    });
    let sig_payload = op_sign_payload(&op).expect("op payload");
    op["sig"] = json!(sign(key, &sig_payload));
    let op_hash = compute_op_hash(&op).expect("op hash");
    (op, op_hash)
}

fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = Kernel::init(KernelConfig {
        data_dir: dir.path().to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: None,
    })
    .unwrap();
    kernel.init_identity(PASSWORD, "alice", None).unwrap();
    (dir, kernel)
}

/// 关注簿记 + 写入操作 + 指定锚定时刻（直写存储，绕过入站）。
fn seed_affair(kernel: &mut Kernel, affair_id: &str, ops: &[(Value, String, Option<i64>)]) {
    {
        let storage = kernel.require_storage_raw_mut().unwrap();
        crate::sync::affairsync::follow_affair(storage, affair_id, T0).unwrap();
        for (op, op_hash, _) in ops {
            storage
                .put(&affair_op_key(affair_id, op_hash), &op.to_string())
                .unwrap();
        }
    }
    for (op, op_hash, anchored_ms) in ops {
        if let Some(ts) = anchored_ms {
            let storage = kernel.require_storage_raw_mut().unwrap();
            append_evidence(
                storage,
                NewEvidenceEntry::from_parts(
                    &format!("affair:{affair_id}"),
                    "ops",
                    op_hash,
                    EvidenceOp::Put,
                    Some(op),
                    None,
                    *ts,
                    "test-node",
                ),
            )
            .unwrap();
        }
    }
}

fn delayed_veto_meta(delay_ms: i64) -> Value {
    json!({
        "title": "修订",
        "mechanism": { "kind": "delayed-veto", "delayMs": delay_ms, "vetoThreshold": { "count": 1 } }
    })
}

/// 端到端装配：两个事务，目标身份在 A 有老 content + 一条被采纳修订 + 一张
/// 赞成票，在 B 有一条被异议否决的修订（不计采纳）+ 一张未锚定反对票 +
/// 一条未锚定 content（不进账龄）；另一身份的更老操作不混入。
#[test]
fn public_profile_aggregates_across_affairs() {
    let (_dir, mut kernel) = unlocked_kernel();
    let alice = fixed_key(0x21);
    let bob = fixed_key(0x22);
    let affair_a = "aa".repeat(32);
    let affair_b = "bb".repeat(32);
    let proposal_target = "cc".repeat(32);

    let (a_content, a_content_hash) = make_op(
        &affair_a,
        &alice,
        "content",
        json!({ "kind": "post", "text": "老帖" }),
        &affair_a,
        T0,
    );
    let (a_meta, a_meta_hash) = make_op(
        &affair_a,
        &alice,
        "meta-revise",
        delayed_veto_meta(DAY),
        &a_content_hash,
        T0 + 1_000,
    );
    let (a_vote, a_vote_hash) = make_op(
        &affair_a,
        &alice,
        "vote",
        json!({ "proposal": proposal_target, "choice": "yes" }),
        &a_meta_hash,
        T0 + 2_000,
    );
    seed_affair(
        &mut kernel,
        &affair_a,
        &[
            (a_content, a_content_hash, Some(T0 + 10 * DAY)),
            (a_meta, a_meta_hash, Some(T0 + 20 * DAY)),
            (a_vote, a_vote_hash, Some(T0 + 21 * DAY)),
        ],
    );

    let (b_meta, b_meta_hash) = make_op(
        &affair_b,
        &alice,
        "meta-revise",
        delayed_veto_meta(DAY),
        &affair_b,
        T0 + 3_000,
    );
    let (b_objection, b_objection_hash) = make_op(
        &affair_b,
        &bob,
        "objection",
        json!({ "target": b_meta_hash }),
        &b_meta_hash,
        T0 + 4_000,
    );
    let (b_vote, b_vote_hash) = make_op(
        &affair_b,
        &alice,
        "vote",
        json!({ "proposal": proposal_target, "choice": "no" }),
        &b_objection_hash,
        T0 + 5_000,
    );
    let (b_unanchored, b_unanchored_hash) = make_op(
        &affair_b,
        &alice,
        "content",
        json!({ "kind": "post", "text": "未锚定" }),
        &b_vote_hash,
        T0 + 6_000,
    );
    let (b_bob, b_bob_hash) = make_op(
        &affair_b,
        &bob,
        "content",
        json!({ "kind": "post", "text": "他人更老" }),
        &b_unanchored_hash,
        T0 + 7_000,
    );
    seed_affair(
        &mut kernel,
        &affair_b,
        &[
            (b_meta, b_meta_hash.clone(), Some(T0 + 30 * DAY)),
            (b_objection, b_objection_hash, Some(T0 + 31 * DAY)),
            (b_vote, b_vote_hash, None), // 未锚定票：计票不进账龄
            (b_unanchored, b_unanchored_hash, None),
            (b_bob, b_bob_hash, Some(T0 - 90 * DAY)), // 他人操作更老不混入
        ],
    );

    let out = kernel.affair_public_profile(&alice.identity).unwrap();
    assert_eq!(out["identity"], json!(alice.identity));
    assert_eq!(out["affairsParticipated"], json!(2));
    // 账号年龄 = 求值时刻 − 最早链上活跃（A 的 content 锚定 T0+10 天）
    assert_eq!(out["firstActivityMs"], json!(T0 + 10 * DAY));
    let age = out["accountAgeMs"].as_i64().unwrap();
    let now = out["nowMs"].as_i64().unwrap();
    assert_eq!(age, now - (T0 + 10 * DAY));
    // 提议 2（A/B 各一 meta-revise）；采纳 1（B 的被 bob 异议否决）
    assert_eq!(out["proposals"], json!(2));
    assert_eq!(out["adoptions"], json!(1));
    // 投票 2：yes 1 / no 1（含未锚定票）
    assert_eq!(out["votes"], json!(2));
    assert_eq!(out["votesYes"], json!(1));
    assert_eq!(out["votesNo"], json!(1));
    let history = out["voteHistory"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["affairId"], json!(affair_a));
    assert_eq!(history[0]["choice"], json!("yes"));
    assert_eq!(history[1]["choice"], json!("no"));
    assert_eq!(history[1]["anchoredMs"], Value::Null);
    // per_affair 按 affairId 字典序
    let per = out["perAffair"].as_array().unwrap();
    assert_eq!(per.len(), 2);
    assert_eq!(per[0]["affairId"], json!(affair_a));
    assert_eq!(per[0]["adoptions"], json!(1));
    assert_eq!(per[1]["affairId"], json!(affair_b));
    assert_eq!(per[1]["adoptions"], json!(0));
    assert_eq!(per[1]["opCount"], json!(3)); // meta + vote + 未锚定 content

    // 重放等价：重复查询除求值时刻（nowMs 走系统时钟，两次调用间可能走字）
    // 与其派生的 accountAgeMs 外逐字节一致（同一副本集合确定性复算）
    let again = kernel.affair_public_profile(&alice.identity).unwrap();
    let strip_clock = |v: &Value| {
        let mut v = v.clone();
        let obj = v.as_object_mut().unwrap();
        obj.remove("nowMs");
        obj.remove("accountAgeMs");
        v
    };
    assert_eq!(strip_clock(&out), strip_clock(&again));

    // 无关身份 → 诚实空集
    let empty = kernel.affair_public_profile(&"dd".repeat(32)).unwrap();
    assert_eq!(empty["affairsParticipated"], json!(0));
    assert_eq!(empty["accountAgeMs"], Value::Null);
    assert_eq!(empty["proposals"], json!(0));
}

#[test]
fn public_profile_rejects_invalid_identity() {
    let (_dir, kernel) = unlocked_kernel();
    let err = kernel.affair_public_profile("not-an-identity").unwrap_err();
    assert!(format!("{err:?}").contains("invalid identity"), "{err}");
    // 未关注任何事务：合法身份 → 空视图而非错误
    let out = kernel.affair_public_profile(&"ee".repeat(32)).unwrap();
    assert_eq!(out["affairsParticipated"], json!(0));
}
