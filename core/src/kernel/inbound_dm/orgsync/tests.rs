//! orgsync 入站编排的内联测试（从 `orgsync.rs` 拆出，文件长度硬线——
//! 测试天然罗列另置，与 access_tests.rs 同款先例）。零逻辑变化。

    use super::*;
    use crate::org::types::{OrganizationMember, OrganizationRecord, OrganizationRole};
    use crate::plugindata::{Accounts, Scope, Space, declare};
    use crate::storage::{MemoryStorage, ScanOptions};
    use crate::sync::meta::DocMeta;
    use serde_json::json;

    fn member(root_id: &str) -> OrganizationMember {
        OrganizationMember {
            root_id: root_id.to_string(),
            role: OrganizationRole::Member,
            joined_at: 1000,
            added_by: "creator".to_string(),
            node_info: None,
            nickname: None,
            avatar: None,
            signature: None,
            gender: None,
            region: None,
            use_personal_identity: None,
            access_key: None,
            extra: Default::default(),
        }
    }

    fn ctx<'a>(
        my_root_id: &'a str,
        remote_peer_id: &'a str,
        online: &'a std::collections::HashSet<String>,
    ) -> InboundContext<'a> {
        InboundContext {
            my_root_id,
            my_nickname: "me",
            remote_peer_id,
            online_peers: online,
            node_id: "local-node",
            now_ms: 2000,
            kverify: None,
        }
    }

    fn setup_org_and_collection() -> MemoryStorage {
        let mut s = MemoryStorage::new();
        // 组织记录：member-a（发送方/接收方）与 self（本机）均为成员
        let record = OrganizationRecord {
            org_id: "org_0000000000000001".to_string(),
            name: "t".to_string(),
            description: String::new(),
            avatar: String::new(),
            base_plugin_domain: None,
            created_at: 1000,
            created_by: "self".to_string(),
            updated_at: 1000,
            members: vec![member("member-a"), member("self")],
            sync: None,
            gateways: vec![],
            data_accounts: vec![],
            org_address: None,
            is_public: false,
            extra: Default::default(),
        };
        crate::org::OrganizationService::save_record(&mut s, &record).unwrap();
        // 声明集合：all-members（复制组 = 全体成员）
        let decl = declare(
            &mut s,
            "ai-chat",
            crate::plugindata::DeclareInput {
                name: "ai-chat:finance".to_string(),
                version: Some("1.0.0".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            1000,
            Some("org_0000000000000001"),
        )
        .unwrap();
        let _ = decl;
        s
    }

    fn meta(node: &str, counter: i64) -> DocMeta {
        DocMeta {
            vv: [(node.to_string(), counter)].into_iter().collect(),
            ts: 2000,
            node_id: Some(node.to_string()),
            ..Default::default()
        }
    }

    /// B3：orgsync-data 入站 key 白名单——`orgd:` 数据键放行，
    /// 越界键（`p2p:` 等）整批拒收。
    #[test]
    fn orgsync_data_key_whitelist_rejects_out_of_collection() {
        let mut s = setup_org_and_collection();
        let online = std::collections::HashSet::new();
        let c = ctx("self", "peer-a", &online);

        // 合法：orgd: 数据键 → 应应用（返回 ok，orgsync_out 非空）
        let ok_body = crate::sync::orgsync::build_orgsync_data_batch(
            "org_0000000000000001",
            "ai-chat:finance@v1.0.0",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: "orgd:org_0000000000000001:ai-chat:finance@v1.0.0:k1".to_string(),
                value: serde_json::json!("v"),
                meta: meta("node-a", 1),
                dseq: None,
            }],
            0,
            1,
        );
        let res = handle_orgsync_data(&mut s, &c, "member-a", &ok_body).unwrap();
        assert_eq!(res.response["ok"], json!(true), "合法 orgd 键应放行");
        assert!(
            s.get("orgd:org_0000000000000001:ai-chat:finance@v1.0.0:k1")
                .unwrap()
                .is_some(),
            "合法记录已合入"
        );

        // 越界：p2p: 键 → 整批拒收（reason key-out-of-collection）
        let bad_body = crate::sync::orgsync::build_orgsync_data_batch(
            "org_0000000000000001",
            "ai-chat:finance@v1.0.0",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: "p2p:identity:privateKey".to_string(),
                value: serde_json::json!("x"),
                meta: meta("node-a", 1),
                dseq: None,
            }],
            0,
            1,
        );
        let res2 = handle_orgsync_data(&mut s, &c, "member-a", &bad_body).unwrap();
        assert_eq!(res2.response["ok"], json!(false));
        assert_eq!(res2.response["reason"], json!("key-out-of-collection"));
        assert!(
            s.get("p2p:identity:privateKey").unwrap().is_none(),
            "越界键不得落库"
        );

        // O4 红线：orgkey（集合对称密钥，personal 域）**永不进组织同步流量**——
        // orgsync-data 白名单只放行 orgd:/org:coll:/org:acl:/存量组织键，orgkey:
        // 越界整批拒收（密钥只经 dm 定向投递 + pdsync 自设备扩散）。
        let key_body = crate::sync::orgsync::build_orgsync_data_batch(
            "org_0000000000000001",
            "ai-chat:finance@v1.0.0",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: crate::sync::orgsync::orgkey_key(
                    "org_0000000000000001",
                    "ai-chat:finance",
                    "1.0.0",
                    1,
                ),
                value: serde_json::json!("secret"),
                meta: meta("node-a", 1),
                dseq: None,
            }],
            0,
            1,
        );
        let res3 = handle_orgsync_data(&mut s, &c, "member-a", &key_body).unwrap();
        assert_eq!(res3.response["ok"], json!(false));
        assert_eq!(res3.response["reason"], json!("key-out-of-collection"));
        assert!(
            s.get(&crate::sync::orgsync::orgkey_key(
                "org_0000000000000001",
                "ai-chat:finance",
                "1.0.0",
                1
            ))
            .unwrap()
            .is_none(),
            "orgkey 密文不得经组织同步流量落库"
        );
    }

    /// B4：orgsync-data 入站远端墓碑落地后补登 **org 域 dlog**（接力传播），
    /// 个人域 dlog 不被 orgd: 污染。
    #[test]
    fn orgsync_data_tombstone_relay_appends_org_dlog() {
        let mut s = setup_org_and_collection();
        let online = std::collections::HashSet::new();
        let c = ctx("self", "peer-a", &online);
        let key = "orgd:org_0000000000000001:ai-chat:finance@v1.0.0:k1";

        // 先落一条本地记录
        s.put(key, "\"v1\"").unwrap();
        s.put(
            &format!("pmeta:{key}"),
            &serde_json::to_string(&DocMeta {
                vv: [("node-a".to_string(), 1)].into_iter().collect(),
                ts: 1000,
                node_id: Some("node-a".to_string()),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();

        // 入站远端墓碑（vv=2 领先）
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            "org_0000000000000001",
            "ai-chat:finance@v1.0.0",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: key.to_string(),
                value: serde_json::Value::Null,
                meta: DocMeta {
                    vv: [("node-a".to_string(), 2)].into_iter().collect(),
                    ts: 2000,
                    node_id: Some("node-a".to_string()),
                    tombstone: Some(true),
                },
                dseq: Some(3),
            }],
            0,
            1,
        );
        handle_orgsync_data(&mut s, &c, "member-a", &body).unwrap();

        // 个人域 dlog 为空
        let personal_dlog: Vec<_> = s
            .scan(&ScanOptions::prefix("dlog:entry:"))
            .unwrap()
            .into_iter()
            .collect();
        assert!(personal_dlog.is_empty(), "个人域 dlog 不被 orgd 污染");
        // org 域 dlog 已补登
        let entries = crate::sync::orgsync::org_dlog_entries_after(
            &s,
            "org_0000000000000001",
            "ai-chat:finance",
            "1.0.0",
            0,
        )
        .unwrap();
        assert_eq!(entries.len(), 1, "远端墓碑接力进 org dlog");
        assert_eq!(entries[0].1, key);
    }

    /// O2b 双线幂等合入：存量组织键（内建 all-members 集合）经 orgsync-data
    /// 到达，与既有同 vv 数据合并幂等——重复/并发双线（orgsync + pdsync）
    /// 到达不重复 bump vv、值不被旧版本覆盖。
    #[test]
    fn orgsync_data_builtin_collection_merges_idempotently() {
        let mut s = setup_org_and_collection();
        let online = std::collections::HashSet::new();
        let c = ctx("self", "peer-a", &online);
        // 声明内建 org:contacts 集合（all-members，声明记录 + pmeta）
        let org_id = "org_0000000000000001";
        let decl_key = crate::plugindata::org_decl_key(org_id, "org:contacts", "1");
        let decl = declare(
            &mut s,
            "org",
            crate::plugindata::DeclareInput {
                name: "org:contacts".to_string(),
                version: Some("1".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            1000,
            Some(org_id),
        )
        .unwrap();
        s.put(&decl_key, &serde_json::to_string(&decl).unwrap())
            .unwrap();
        s.put(
            &format!("pmeta:{decl_key}"),
            &serde_json::to_string(&meta("node-a", 1)).unwrap(),
        )
        .unwrap();

        // 存量键 ct:org:{orgId}:* 经 orgsync 到达（vv node-a=1）
        let key = "ct:org:org_0000000000000001:member-x";
        let remote_meta = DocMeta {
            vv: [("node-a".to_string(), 1)].into_iter().collect(),
            ts: 1500,
            node_id: Some("node-a".to_string()),
            ..Default::default()
        };
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            "org:contacts@v1",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: key.to_string(),
                value: json!("member-value"),
                meta: remote_meta.clone(),
                dseq: None,
            }],
            0,
            1,
        );
        handle_orgsync_data(&mut s, &c, "member-a", &body).unwrap();
        assert_eq!(s.get(key).unwrap().as_deref(), Some("\"member-value\""));
        let stored = crate::sync::get_personal_meta(&s, key).unwrap().unwrap();
        assert_eq!(stored.vv.get("node-a"), Some(&1));

        // 双线幂等：同 vv 再次到达（pdsync/orgsync 并发重放）→ 不重复 bump、
        // 值不翻转
        let body2 = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            "org:contacts@v1",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: key.to_string(),
                value: json!("member-value"),
                meta: remote_meta,
                dseq: None,
            }],
            0,
            1,
        );
        handle_orgsync_data(&mut s, &c, "member-a", &body2).unwrap();
        assert_eq!(s.get(key).unwrap().as_deref(), Some("\"member-value\""));
        let stored2 = crate::sync::get_personal_meta(&s, key).unwrap().unwrap();
        assert_eq!(
            stored2.vv.get("node-a"),
            Some(&1),
            "同 vv 重复到达不重复 bump"
        );
    }

    /// F5：远端合入**存量组织键**（内建集合键域 ct:org:）墓碑 → org dlog 与
    /// 个人域 dlog **双有**（与本地 tombstone_local 双写对称：orgsync 走 org
    /// dlog、pdsync 自设备同步走个人 dlog）。
    #[test]
    fn orgsync_data_legacy_key_tombstone_writes_both_dlogs() {
        let mut s = setup_org_and_collection();
        let online = std::collections::HashSet::new();
        let c = ctx("self", "peer-a", &online);
        let org_id = "org_0000000000000001";
        // 声明 org:contacts 内建集合（存量键域 ct:org:{orgId}:*）
        let decl_key = crate::plugindata::org_decl_key(org_id, "org:contacts", "1");
        let decl = declare(
            &mut s,
            "org",
            crate::plugindata::DeclareInput {
                name: "org:contacts".to_string(),
                version: Some("1".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            1000,
            Some(org_id),
        )
        .unwrap();
        s.put(&decl_key, &serde_json::to_string(&decl).unwrap())
            .unwrap();
        s.put(
            &format!("pmeta:{decl_key}"),
            &serde_json::to_string(&meta("node-a", 1)).unwrap(),
        )
        .unwrap();
        // 先落一条存量数据：声明 pmeta（node-a:1）已把 node-a 序号种子到 1，
        // 本次受管写拿到 per-node 序号 2 → vv={node-a:2}
        let key = "ct:org:org_0000000000000001:member-x";
        crate::sync::put_personal(&mut s, "node-a", key, "\"v1\"", 1000).unwrap();
        // 远端墓碑（vv=3 领先本地 2）经 orgsync-data 到达
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            "org:contacts@v1",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: key.to_string(),
                value: serde_json::Value::Null,
                meta: DocMeta {
                    vv: [("node-a".to_string(), 3)].into_iter().collect(),
                    ts: 2000,
                    node_id: Some("node-a".to_string()),
                    tombstone: Some(true),
                },
                dseq: Some(4),
            }],
            0,
            1,
        );
        handle_orgsync_data(&mut s, &c, "member-a", &body).unwrap();
        // org dlog 有（接力）
        let org_entries =
            crate::sync::orgsync::org_dlog_entries_after(&s, org_id, "org:contacts", "1", 0)
                .unwrap();
        assert_eq!(org_entries.len(), 1, "存量键墓碑登 org dlog");
        assert_eq!(org_entries[0].1, key);
        // 个人域 dlog 也有（pdsync 自设备同步）
        let personal_entries: Vec<_> = s
            .scan(&ScanOptions::prefix("dlog:entry:"))
            .unwrap()
            .into_iter()
            .collect();
        assert!(
            personal_entries.iter().any(|(_, v)| v == key),
            "存量键墓碑同时登个人域 dlog"
        );
    }

    /// F5 防双写幂等：同一存量键墓碑再次（同 vv）到达 → did_apply=false，
    /// 个人域 dlog 不重复补登（保持单条目）。
    #[test]
    fn orgsync_data_legacy_tombstone_does_not_double_log_personal() {
        let mut s = setup_org_and_collection();
        let online = std::collections::HashSet::new();
        let c = ctx("self", "peer-a", &online);
        let org_id = "org_0000000000000001";
        let decl_key = crate::plugindata::org_decl_key(org_id, "org:contacts", "1");
        let decl = declare(
            &mut s,
            "org",
            crate::plugindata::DeclareInput {
                name: "org:contacts".to_string(),
                version: Some("1".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            1000,
            Some(org_id),
        )
        .unwrap();
        s.put(&decl_key, &serde_json::to_string(&decl).unwrap())
            .unwrap();
        s.put(
            &format!("pmeta:{decl_key}"),
            &serde_json::to_string(&meta("node-a", 1)).unwrap(),
        )
        .unwrap();
        let key = "ct:org:org_0000000000000001:member-y";
        // 声明 pmeta（node-a:1）把 node-a 序号种子到 1，本地存量数据拿到序号 2
        crate::sync::put_personal(&mut s, "node-a", key, "\"v1\"", 1000).unwrap();
        // 远端墓碑 vv=3 领先本地 2 → 首达合入（登个人 dlog），同 vv 重放不重复登
        let tomb_meta = DocMeta {
            vv: [("node-a".to_string(), 3)].into_iter().collect(),
            ts: 2000,
            node_id: Some("node-a".to_string()),
            tombstone: Some(true),
        };
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            "org:contacts@v1",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: key.to_string(),
                value: serde_json::Value::Null,
                meta: tomb_meta.clone(),
                dseq: Some(4),
            }],
            0,
            1,
        );
        handle_orgsync_data(&mut s, &c, "member-a", &body).unwrap();
        // 同 vv 墓碑重放 → 不重复补登个人 dlog
        let body2 = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            "org:contacts@v1",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: key.to_string(),
                value: serde_json::Value::Null,
                meta: tomb_meta,
                dseq: Some(4),
            }],
            0,
            1,
        );
        handle_orgsync_data(&mut s, &c, "member-a", &body2).unwrap();
        let personal_entries: Vec<_> = s
            .scan(&ScanOptions::prefix("dlog:entry:"))
            .unwrap()
            .into_iter()
            .filter(|(_, v)| v == key)
            .collect();
        assert_eq!(personal_entries.len(), 1, "同 vv 重放不重复登个人 dlog");
    }

    /// O4 §20.7 acl 合入验签：owner（member-a，accessKey 已发布）签名的 acl
    /// 经 orgsync-data 到达 → 验签通过合入；篡改签名/非 owner 签名 → 拒绝保留
    /// 本地；未发布 accessKey 的成员发起的变更 → 拒绝并保留本地。
    #[test]
    fn orgsync_acl_verified_merge_positive_and_negative() {
        use crate::identity::derive_domain_identity;
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD as B64;

        let org_id = "org_0000000000000001";
        let name = "ai-chat:finance";
        let version = "1.0.0";
        let col_full = format!("{name}@v{version}");
        // owner = member-a：org-access 域身份（seed 派生）+ accessKey 发布
        let owner_seed = [7u8; 64];
        let owner_domain = derive_domain_identity(&owner_seed, &format!("org-access:{org_id}"));
        let owner_pk_b64 = B64.encode(owner_domain.public_key());
        let owner_root = "owner-a".to_string() + &"a".repeat(49);
        let self_root = "self-a".to_string() + &"a".repeat(49);
        let mut s = MemoryStorage::new();
        let record = OrganizationRecord {
            org_id: org_id.to_string(),
            name: "t".to_string(),
            description: String::new(),
            avatar: String::new(),
            base_plugin_domain: None,
            created_at: 1000,
            created_by: self_root.clone(),
            updated_at: 1000,
            members: vec![
                OrganizationMember {
                    root_id: owner_root.clone(),
                    role: OrganizationRole::Admin,
                    joined_at: 1000,
                    added_by: self_root.clone(),
                    node_info: None,
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: Some(crate::org::types::OrganizationAccessKey {
                        public_key: owner_pk_b64.clone(),
                        // bind_sig 由内核发布路径生成；入站验签以 accessKey 公钥
                        // 验 acl 签名，故测试填充任意非空串（结构上存在即可）
                        bind_sig: "bind".to_string(),
                    }),
                    extra: Default::default(),
                },
                OrganizationMember {
                    root_id: self_root.clone(),
                    role: OrganizationRole::Admin,
                    joined_at: 1000,
                    added_by: self_root.clone(),
                    node_info: None,
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: None,
                    extra: Default::default(),
                },
            ],
            sync: None,
            gateways: vec![],
            data_accounts: vec![],
            org_address: None,
            is_public: false,
            extra: Default::default(),
        };
        crate::org::OrganizationService::save_record(&mut s, &record).unwrap();
        // 声明 all-members 集合（复制组 = 全体成员）。O1：创世 acl 锚定
        // declaredBy——声明者（owner_root）须在声明记录中登记，测试对齐真实
        // 发布路径（kernel 强制 declaredBy=调用方）。
        let decl = declare(
            &mut s,
            "ai-chat",
            crate::plugindata::DeclareInput {
                name: name.to_string(),
                version: Some(version.to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                declared_by: Some(owner_root.clone()),
                ..Default::default()
            },
            1000,
            Some(org_id),
        )
        .unwrap();
        let _ = decl;

        let acl_key = crate::sync::orgsync::acl_key(org_id, name, version);
        let online = std::collections::HashSet::new();
        let c = ctx(&self_root, "peer-a", &online);

        // 构造 owner 签名的 acl（创世：owners=[owner]）
        let make_acl = |epoch: u64, owners: Vec<String>, readers: Vec<String>, updated_at: i64| {
            let payload = crate::sync::orgsync::acl_sign_payload(
                epoch, org_id, &col_full, &owners, &readers, None, updated_at,
            );
            let sig = crate::sync::orgsync::acl_sign(&owner_domain.signing_key, &payload);
            serde_json::json!({
                "owners": owners,
                "readers": readers,
                "epoch": epoch,
                "updatedAt": updated_at,
                "sig": sig,
            })
        };
        // (a) 正向：owner 签名 acl 合入（owners=[owner, self]，self 也是 owner
        // 但无 accessKey——供 (c) 走「无 accessKey」分支）
        let value = make_acl(
            1,
            vec![owner_root.clone(), self_root.clone()],
            vec![owner_root.clone()],
            100,
        );
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            &col_full,
            &[crate::sync::orgsync::OrgsyncRecord {
                key: acl_key.clone(),
                value: value.clone(),
                meta: meta("node-a", 1),
                dseq: None,
            }],
            0,
            1,
        );
        let r = handle_orgsync_data(&mut s, &c, &owner_root, &body).unwrap();
        assert_eq!(r.response["ok"], json!(true), "owner 签名 acl 合入");
        let stored: crate::sync::orgsync::AclRecord =
            serde_json::from_str(&s.get(&acl_key).unwrap().unwrap()).unwrap();
        assert!(stored.is_owner(&owner_root), "合入 acl owner 正确");

        // (b) 负向：先按合法 readers 签名，再篡改 readers 字段（签名不再匹配）→
        // acl-bad-signature 拒绝，本地保留
        let mut tampered = make_acl(2, vec![owner_root.clone()], vec![owner_root.clone()], 200);
        tampered["readers"] = json!(["evil"]);
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            &col_full,
            &[crate::sync::orgsync::OrgsyncRecord {
                key: acl_key.clone(),
                value: tampered,
                meta: meta("node-a", 2),
                dseq: None,
            }],
            0,
            1,
        );
        let r2 = handle_orgsync_data(&mut s, &c, &owner_root, &body).unwrap();
        assert_eq!(r2.response["ok"], json!(false));
        assert_eq!(r2.response["reason"], json!("acl-bad-signature"));
        let stored2: crate::sync::orgsync::AclRecord =
            serde_json::from_str(&s.get(&acl_key).unwrap().unwrap()).unwrap();
        assert_eq!(stored2.epoch, 1, "篡改 acl 拒绝，本地保留 epoch=1");

        // (c) 负向：未发布 accessKey 的成员（self 本机无 accessKey）发起的变更
        // → acl-signer-no-access-key 拒绝，本地保留
        let self_payload = crate::sync::orgsync::acl_sign_payload(
            1,
            org_id,
            &col_full,
            &[self_root.clone()],
            &[self_root.clone()],
            None,
            300,
        );
        // 用 owner 的域身份签（self 无域身份可用；重点是走「无 accessKey」分支）
        let self_sig = crate::sync::orgsync::acl_sign(&owner_domain.signing_key, &self_payload);
        let self_value = serde_json::json!({
            "owners": [self_root],
            "readers": [self_root],
            "epoch": 1,
            "updatedAt": 300,
            "sig": self_sig,
        });
        let body3 = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            &col_full,
            &[crate::sync::orgsync::OrgsyncRecord {
                key: acl_key.clone(),
                value: self_value,
                meta: meta("node-self", 3),
                dseq: None,
            }],
            0,
            1,
        );
        let r3 = handle_orgsync_data(&mut s, &c, &self_root, &body3).unwrap();
        assert_eq!(r3.response["ok"], json!(false));
        assert_eq!(r3.response["reason"], json!("acl-signer-no-access-key"));
        let stored3: crate::sync::orgsync::AclRecord =
            serde_json::from_str(&s.get(&acl_key).unwrap().unwrap()).unwrap();
        assert_eq!(stored3.epoch, 1, "无 accessKey 变更拒绝，本地保留");
    }

    /// O1 创世锚 + 时间窗 + epoch 单调性：
    /// - 创世 acl 签名者必须是声明记录 declaredBy（非声明者抢先自签创世 → 拒绝）；
    /// - 合入 acl 的 updatedAt 与本地时钟偏差超窗 → 拒绝；
    /// - 合入 acl 的 epoch 低于本地当前 epoch（非 reset）→ 拒绝降级。
    #[test]
    fn orgsync_acl_genesis_anchor_time_window_and_epoch_monotonic() {
        use crate::identity::derive_domain_identity;
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD as B64;

        let org_id = "org_0000000000000001";
        let name = "ai-chat:fin2";
        let version = "1.0.0";
        let col_full = format!("{name}@v{version}");
        // 声明者 = owner（declaredBy），攻击者 = other（有 accessKey 但非声明者）
        let owner_seed = [21u8; 64];
        let other_seed = [22u8; 64];
        let owner_domain = derive_domain_identity(&owner_seed, &format!("org-access:{org_id}"));
        let other_domain = derive_domain_identity(&other_seed, &format!("org-access:{org_id}"));
        let owner_root = "owner-b".to_string() + &"b".repeat(48);
        let other_root = "other-c".to_string() + &"c".repeat(48);
        let self_root = "self-d".to_string() + &"d".repeat(48);

        let mut s = MemoryStorage::new();
        let record = OrganizationRecord {
            org_id: org_id.to_string(),
            name: "t".to_string(),
            description: String::new(),
            avatar: String::new(),
            base_plugin_domain: None,
            created_at: 1000,
            created_by: self_root.clone(),
            updated_at: 1000,
            members: vec![
                OrganizationMember {
                    root_id: owner_root.clone(),
                    role: OrganizationRole::Member,
                    joined_at: 1000,
                    added_by: self_root.clone(),
                    node_info: None,
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: Some(crate::org::types::OrganizationAccessKey {
                        public_key: B64.encode(owner_domain.public_key()),
                        bind_sig: "bind".to_string(),
                    }),
                    extra: Default::default(),
                },
                OrganizationMember {
                    root_id: other_root.clone(),
                    role: OrganizationRole::Member,
                    joined_at: 1000,
                    added_by: self_root.clone(),
                    node_info: None,
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: Some(crate::org::types::OrganizationAccessKey {
                        public_key: B64.encode(other_domain.public_key()),
                        bind_sig: "bind".to_string(),
                    }),
                    extra: Default::default(),
                },
                OrganizationMember {
                    root_id: self_root.clone(),
                    role: OrganizationRole::Member,
                    joined_at: 1000,
                    added_by: self_root.clone(),
                    node_info: None,
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: None,
                    extra: Default::default(),
                },
            ],
            sync: None,
            gateways: vec![],
            data_accounts: vec![],
            org_address: None,
            is_public: false,
            extra: Default::default(),
        };
        crate::org::OrganizationService::save_record(&mut s, &record).unwrap();
        // 声明集合（declaredBy = owner_root）
        declare(
            &mut s,
            "ai-chat",
            crate::plugindata::DeclareInput {
                name: name.to_string(),
                version: Some(version.to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                declared_by: Some(owner_root.clone()),
                ..Default::default()
            },
            1000,
            Some(org_id),
        )
        .unwrap();

        let acl_key = crate::sync::orgsync::acl_key(org_id, name, version);
        let online = std::collections::HashSet::new();
        // ctx now_ms = 2000（create_ctx 固定）——时间窗 ±10min 内
        let c = ctx(&self_root, "peer-a", &online);

        let make_acl = |signing: &ed25519_dalek::SigningKey,
                        epoch: u64,
                        owners: Vec<String>,
                        readers: Vec<String>,
                        updated_at: i64| {
            let payload = crate::sync::orgsync::acl_sign_payload(
                epoch, org_id, &col_full, &owners, &readers, None, updated_at,
            );
            let sig = crate::sync::orgsync::acl_sign(signing, &payload);
            serde_json::json!({
                "owners": owners,
                "readers": readers,
                "epoch": epoch,
                "updatedAt": updated_at,
                "sig": sig,
            })
        };
        let deliver_acl = |s: &mut MemoryStorage, from: &str, value: Value| {
            let body = crate::sync::orgsync::build_orgsync_data_batch(
                org_id,
                &col_full,
                &[crate::sync::orgsync::OrgsyncRecord {
                    key: acl_key.clone(),
                    value,
                    meta: meta("node-x", 1),
                    dseq: None,
                }],
                0,
                1,
            );
            handle_orgsync_data(s, &c, from, &body).unwrap()
        };

        // (1) 创世抢注反例：非声明者（other）抢先自签创世 acl → 拒绝
        let squatter = make_acl(
            &other_domain.signing_key,
            1,
            vec![other_root.clone()],
            vec![other_root.clone()],
            1500,
        );
        let r1 = deliver_acl(&mut s, &other_root, squatter);
        assert_eq!(r1.response["ok"], json!(false));
        assert_eq!(
            r1.response["reason"],
            json!("acl-genesis-signer-not-declared-by")
        );
        assert!(s.get(&acl_key).unwrap().is_none(), "创世抢注 acl 不落库");

        // (2) 正向：声明者（owner）创世 acl 合入
        let genesis = make_acl(
            &owner_domain.signing_key,
            1,
            vec![owner_root.clone()],
            vec![owner_root.clone()],
            1500,
        );
        let r2 = deliver_acl(&mut s, &owner_root, genesis);
        assert_eq!(r2.response["ok"], json!(true), "声明者创世 acl 合入");

        // (3) 时间窗外（真正超窗）：updatedAt = now - 11min → 拒绝
        let stale_ts = 2000 - super::acl::ACL_TS_WINDOW_MS - 1;
        let stale = make_acl(
            &owner_domain.signing_key,
            2,
            vec![owner_root.clone()],
            vec![owner_root.clone(), other_root.clone()],
            stale_ts,
        );
        let r3 = deliver_acl(&mut s, &owner_root, stale);
        assert_eq!(r3.response["ok"], json!(false));
        assert_eq!(r3.response["reason"], json!("acl-ts-out-of-window"));
        let stored: crate::sync::orgsync::AclRecord =
            serde_json::from_str(&s.get(&acl_key).unwrap().unwrap()).unwrap();
        assert_eq!(stored.epoch, 1, "时间窗外 acl 拒绝，保留本地 epoch=1");

        // (4) epoch 回退：owner 提交 epoch=1（< 当前 1？需 < 当前）——
        // 当前 epoch=1，提交 epoch=0 非 reset → acl-epoch-regress 拒绝
        let regress = make_acl(
            &owner_domain.signing_key,
            0,
            vec![owner_root.clone()],
            vec![owner_root.clone()],
            1900,
        );
        let r4 = deliver_acl(&mut s, &owner_root, regress);
        assert_eq!(r4.response["ok"], json!(false));
        assert_eq!(r4.response["reason"], json!("acl-epoch-regress"));
    }
