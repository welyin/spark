//! `affair_retention_ops.rs` 的内联单元测试（affair_evi_ops_tests.rs 同款拆分先例）。
//!
//! 覆盖（affair-model §4.2 / §六验收）：
//! - 两档语义与缺省兼容：无 retention 字段的旧事务 → followers；
//! - 双条件复制组并集：事务声明 ∧ 组织接受 ∧ 已生效（公示窗内/撤销/未声明
//!   组织的 pin 一律不生效力）；
//! - 清理边界（不杀最后副本原则）：followers 档取关即可回收；org-pinned 档
//!   本机是生效 pin 组织数据节点时即使未关注也不得回收；组织记录缺失的
//!   保守分支保留；
//! - pin 发布流：接受 = 摊派面扩大须显式确认 + 24h 公示延迟；撤销即时；
//!   version 单调。

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::affair::{
    PIN_PUB_PERIOD_MS, affair_record_key, compute_affair_id, genesis_sign_payload, org_pin_key,
};
use crate::kernel::KernelConfig;

const PASSWORD: &str = "correct-horse-battery";
/// 固定历史时刻（真实 system_now_ms 远大于此 → 以此为 effectiveAt 的 pin 必生效）。
const T0: i64 = 1_720_000_000_000;
const DAY: i64 = 24 * 60 * 60 * 1000;
/// 远期未来时刻（以此为 effectiveAt 的 pin 恒在公示窗内）。
const FUTURE: i64 = 4_000_000_000_000;

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

fn baseline_rules() -> Value {
    json!({
        "engine": "b1",
        "closeConditions": [{ "type": "op-count", "opType": "content", "count": 1 }],
        "pubPeriod": { "delayMs": DAY, "vetoThreshold": { "count": 1 } },
        "ruleChange": { "kind": "delayed-veto", "delayMs": 3 * DAY, "vetoThreshold": { "count": 1 } },
        "exec": null,
    })
}

fn make_genesis(initiator: &FixedKey, rules: Value) -> (Value, String) {
    let mut genesis = json!({
        "affairV": 1, "type": "forum", "title": "保留策略事务", "summary": "", "tags": [],
        "initiator": { "kind": "person", "identity": initiator.identity, "publicKey": initiator.public_key },
        "rules": rules, "initialVoters": [initiator.identity], "refs": [], "createdAt": T0,
    });
    let payload = genesis_sign_payload(&genesis).expect("genesis payload");
    genesis["sig"] = json!(sign(initiator, &payload));
    let affair_id = compute_affair_id(&genesis).expect("affair id");
    (genesis, affair_id)
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

/// 直写事务创世记录（读/编排路径只消费本地副本；绕过复制面入站）。
fn write_affair(kernel: &mut Kernel, affair_id: &str, genesis: &Value) {
    let storage = kernel.require_storage_raw_mut().unwrap();
    storage
        .put(&affair_record_key(affair_id), &genesis.to_string())
        .unwrap();
}

/// 直写 pin 接受声明（dummy sigSet——本组用例不走入站合入五步链）。
fn write_pin(kernel: &mut Kernel, org_id: &str, affair_id: &str, effective_at: i64, revoked: bool) {
    let mut value = json!({
        "pinV": 1, "orgId": org_id, "affairId": affair_id,
        "version": 1, "updatedAt": T0, "effectiveAt": effective_at,
        "sigSet": { "signatures": [] },
    });
    if revoked {
        value["revoked"] = json!(true);
    }
    let storage = kernel.require_storage_raw_mut().unwrap();
    storage
        .put(&org_pin_key(org_id, affair_id).unwrap(), &value.to_string())
        .unwrap();
}

fn org_id(byte: &str) -> String {
    format!("org_{}", byte.repeat(32))
}

/// 组织签名包前提：本机存证链有该组织域条目且锚记录已发布
/// （build_signed_org_sig_set 的 anchor_root 取自 org:evi:anchor: 键域）。
fn anchor_org(kernel: &mut Kernel, org_id: &str) {
    {
        let storage = kernel.require_storage_raw_mut().unwrap();
        crate::evidence::append_evidence(
            storage,
            crate::evidence::NewEvidenceEntry::from_parts(
                org_id,
                "genesis",
                org_id,
                crate::evidence::EvidenceOp::Put,
                Some(&json!({ "orgId": org_id })),
                None,
                T0,
                "test-node",
            ),
        )
        .unwrap();
    }
    kernel.evidence_anchor(org_id).unwrap();
}

/// 缺省兼容：无 retention 字段的旧事务 → followers，无任何 pin 状态。
#[test]
fn retention_status_default_followers() {
    let (_dir, mut kernel) = unlocked_kernel();
    let initiator = fixed_key(0x81);
    let (genesis, affair_id) = make_genesis(&initiator, baseline_rules());
    write_affair(&mut kernel, &affair_id, &genesis);

    let status = kernel.affair_retention_status(&affair_id).unwrap();
    assert_eq!(status["policy"], "followers");
    assert_eq!(status["declaredOrgs"], json!([]));
    assert_eq!(status["pins"], json!([]));
    assert_eq!(status["effectivePinOrgs"], json!([]));
    assert!(kernel.affair_retention_status("zz").is_err());
}

/// 两档语义 + 双条件并集：org-pinned 声明 org_a/org_b；org_a pin 已生效、
/// org_b pin 公示窗内、org_c pin 已生效但事务未声明 → 并集仅 org_a。
#[test]
fn retention_status_org_pinned_double_condition() {
    let (_dir, mut kernel) = unlocked_kernel();
    let initiator = fixed_key(0x82);
    let (org_a, org_b, org_c) = (org_id("aa"), org_id("bb"), org_id("cc"));
    let mut rules = baseline_rules();
    rules["retention"] = json!({ "policy": "org-pinned", "orgs": [org_a.clone(), org_b.clone()] });
    let (genesis, affair_id) = make_genesis(&initiator, rules);
    write_affair(&mut kernel, &affair_id, &genesis);
    write_pin(&mut kernel, &org_a, &affair_id, T0, false); // 已生效
    write_pin(&mut kernel, &org_b, &affair_id, FUTURE, false); // 公示窗内
    write_pin(&mut kernel, &org_c, &affair_id, T0, false); // 未声明组织

    let status = kernel.affair_retention_status(&affair_id).unwrap();
    assert_eq!(status["policy"], "org-pinned");
    assert_eq!(status["declaredOrgs"], json!([org_a.clone(), org_b.clone()]));
    assert_eq!(status["pins"][0]["state"], "effective");
    assert_eq!(status["pins"][1]["state"], "pending");
    // 双条件：声明 ∧ 接受 ∧ 已生效——org_c 接受但未声明，不生效力（防摊派）
    assert_eq!(status["effectivePinOrgs"], json!([org_a.clone()]));

    // org_a 撤销 → 并集空
    write_pin(&mut kernel, &org_a, &affair_id, T0, true);
    let status = kernel.affair_retention_status(&affair_id).unwrap();
    assert_eq!(status["pins"][0]["state"], "revoked");
    assert_eq!(status["effectivePinOrgs"], json!([]));
}

/// 清理边界（followers 档）：只看关注者副本——未关注可回收，关注即保留，
/// 取关后回收面重新打开。
#[test]
fn body_hold_followers_tier() {
    let (_dir, mut kernel) = unlocked_kernel();
    let initiator = fixed_key(0x83);
    let (genesis, affair_id) = make_genesis(&initiator, baseline_rules());
    write_affair(&mut kernel, &affair_id, &genesis);

    assert!(
        !kernel.affair_body_hold(&affair_id).unwrap(),
        "followers 档未关注 → 本机副本可消亡"
    );
    crate::sync::affairsync::follow_affair(
        kernel.require_storage_raw_mut().unwrap(),
        &affair_id,
        T0,
    )
    .unwrap();
    assert!(kernel.affair_body_hold(&affair_id).unwrap(), "关注即保留");
    crate::sync::affairsync::unfollow_affair(kernel.require_storage_raw_mut().unwrap(), &affair_id)
        .unwrap();
    assert!(
        !kernel.affair_body_hold(&affair_id).unwrap(),
        "取关后回收面重新打开"
    );
}

/// 清理边界（org-pinned 档，不杀最后副本原则）：本机是生效 pin 组织的数据
/// 节点（成员即数据节点，A14）时即使未关注也不得回收；公示窗内/已撤销 pin
/// 不生效力；生效 pin 在手但组织记录缺失 → 保守保留。
#[test]
fn body_hold_org_pinned_member_keeps_body() {
    let (_dir, mut kernel) = unlocked_kernel();
    // 本机创建的org：创建者为唯一 admin 成员（= 数据节点）
    let view = kernel
        .create_org(crate::org::service::CreateOrganizationInput {
            name: "保留测试组织".to_string(),
            ..Default::default()
        })
        .unwrap();
    let my_org = view.record.org_id.clone();
    let other_org = org_id("ee");

    let initiator = fixed_key(0x84);
    let mut rules = baseline_rules();
    rules["retention"] =
        json!({ "policy": "org-pinned", "orgs": [my_org.clone(), other_org.clone()] });
    let (genesis, affair_id) = make_genesis(&initiator, rules);
    write_affair(&mut kernel, &affair_id, &genesis);

    // 未关注 + 无任何 pin → 可回收
    assert!(!kernel.affair_body_hold(&affair_id).unwrap());
    // 本机组织 pin 公示窗内 → 不生效力，可回收
    write_pin(&mut kernel, &my_org, &affair_id, FUTURE, false);
    assert!(
        !kernel.affair_body_hold(&affair_id).unwrap(),
        "公示窗内 pin 不构成保留义务"
    );
    // 本机组织 pin 生效 → 本机是数据节点 → 不杀最后副本
    write_pin(&mut kernel, &my_org, &affair_id, T0, false);
    assert!(
        kernel.affair_body_hold(&affair_id).unwrap(),
        "生效 pin ∧ 本机是组织数据节点 → 即使未关注也不得回收"
    );
    // 撤销 → 保留义务解除
    write_pin(&mut kernel, &my_org, &affair_id, T0, true);
    assert!(!kernel.affair_body_hold(&affair_id).unwrap());
    // 保守分支：他组织生效 pin 在手但本地无该组织记录（无法证伪成员资格）→ 保留
    write_pin(&mut kernel, &other_org, &affair_id, T0, false);
    assert!(
        kernel.affair_body_hold(&affair_id).unwrap(),
        "组织记录缺失 → fail-safe 保留"
    );
}

/// pin 发布流：接受 = 摊派面扩大须显式确认 + 24h 公示延迟；重复接受持平
/// 即时；撤销收窄即时；version 单调；org_pin_list 视图一致。
#[test]
fn pin_publish_flow() {
    let (_dir, mut kernel) = unlocked_kernel();
    let view = kernel
        .create_org(crate::org::service::CreateOrganizationInput {
            name: "pin 发布组织".to_string(),
            ..Default::default()
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    let affair_id = "dd".repeat(32);
    // 组织签名包须以存证锚承诺名册状态（build_signed_org_sig_set 五步链第 3 步）
    anchor_org(&mut kernel, &org_id);

    // 接受未确认 → 拒（扩大须显式确认）
    assert!(kernel.org_pin_publish(&org_id, &affair_id, false, false).is_err());
    // 确认接受 → 公示延迟 24h 生效
    let out = kernel.org_pin_publish(&org_id, &affair_id, false, true).unwrap();
    assert_eq!(out["widening"], true);
    assert_eq!(out["version"], 1);
    assert_eq!(out["effectiveAt"], out["updatedAt"].as_i64().unwrap() + PIN_PUB_PERIOD_MS);
    // 重复接受（前版未撤销）→ 持平即时
    let out = kernel.org_pin_publish(&org_id, &affair_id, false, false).unwrap();
    assert_eq!(out["widening"], false);
    assert_eq!(out["version"], 2);
    assert_eq!(out["effectiveAt"], out["updatedAt"]);
    // 撤销 → 收窄即时
    let out = kernel.org_pin_publish(&org_id, &affair_id, true, false).unwrap();
    assert_eq!(out["widening"], false);
    assert_eq!(out["revoked"], true);
    assert_eq!(out["version"], 3);
    assert_eq!(out["effectiveAt"], out["updatedAt"]);

    // accept 侧视图：最新版（撤销）记录
    let pins = kernel.org_pin_list(&org_id).unwrap();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0]["affairId"], json!(affair_id));
    assert_eq!(pins[0]["version"], 3);
    assert_eq!(pins[0]["revoked"], true);
    assert_eq!(pins[0]["effective"], false);
}

/// 撤销后再接受 = 摊派面重新扩大 → 24h 公示延迟（防「秒撤秒收」绕过公示）。
#[test]
fn pin_reaccept_after_revoke_is_widening() {
    let (_dir, mut kernel) = unlocked_kernel();
    let view = kernel
        .create_org(crate::org::service::CreateOrganizationInput {
            name: "再接受组织".to_string(),
            ..Default::default()
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    let affair_id = "ee".repeat(32);
    anchor_org(&mut kernel, &org_id);

    kernel.org_pin_publish(&org_id, &affair_id, false, true).unwrap();
    kernel.org_pin_publish(&org_id, &affair_id, true, false).unwrap();
    // 撤销后再接受：未确认 → 拒
    assert!(kernel.org_pin_publish(&org_id, &affair_id, false, false).is_err());
    let out = kernel.org_pin_publish(&org_id, &affair_id, false, true).unwrap();
    assert_eq!(out["widening"], true);
    assert_eq!(out["version"], 3);
    assert_eq!(out["effectiveAt"], out["updatedAt"].as_i64().unwrap() + PIN_PUB_PERIOD_MS);
}
