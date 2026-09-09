//! 回填 `code/spec/vectors/community.json` 的 `policy.disclosure` case 组
//! （A15 名册开放声明线形向量；规格登记：policy §8 / read-gate §6）：
//!
//! - 线形：DisclosureRecord canonical → disclosureHash 固定值（sigSet 剔除
//!   自认证口径钉死）；
//! - 暴露面扩大静态分析（`disclosure_widening`）真值表：首声明/档位升降/
//!   字段授权增删/受众升降/开放集合增删；
//! - 求值（`eval_disclosure`）：生效门控（公示延迟窗口内不装配）/ 最高
//!   version 胜 / 无声明默认仅组织档。
//!
//! 本生成器只对 `policy.disclosure` 一组做 read-modify-write upsert，其他组
//! 一字节不动（与 gen_credential_vectors.rs 同纪律）。
//!
//! 用法：`cargo run --example gen_disclosure_vectors -- [community.json 路径]`
//! 自检：哈希复算 + widening/eval 逐 case 以 core 实现实跑比对 expect，
//! 任一失败 panic（非零退出）。

use serde_json::{Value, json};
use spark_core::policy::{
    Audience, DISCLOSURE_PUB_PERIOD_MS, DISCLOSURE_V, DisclosureRecord, FieldRule, RosterTier,
    disclosure_hash, disclosure_widening, eval_disclosure, validate_disclosure,
};

const NOW: i64 = 1_720_000_000_000;

fn owner() -> String {
    format!("org_{}", "dd".repeat(32))
}

fn target() -> String {
    format!("org_{}", "ee".repeat(32))
}

fn third() -> String {
    format!("org_{}", "ff".repeat(32))
}

fn field(name: &str, audience: Audience) -> FieldRule {
    FieldRule {
        field: name.to_string(),
        audience,
    }
}

/// v1 全量声明（代表可见 + 字段授权 + 开放集合；扩大方向，effectiveAt =
/// updatedAt + 24h 公示延迟）。
fn v1_full() -> DisclosureRecord {
    DisclosureRecord {
        disclosure_v: DISCLOSURE_V,
        org_id: owner(),
        target_domain: target(),
        tier: RosterTier::Representatives,
        fields: vec![field("phone", Audience::Representatives)],
        collections: vec!["finance:monthly@v1".to_string()],
        version: 1,
        updated_at: NOW,
        effective_at: NOW + DISCLOSURE_PUB_PERIOD_MS,
        sig_set: None,
    }
}

/// v1 全隐首声明（仅组织 + 空授权 + 空集合，即时生效）。
fn v1_minimal() -> DisclosureRecord {
    DisclosureRecord {
        disclosure_v: DISCLOSURE_V,
        org_id: owner(),
        target_domain: target(),
        tier: RosterTier::OrgOnly,
        fields: vec![],
        collections: vec![],
        version: 1,
        updated_at: NOW,
        effective_at: NOW,
        sig_set: None,
    }
}

/// v2 收窄（档位降回仅组织、移除字段授权与开放集合，即时生效）。
fn v2_narrowed() -> DisclosureRecord {
    DisclosureRecord {
        version: 2,
        tier: RosterTier::OrgOnly,
        fields: vec![],
        collections: vec![],
        updated_at: NOW + DISCLOSURE_PUB_PERIOD_MS,
        effective_at: NOW + DISCLOSURE_PUB_PERIOD_MS, // 收窄即时：= updatedAt
        ..v1_full()
    }
}

/// v2 扩大（档位升名册公开 + 字段受众升 + 新增开放集合）。
fn v2_widened() -> DisclosureRecord {
    DisclosureRecord {
        version: 2,
        tier: RosterTier::Public,
        fields: vec![field("phone", Audience::Public)],
        collections: vec![
            "finance:monthly@v1".to_string(),
            "finance:yearly@v1".to_string(),
        ],
        updated_at: NOW + DISCLOSURE_PUB_PERIOD_MS,
        effective_at: NOW + 2 * DISCLOSURE_PUB_PERIOD_MS,
        ..v1_full()
    }
}

fn record_json(r: &DisclosureRecord) -> Value {
    serde_json::to_value(r).expect("serialize disclosure record")
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../spec/vectors/community.json"
        )
        .to_string()
    });
    let raw = std::fs::read_to_string(&path).expect("read community.json");
    let mut doc: Value = serde_json::from_str(&raw).expect("parse community.json");

    let full = v1_full();
    let minimal = v1_minimal();
    let narrowed = v2_narrowed();
    let widened = v2_widened();
    for (name, r) in [
        ("v1Full", &full),
        ("v1Minimal", &minimal),
        ("v2Narrowed", &narrowed),
        ("v2Widened", &widened),
    ] {
        validate_disclosure(r).unwrap_or_else(|e| panic!("{name} invalid: {e}"));
    }

    let group = json!({
        "desc": "名册开放声明（A15 / membership §4.3，policy §8）：线形 canonical → disclosureHash 固定值（sigSet 剔除自认证）；暴露面扩大静态分析（disclosure_widening）真值表——公示延迟触发条件；求值（eval_disclosure）——生效门控（公示延迟窗口内不装配）/ 最高 version 胜 / 无声明默认仅组织档。生成器：core/examples/gen_disclosure_vectors.rs",
        "context": {
            "ownerOrgId": owner(),
            "targetDomain": target(),
            "thirdDomain": third(),
            "nowMs": NOW,
            "pubPeriodMs": DISCLOSURE_PUB_PERIOD_MS,
        },
        "records": {
            "v1Full": record_json(&full),
            "v1Minimal": record_json(&minimal),
            "v2Narrowed": record_json(&narrowed),
            "v2Widened": record_json(&widened),
        },
        "expect": {
            "disclosureHash": {
                "v1Full": disclosure_hash(&full).expect("hash v1Full"),
                "v1Minimal": disclosure_hash(&minimal).expect("hash v1Minimal"),
                "v2Narrowed": disclosure_hash(&narrowed).expect("hash v2Narrowed"),
                "v2Widened": disclosure_hash(&widened).expect("hash v2Widened"),
            },
        },
        "wideningCases": [
            // 首份声明：全隐不扩大；任何非全隐形态皆扩大（从无到有即暴露面扩大）
            { "name": "first-default-not-widening", "prev": Value::Null, "next": "v1Minimal", "expect": false },
            { "name": "first-non-default-widening", "prev": Value::Null, "next": "v1Full", "expect": true },
            // 持平 / 收窄（档位降、授权移除）不扩大
            { "name": "unchanged-not-widening", "prev": "v1Full", "next": "v1Full", "expect": false },
            { "name": "narrowed-not-widening", "prev": "v1Full", "next": "v2Narrowed", "expect": false },
            // 档位升 / 字段受众升 / 新增开放集合 = 扩大（公示延迟触发）
            { "name": "tier-raised-widening", "prev": "v1Full", "next": "v2Widened", "expect": true },
        ],
        "evalCases": [
            { "name": "no-record-default-org-only",
              "records": [], "targetDomain": target(), "nowMs": NOW,
              "expect": { "tier": "org-only", "fields": [], "collections": [] } },
            { "name": "pending-not-assembled",
              "records": ["v1Full"], "targetDomain": target(), "nowMs": NOW,
              "expect": { "tier": "org-only", "fields": [], "collections": [] } },
            { "name": "effective-full-view",
              "records": ["v1Full"], "targetDomain": target(),
              "nowMs": NOW + DISCLOSURE_PUB_PERIOD_MS,
              "expect": {
                  "tier": "representatives",
                  "fields": [{ "field": "phone", "audience": "representatives" }],
                  "collections": ["finance:monthly@v1"],
              } },
            { "name": "highest-effective-version-wins",
              "records": ["v1Full", "v2Narrowed"], "targetDomain": target(),
              "nowMs": NOW + DISCLOSURE_PUB_PERIOD_MS,
              "expect": { "tier": "org-only", "fields": [], "collections": [] } },
            { "name": "other-domain-untouched",
              "records": ["v1Full"], "targetDomain": third(),
              "nowMs": NOW + 2 * DISCLOSURE_PUB_PERIOD_MS,
              "expect": { "tier": "org-only", "fields": [], "collections": [] } },
        ],
    });

    // ── 自检：以 core 实现实跑全部 case ──
    self_check(&group);

    doc["policy.disclosure"] = group;
    let text = serde_json::to_string_pretty(&doc).expect("serialize");
    std::fs::write(&path, format!("{text}\n")).expect("write community.json");
    println!("written {path}: +policy.disclosure");
}

fn case_record(group: &Value, key: &str) -> DisclosureRecord {
    serde_json::from_value(group["records"][key].clone()).expect("deserialize case record")
}

fn self_check(group: &Value) {
    // 哈希逐条复算
    for (key, expect) in group["expect"]["disclosureHash"]
        .as_object()
        .expect("hash map")
    {
        let record = case_record(group, key);
        assert_eq!(
            disclosure_hash(&record).expect("rehash"),
            expect.as_str().unwrap(),
            "hash drift {key}"
        );
    }
    // widening 逐 case
    for case in group["wideningCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let prev = case["prev"].as_str().map(|k| case_record(group, k));
        let next = case_record(group, case["next"].as_str().unwrap());
        assert_eq!(
            disclosure_widening(prev.as_ref(), &next),
            case["expect"].as_bool().unwrap(),
            "widening case {name}"
        );
    }
    // eval 逐 case
    for case in group["evalCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let records: Vec<DisclosureRecord> = case["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| case_record(group, k.as_str().unwrap()))
            .collect();
        let refs: Vec<&DisclosureRecord> = records.iter().collect();
        let view = eval_disclosure(
            &refs,
            case["targetDomain"].as_str().unwrap(),
            case["nowMs"].as_i64().unwrap(),
        );
        let expect = &case["expect"];
        assert_eq!(
            view.tier.to_string(),
            expect["tier"].as_str().unwrap(),
            "eval tier {name}"
        );
        let expect_fields: Vec<FieldRule> = expect["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| serde_json::from_value(f.clone()).unwrap())
            .collect();
        assert_eq!(view.fields, expect_fields, "eval fields {name}");
        let expect_collections: Vec<String> = expect["collections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_string())
            .collect();
        assert_eq!(view.collections, expect_collections, "eval collections {name}");
    }
}
