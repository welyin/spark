//! 生成 `code/spec/vectors/evidence-resolution.json`（affair.md §6.3
//! evi:resolution 存证条目 golden vectors，affair-model §六验收：条目逐字节
//! 含签名包内嵌、conclusionHash 计算、链上条目逐字节）。
//!
//! 全部输入固定（密钥/ts/决议操作/签名包），输出逐字节确定——消费测试
//! `core/tests/evidence_resolution_vectors.rs` 按本文件验收。
//! 运行：`cargo run --example gen_evidence_resolution_vectors -- <repo_root>/code/spec/vectors/evidence-resolution.json`

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spark_core::affair::{
    RESOLUTION_ENTRY_COLLECTION, compute_op_hash, conclusion_hash, op_sign_payload,
    resolution_entry_payload,
};
use spark_core::evidence::{
    EvidenceOp, NewEvidenceEntry, append_evidence, build_evidence_payload_hash, normalize_object,
    verify_evidence_chain,
};
use spark_core::storage::MemoryStorage;

const ORG: &str = "org_0000000000000001";
const AFFAIR: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const T0: i64 = 1_720_000_000_000;
const EFFECTIVE_TS: i64 = T0 + 10_000;

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn identity_of(k: &SigningKey) -> String {
    hex::encode(Sha256::digest(k.verifying_key().to_bytes()))
}

/// 固定决议 payload（§6.1 线形，op-count content 1 关闭条件）。
fn resolution_payload(counted_op: &str, rules_hash: &str) -> Value {
    json!({
        "result": "passed",
        "condition": { "type": "op-count", "opType": "content", "count": 1 },
        "countedOps": [counted_op],
        "tally": null,
        "quorumSnapshot": null,
        "rulesHash": rules_hash,
        "pubPeriod": { "delayMs": 86_400_000i64 },
    })
}

/// 代表性 OrgSigSet（线形见 org-signature §2；条目原样内嵌，本向量不校验
/// 签名包本身——五步验证链向量归 org-signature 组）。
fn fixed_sig_set() -> Value {
    json!({
        "sigSetV": 1,
        "orgId": ORG,
        "subject": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "policyHash": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        "roster": {
            "memberSetHash": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            "anchor": { "orgId": ORG, "anchorRoot": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", "ts": T0 }
        },
        "signedAt": T0 + 9_000,
        "signatures": [
            { "identity": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", "sig": "c2lnLXN0dWItMQ==" }
        ]
    })
}

/// 固定签名决议操作（kind=org 携带 orgSig / kind=person 不携带）。
fn make_resolution_op(org_actor: bool) -> Value {
    let k = key(0x42);
    let sig_set = fixed_sig_set();
    let actor = if org_actor {
        json!({
            "kind": "org",
            "identity": identity_of(&k),
            "publicKey": B64.encode(k.verifying_key().to_bytes()),
            "orgSig": sig_set,
        })
    } else {
        json!({
            "kind": "person",
            "identity": identity_of(&k),
            "publicKey": B64.encode(k.verifying_key().to_bytes()),
        })
    };
    let mut op = json!({
        "opV": 1,
        "affairId": AFFAIR,
        "prevOpHash": AFFAIR,
        "opType": "resolution",
        "payload": resolution_payload(&"99".repeat(32), &"88".repeat(32)),
        "actor": actor,
        "declaredAt": T0 + 9_500,
    });
    let sign_payload = op_sign_payload(&op).expect("op sign payload");
    let sig = k.sign(sign_payload.as_bytes());
    op["sig"] = json!(B64.encode(sig.to_bytes()));
    op
}

/// 由决议操作构建 evi:resolution 条目（生产路径同口径：subject = opHash，
/// sigSet = actor.orgSig 原样，effectiveTs 注入）。
fn build_entry_payload(op: &Value) -> (String, Value) {
    let op_hash = compute_op_hash(op).expect("op hash");
    let sig_set = op["actor"].get("orgSig").cloned();
    let payload = resolution_entry_payload(
        AFFAIR,
        &op_hash,
        &op["payload"],
        sig_set.as_ref(),
        EFFECTIVE_TS,
    );
    (op_hash, payload)
}

fn main() {
    let out_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../spec/vectors/evidence-resolution.json".to_string());

    // ── 组 1：组织决议条目（签名包原样内嵌）──
    let org_op = make_resolution_op(true);
    let (org_op_hash, org_entry) = build_entry_payload(&org_op);
    // 自检：conclusionHash = 决议 payload canonical 哈希；同输入逐字节相同
    assert_eq!(
        org_entry["conclusionHash"].as_str().unwrap(),
        conclusion_hash(&org_op["payload"])
    );
    let (again_hash, again_entry) = build_entry_payload(&org_op);
    assert_eq!(org_op_hash, again_hash);
    assert_eq!(
        normalize_object(&org_entry),
        normalize_object(&again_entry),
        "条目确定性自检"
    );
    let group_entry = json!({
        "desc": "evi:resolution 条目逐字节（组织决议，sigSet 原样内嵌）：固定决议操作 + 固定锚时刻 → subject=opHash、conclusionHash、payload canonical 与 payloadHash 固定值；篡改 payload → conclusionHash 必变（消费侧断言）",
        "input": {
            "affairId": AFFAIR,
            "resolutionOp": org_op,
            "effectiveTs": EFFECTIVE_TS,
        },
        "expect": {
            "subject": org_op_hash,
            "conclusionHash": org_entry["conclusionHash"],
            "payload": org_entry,
            "canonical": normalize_object(&org_entry),
            "payloadHash": build_evidence_payload_hash(Some(&org_entry)),
        },
    });

    // ── 组 2：个人决议条目（无 orgSig → sigSet 落 null）──
    let person_op = make_resolution_op(false);
    let (person_op_hash, person_entry) = build_entry_payload(&person_op);
    assert_eq!(person_entry["sigSet"], Value::Null);
    // 两组的 conclusionHash 一致（payload 相同，签名面不影响结论哈希）
    assert_eq!(person_entry["conclusionHash"], org_entry["conclusionHash"]);
    let group_person = json!({
        "desc": "evi:resolution 条目（个人决议）：actor 无 orgSig → sigSet 落 null（如实标注）；conclusionHash 与组 1 一致（结论哈希只绑定决议 payload，不绑定签名面）",
        "input": {
            "affairId": AFFAIR,
            "resolutionOp": person_op,
            "effectiveTs": EFFECTIVE_TS,
        },
        "expect": {
            "subject": person_op_hash,
            "conclusionHash": person_entry["conclusionHash"],
            "payload": person_entry,
        },
    });

    // ── 组 3：链上条目逐字节（空链追加，固定 ts/nodeId）──
    let mut storage = MemoryStorage::new();
    let entry = append_evidence(
        &mut storage,
        NewEvidenceEntry::from_parts(
            ORG,
            RESOLUTION_ENTRY_COLLECTION,
            &org_op_hash,
            EvidenceOp::Put,
            Some(&org_entry),
            None,
            EFFECTIVE_TS,
            "12D3KooWNodeA",
        ),
    )
    .expect("append resolution entry");
    assert!(verify_evidence_chain(&storage).expect("verify chain"));
    let group_chain = json!({
        "desc": "链上条目逐字节：空链追加 evi:resolution（domain=orgId, collection=resolution, id=决议 opHash，固定 ts/nodeId）→ seq/prevHash/dataHash/payloadHash/hash 固定值；verify_evidence_chain 通过（消费侧断言）",
        "input": {
            "orgId": ORG,
            "timestamp": EFFECTIVE_TS,
            "nodeId": "12D3KooWNodeA",
        },
        "expect": { "entry": serde_json::to_value(&entry).unwrap() },
    });

    let doc = json!({
        "_comment": "affair.md §6.3 evi:resolution 存证条目 golden vectors（A21）。生成器：core/examples/gen_evidence_resolution_vectors.rs（Rust 自产，含确定性/链校验自检）；消费：core/tests/evidence_resolution_vectors.rs。",
        "entry": group_entry,
        "entryPerson": group_person,
        "chainEntry": group_chain,
    });
    let text = serde_json::to_string_pretty(&doc).unwrap();
    std::fs::write(&out_path, format!("{text}\n")).expect("write vectors");
    println!("written {out_path}");
}
