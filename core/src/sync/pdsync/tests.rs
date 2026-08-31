//! pdsync 单元测试（实现见 ../pdsync.rs；本文件由 pdsync.rs 以 #[cfg(test)] mod tests 挂载。

    use super::*;
    use crate::storage::MemoryStorage;
    use crate::sync::personal::{
        apply_personal_remote, delete_personal, is_tombstone, put_personal,
    };

    const NODE_A: &str = "node-a";
    const NODE_B: &str = "node-b";
    const FRIEND_PREFIX: &str = "ct:friend:";

    fn category_friend() -> &'static Category {
        CATEGORIES
            .iter()
            .find(|c| c.name == "ct:friend")
            .unwrap()
    }

    #[test]
    fn category_for_key_epoch_state() {
        // epoch:state 必须归属 pdsync 的 epoch category，否则不会进入 hello/need/data diff。
        let cat = category_for_key("epoch:state").expect("epoch:state should be categorized");
        assert_eq!(cat.name, "epoch");
    }

    #[test]
    fn category_for_key_ikey_takes_epoch_category() {
        // ikey: 也走 epoch category，且较长前缀 epoch: 不能误吞 ikey:。
        let cat = category_for_key("ikey:3:alice:bob").expect("ikey should be categorized");
        assert_eq!(cat.name, "epoch");
    }

    #[test]
    fn collect_folds_max_across_records() {
        let mut s = MemoryStorage::new();
        // A 写两条朋友，B 在其中一条上再改
        put_personal(&mut s, NODE_A, &format!("{FRIEND_PREFIX}a"), "1", 1000).unwrap();
        put_personal(&mut s, NODE_A, &format!("{FRIEND_PREFIX}b"), "2", 1000).unwrap();
        let meta_b = put_personal(&mut s, NODE_B, &format!("{FRIEND_PREFIX}b"), "3", 2000).unwrap();
        assert_eq!(meta_b.vv.get(NODE_B), Some(&1));

        let folded = collect_category_vv(&s, category_friend(), None).unwrap();
        // per-node 序号：A 两条写依次为 A:1/A:2；b 上 B 再改为 B:1 → 折叠 A:2, B:1
        assert_eq!(folded.get(NODE_A), Some(&2));
        assert_eq!(folded.get(NODE_B), Some(&1));
    }

    #[test]
    fn diff_detects_local_behind_ahead_equal() {
        // 本地 {A:1}，对端 {A:2} → 落后
        let local: VersionVector = [(NODE_A.to_string(), 1)].into_iter().collect();
        let remote: VersionVector = [(NODE_A.to_string(), 2)].into_iter().collect();
        assert!(matches!(
            diff_category(&local, &remote),
            DiffOutcome::LocalBehind { .. }
        ));

        // 本地 {A:2}，对端 {A:1} → 领先
        let remote_older: VersionVector = [(NODE_A.to_string(), 1)].into_iter().collect();
        assert!(matches!(
            diff_category(&remote, &remote_older),
            DiffOutcome::LocalAhead
        ));

        // 相等 → Equal
        assert!(matches!(
            diff_category(&remote, &remote),
            DiffOutcome::Equal
        ));
    }

    #[test]
    fn collect_incremental_filters_by_known_vv() {
        let mut s = MemoryStorage::new();
        // A 写两条，B 在其一上再改
        // 记录本体需为合法 JSON（损坏记录会被跳过不推，防 null 覆盖对端）
        put_personal(&mut s, NODE_A, &format!("{FRIEND_PREFIX}a"), r#""A1""#, 1000).unwrap();
        put_personal(&mut s, NODE_A, &format!("{FRIEND_PREFIX}b"), r#""B1""#, 1000).unwrap();
        put_personal(&mut s, NODE_B, &format!("{FRIEND_PREFIX}b"), r#""B2""#, 2000).unwrap();

        // knownVv = {A:2, B:1}（对端已齐全：A 两条写 + B 对 b 的更新）→ 无增量
        let known_full: VersionVector =
            [(NODE_A.to_string(), 2), (NODE_B.to_string(), 1)].into_iter().collect();
        let inc = collect_incremental(&s, category_friend(), &known_full, None, 0).unwrap();
        assert!(inc.is_empty());

        // knownVv = {A:1}（对端只见 A 第 1 条，缺 A 对 b 的第 2 条写 + B 的更新）
        // → 只推 b（vv {A:2, B:1} 两分量均领先）
        let known_a: VersionVector = [(NODE_A.to_string(), 1)].into_iter().collect();
        let inc = collect_incremental(&s, category_friend(), &known_a, None, 0).unwrap();
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].key, format!("{FRIEND_PREFIX}b"));
        // b 的 meta vv 含 B 分量（相对 known_a 是 Remote）
        assert_eq!(inc[0].meta.vv.get(NODE_B), Some(&1));

        // knownVv 空 → 全部增量（两条都要）
        let inc = collect_incremental(&s, category_friend(), &VersionVector::new(), None, 0).unwrap();
        assert_eq!(inc.len(), 2);
    }

    #[test]
    fn apply_data_is_idempotent() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();

        put_personal(&mut a, NODE_A, &format!("{FRIEND_PREFIX}a"), "A1", 1000).unwrap();
        let meta = get_personal_meta(&a, &format!("{FRIEND_PREFIX}a")).unwrap().unwrap();

        // A 发 data 给 B：B 采纳
        let rec = PdsyncRecord {
            key: format!("{FRIEND_PREFIX}a"),
            value: json!("A1"),
            meta: meta.clone(),
            dseq: None,
        };
        let r = apply_personal_remote(&mut b, &rec.key, &rec.value.to_string(), &rec.meta).unwrap();
        assert_eq!(r, crate::sync::personal::ApplyResult::Applied);

        // 重放同一 data：Equal，幂等
        let r = apply_personal_remote(&mut b, &rec.key, &rec.value.to_string(), &rec.meta).unwrap();
        assert_eq!(r, crate::sync::personal::ApplyResult::Equal);
    }

    #[test]
    fn build_parse_roundtrip_hello() {
        let mut s = MemoryStorage::new();
        put_personal(&mut s, NODE_A, &format!("{FRIEND_PREFIX}a"), "1", 1000).unwrap();

        let hello = build_hello(&s, 2_592_000_000, 500, "eager", None, None).unwrap();
        let cats = parse_hello_categories(&hello);
        let friend_vv = cats.get("ct:friend").unwrap();
        assert_eq!(friend_vv.get(NODE_A), Some(&1));
        // 其余 category 存在（空 vv）
        for c in CATEGORIES {
            assert!(cats.contains_key(c.name), "category {} 缺失", c.name);
        }
    }

    #[test]
    fn build_parse_roundtrip_need_and_data() {
        let mut s = MemoryStorage::new();
        put_personal(&mut s, NODE_A, &format!("{FRIEND_PREFIX}a"), r#""v""#, 1000)
            .unwrap();
        let known: VersionVector = VersionVector::new();
        let inc = collect_incremental(&s, category_friend(), &known, None, 0).unwrap();

        let need = build_need("ct:friend", &known, 7);
        let (cat, parsed_vv, ack) = parse_need(&need).unwrap();
        assert_eq!(cat, "ct:friend");
        assert!(parsed_vv.is_empty());
        assert_eq!(ack, 7, "dlogAck 往返一致");

        let data = build_data_batch("ct:friend", &inc, 0, 1);
        let (cat2, records) = parse_data(&data).unwrap();
        assert_eq!(cat2, "ct:friend");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, format!("{FRIEND_PREFIX}a"));
    }

    #[test]
    fn split_batches_respects_limit() {
        let mut s = MemoryStorage::new();
        for i in 0..10 {
            let key = format!("{FRIEND_PREFIX}{i}");
            let val = format!("\"user-{i}-{}\"", "x".repeat(100));
            put_personal(&mut s, NODE_A, &key, &val, 1000).unwrap();
        }
        let inc = collect_incremental(&s, category_friend(), &VersionVector::new(), None, 0).unwrap();
        let batches = split_batches(inc, 300);
        assert!(batches.len() > 1, "应切分为多批，实际 {}", batches.len());
        // 所有记录都覆盖到
        let total: usize = batches.iter().map(|b| b.len()).sum();
        assert_eq!(total, 10);
    }

    /// 端到端：两台设备（node-a / node-b）各自写入互不知情的数据，
    /// 通过 hello→need→data 三信封收敛，最终双方记录一致。
    ///
    /// 模拟协议：
    /// 1. A 发 hello（折叠 vv + dlogAck）给 B；
    /// 2. B 比对 → B 落后于 A 的类别发 need 回 A；
    /// 3. A 收到 need → 回 data；
    /// 4. B 应用 data → 双方一致。
    ///
    /// 删除日志 ACK 建模：`receiver_seen`/`sender_seen` 分别为两侧已收对端
    /// 日志的最大 dseq——hello 的 dlogAck 由调用方注入（= sender_seen），
    /// need 携带 receiver_seen；data 中墓碑条目的 dseq 回推更新两个计数器。
    ///
    /// `exclude`：对称排除键（[`self_friend_key`] 自记录排除的端到端验证用，
    /// 其余测试传 `None`）。
    fn exchange(
        sender: &MemoryStorage,
        receiver: &mut MemoryStorage,
        hello: &Value,
        exclude: Option<&str>,
        receiver_seen: &mut u64,
        sender_seen: &mut u64,
    ) -> Vec<Value> {
        // receiver 处理 hello：产生 need/data 出站 body
        let hello_ack = crate::sync::dlog::parse_dlog_ack(hello);
        let remote_cats = parse_hello_categories(hello);
        let mut out = Vec::new();
        for category in CATEGORIES {
            let local_vv = collect_category_vv(receiver, category, exclude).unwrap_or_default();
            let remote_vv = remote_cats.get(category.name).cloned().unwrap_or_default();
            match diff_category(&local_vv, &remote_vv) {
                DiffOutcome::LocalBehind { local_vv } => {
                    out.push(build_need(category.name, &local_vv, *receiver_seen));
                }
                DiffOutcome::LocalAhead => {
                    if let Ok(records) =
                        collect_incremental(receiver, category, &remote_vv, exclude, hello_ack)
                    {
                        *sender_seen = records
                            .iter()
                            .filter_map(|r| r.dseq)
                            .fold(*sender_seen, u64::max);
                        let batches = split_batches(records, 4096);
                        let total = batches.len();
                        for (i, b) in batches.into_iter().enumerate() {
                            out.push(build_data_batch(category.name, &b, i, total));
                        }
                    }
                }
                DiffOutcome::Concurrent => {
                    // 双向：推本机缺的 + 请求对端缺的
                    if let Ok(records) =
                        collect_incremental(receiver, category, &remote_vv, exclude, hello_ack)
                    {
                        *sender_seen = records
                            .iter()
                            .filter_map(|r| r.dseq)
                            .fold(*sender_seen, u64::max);
                        let batches = split_batches(records, 4096);
                        let total = batches.len();
                        for (i, b) in batches.into_iter().enumerate() {
                            out.push(build_data_batch(category.name, &b, i, total));
                        }
                    }
                    out.push(build_need(category.name, &local_vv, *receiver_seen));
                }
                DiffOutcome::Equal => {
                    // 折叠 vv Equal 不代表对端收齐墓碑：按 ACK 游标补推
                    if let Ok(tombs) =
                        collect_tombstones_after(receiver, category, exclude, hello_ack)
                    {
                        *sender_seen = tombs
                            .iter()
                            .filter_map(|r| r.dseq)
                            .fold(*sender_seen, u64::max);
                        let batches = split_batches(tombs, 4096);
                        let total = batches.len();
                        for (i, b) in batches.into_iter().enumerate() {
                            out.push(build_data_batch(category.name, &b, i, total));
                        }
                    }
                }
            }
        }
        // 处理 need：sender 侧采集并回 data（在真实链路由 sender 处理）
        let mut responses = Vec::new();
        for body in out {
            if let Some((cat_name, known_vv, need_ack)) = parse_need(&body) {
                let cat = category_by_name(&cat_name).unwrap();
                let records =
                    collect_incremental(sender, cat, &known_vv, exclude, need_ack).unwrap();
                *receiver_seen = records
                    .iter()
                    .filter_map(|r| r.dseq)
                    .fold(*receiver_seen, u64::max);
                let batches = split_batches(records, 4096);
                let total = batches.len();
                for (i, b) in batches.into_iter().enumerate() {
                    responses.push(build_data_batch(&cat_name, &b, i, total));
                }
            }
        }
        responses
    }

    /// 无删除场景的简化包装：ACK 计数器用一次性 dummy（无墓碑即无 dseq，
    /// 回执不影响行为）。
    fn exchange_simple(
        sender: &MemoryStorage,
        receiver: &mut MemoryStorage,
        hello: &Value,
        exclude: Option<&str>,
    ) -> Vec<Value> {
        exchange(sender, receiver, hello, exclude, &mut 0, &mut 0)
    }

    #[test]
    fn two_device_exchange_converges() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();

        // A 写 3 条朋友，B 写 1 条不同的朋友
        for i in 0..3 {
            put_personal(
                &mut a,
                NODE_A,
                &format!("{FRIEND_PREFIX}a{i}"),
                &format!(r#""friend-a{i}""#),
                1000,
            )
            .unwrap();
        }
        put_personal(
            &mut b,
            NODE_B,
            &format!("{FRIEND_PREFIX}b0"),
            r#""friend-b0""#,
            2000,
        )
        .unwrap();

        // A → B：A 领先（B 缺 A 的 3 条），B 发 need，A 回 data，B 应用
        let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", None, None).unwrap();
        let responses = exchange_simple(&a, &mut b, &hello_a, None);
        for data in &responses {
            let (_, records) = parse_data(data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        // B 现在应有 a0,a1,a2（来自 A，per-node 序号各为 A:1..A:3，折叠 A:3）+ b0（自己）
        assert_eq!(collect_category_vv(&b, category_friend(), None).unwrap().get(NODE_A), Some(&3));
        assert_eq!(collect_category_vv(&b, category_friend(), None).unwrap().get(NODE_B), Some(&1));

        // 反向 B → A：A 缺 b0，B 领先，A 发 need，B 回 data，A 应用
        let hello_b = build_hello(&b, 2_592_000_000, 500, "eager", None, None).unwrap();
        let responses_b = exchange_simple(&b, &mut a, &hello_b, None);
        for data in &responses_b {
            let (_, records) = parse_data(data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut a, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        // A 也有 b0 了
        assert_eq!(collect_category_vv(&a, category_friend(), None).unwrap().get(NODE_B), Some(&1));

        // 收敛后再互发 hello → 均 Equal，无新响应
        let hello_a2 = build_hello(&a, 2_592_000_000, 500, "eager", None, None).unwrap();
        let responses_a2 = exchange_simple(&a, &mut b, &hello_a2, None);
        assert!(responses_a2.is_empty(), "收敛后不应有 need/data");
        let hello_b2 = build_hello(&b, 2_592_000_000, 500, "eager", None, None).unwrap();
        let responses_b2 = exchange_simple(&b, &mut a, &hello_b2, None);
        assert!(responses_b2.is_empty(), "收敛后不应有 need/data");
    }

    /// P2：个人资料 `profile:self` + 会话元数据 `msg:conv:personal:` 作为独立
    /// category 折叠，且双向交换收敛（A 改资料 + 置顶会话，B 改昵称 + 草稿）。
    #[test]
    fn two_device_exchange_converges_profile_and_conv() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();

        let profile_key = "profile:self";
        let conv_key = "msg:conv:personal:c1";
        // A：昵称 + 会话置顶
        put_personal(
            &mut a,
            NODE_A,
            profile_key,
            r#"{"nickname":"甲","avatar":"","gender":"","region":"","signature":""}"#,
            1000,
        )
        .unwrap();
        put_personal(
            &mut a,
            NODE_A,
            conv_key,
            r#"{"id":"c1","kind":"Direct","title":"peer","peerRootId":"peer","peer":null,"unreadCount":0,"pinnedAt":1000,"muted":false,"draft":"","updatedAt":0,"metaUpdatedAt":1000}"#,
            1000,
        )
        .unwrap();
        // B：昵称 + 会话草稿
        put_personal(
            &mut b,
            NODE_B,
            profile_key,
            r#"{"nickname":"乙","avatar":"","gender":"","region":"","signature":""}"#,
            2000,
        )
        .unwrap();
        put_personal(
            &mut b,
            NODE_B,
            conv_key,
            r#"{"id":"c1","kind":"Direct","title":"peer","peerRootId":"peer","peer":null,"unreadCount":0,"pinnedAt":0,"muted":false,"draft":"草稿","updatedAt":0,"metaUpdatedAt":2000}"#,
            2000,
        )
        .unwrap();

        // A → B 交换：B 落后于 A 的 profile（A 先写），并发于 conv（各自改不同
        // 字段）。双向交换后双方各取所需。
        let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", None, None).unwrap();
        let responses = exchange_simple(&a, &mut b, &hello_a, None);
        for data in responses {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        // B → A 反向
        let hello_b = build_hello(&b, 2_592_000_000, 500, "eager", None, None).unwrap();
        let responses_b = exchange_simple(&b, &mut a, &hello_b, None);
        for data in responses_b {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut a, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }

        // 单条 profile:self / msg:conv 是"单记录 category"——并发写冲突时
        // 只有 LWW 胜者保留（这里 B 后写 ts 更大 → B 胜），双方收敛到同一
        // 胜者，vv 只含胜者分量（B）。这是 LWW 的确定性收敛，非数据丢失。
        for s in [&a, &b] {
            // profile：B 胜（B 昵称 "乙"）
            let prof_vv = collect_category_vv(s, category_named("profile:self"), None).unwrap();
            assert_eq!(prof_vv.get(NODE_A), None, "A 的 profile 编辑被 LWW 丢弃");
            assert_eq!(prof_vv.get(NODE_B), Some(&1));
            // conv：B 胜（B 草稿，ts 更大）；conv 是 B 的第 2 次受管写
            // （profile 已耗 B:1）→ per-node 序号 B:2
            let conv_vv = collect_category_vv(s, category_named("msg:conv"), None).unwrap();
            assert_eq!(conv_vv.get(NODE_A), None, "A 的 conv 编辑被 LWW 丢弃");
            assert_eq!(conv_vv.get(NODE_B), Some(&2));
        }
        // 双方实际数据一致：profile 昵称 = 乙，conv 草稿 = "草稿"
        let profile_raw = get_personal_meta(&a, profile_key).unwrap().unwrap();
        assert_eq!(profile_raw.vv.get(NODE_B), Some(&1));
        // 收敛后再互发 hello → 无新响应
        let hello_a2 = build_hello(&a, 2_592_000_000, 500, "eager", None, None).unwrap();
        assert!(exchange_simple(&a, &mut b, &hello_a2, None).is_empty(), "profile/conv 收敛后无增量");
    }

    // ── P4 消息窗口 ─────────────────────────────────────────────────

    /// 构造一条消息记录 JSON（字段对齐 `MessageRecord` 的 camelCase；
    /// `type` 用小写，因 `MessageType` 以 lowercase 序列化）。
    fn message_json(id: &str, created_at: i64, recalled: bool) -> String {
        format!(
            r#"{{"id":"{id}","senderId":"u1","senderName":"u1","type":"text","content":"{id}","createdAt":{created_at},"status":null,"recalled":{recalled},"read":false}}"#
        )
    }

    #[test]
    fn message_conv_id_parses_simple_and_nested() {
        let k1 = crate::message::types::message_key("personal", "peer1", 1234567890123, "m1");
        assert_eq!(message_conv_id(&k1).unwrap(), "peer1");
        // convId 含 `:`（应用会话 app:pluginId）
        let k2 = crate::message::types::message_key("personal", "app:plug", 1234567890123, "m1");
        assert_eq!(message_conv_id(&k2).unwrap(), "app:plug");
    }

    #[test]
    fn collect_message_window_clips_by_count_and_age() {
        let mut s = MemoryStorage::new();
        let now = crate::p2p::node::system_now_ms();
        // 采集按会话枚举：conv 记录是消息归属的入口
        s.put("msg:conv:personal:c1", "{}").unwrap();
        // 5 条消息，时间从旧到新
        for i in 0..5 {
            let key = crate::message::types::message_key(
                "personal",
                "c1",
                now - 1000 * (5 - i as i64),
                &format!("m{i}"),
            );
            let val = message_json(&format!("m{i}"), now - 1000 * (5 - i as i64), false);
            s.put(&key, &val).unwrap();
        }
        // 窗口：每 conv 3 条，时间全收 → 取最新 3 条（m2,m3,m4）
        let w = MessageWindow { max_per_conv: 3, max_age_ms: i64::MAX, msg_sync_after: None };
        let recs = collect_message_window(&s, &w).unwrap();
        assert_eq!(recs.len(), 3);
        // 最新 3 条是 m2,m3,m4
        for r in &recs {
            let last_seg = r.key.rsplit(':').next().unwrap();
            assert!(
                matches!(last_seg, "m2" | "m3" | "m4"),
                "窗口应只含最新 3 条，实为 {last_seg}"
            );
        }
    }

    /// 时间下界裁剪：同一会话部分消息在窗口外（更老）、部分在窗口内——
    /// 窗口外旧消息裁掉、窗口内新消息必须全部保留（回归：迭代方向与
    /// break 语义错配会把窗口内新消息一并丢弃）。
    #[test]
    fn collect_message_window_age_cutoff_keeps_in_window_messages() {
        let mut s = MemoryStorage::new();
        let now = crate::p2p::node::system_now_ms();
        s.put("msg:conv:personal:c1", "{}").unwrap();
        let hour = 3_600_000i64;
        // m0=-3h, m1=-2h 在 1h 窗口外；m2=-30m, m3=-10m, m4=now 在窗口内
        let offsets = [-3 * hour, -2 * hour, -hour / 2, -hour / 6, 0];
        for (i, off) in offsets.iter().enumerate() {
            let key =
                crate::message::types::message_key("personal", "c1", now + off, &format!("m{i}"));
            s.put(&key, &message_json(&format!("m{i}"), now + off, false))
                .unwrap();
        }
        let w = MessageWindow { max_per_conv: 100, max_age_ms: hour, msg_sync_after: None };
        let recs = collect_message_window(&s, &w).unwrap();
        let ids: Vec<&str> = recs
            .iter()
            .map(|r| r.key.rsplit(':').next().unwrap())
            .collect();
        assert_eq!(ids, ["m2", "m3", "m4"], "窗口外裁掉、窗口内全保留");
    }

    #[test]
    fn apply_message_record_writes_item_and_byid() {
        let mut s = MemoryStorage::new();
        let now = 1_000_000_000;
        let key = crate::message::types::message_key("personal", "c1", now, "m1");
        let val = message_json("m1", now, false);
        apply_message_record(&mut s, &key, &val).unwrap();
        // 消息本体 + byid 索引
        assert!(s.get(&key).unwrap().is_some());
        let idx = crate::message::types::message_id_index_key("personal", "c1", "m1");
        assert_eq!(s.get(&idx).unwrap().unwrap(), key);
    }

    #[test]
    fn apply_message_record_overwrites_on_recall() {
        let mut s = MemoryStorage::new();
        let now = 1_000_000_000;
        let key = crate::message::types::message_key("personal", "c1", now, "m1");
        // 先落普通消息
        apply_message_record(&mut s, &key, &message_json("m1", now, false)).unwrap();
        let before: crate::message::MessageRecord =
            serde_json::from_str(&s.get(&key).unwrap().unwrap()).unwrap();
        assert!(!before.recalled);
        // 撤回传播：覆盖为 recalled=true
        apply_message_record(&mut s, &key, &message_json("m1", now, true)).unwrap();
        let after: crate::message::MessageRecord =
            serde_json::from_str(&s.get(&key).unwrap().unwrap()).unwrap();
        assert!(after.recalled);
    }

    /// recalled 只增不减（§6.2）：本地已撤回，对端窗口里的撤回前旧快照
    /// （recalled=false）不得复活内容。
    #[test]
    fn apply_message_record_recall_is_sticky() {
        let mut s = MemoryStorage::new();
        let now = 1_000_000_000;
        let key = crate::message::types::message_key("personal", "c1", now, "m1");
        // 先落撤回后的记录
        apply_message_record(&mut s, &key, &message_json("m1", now, true)).unwrap();
        // 撤回前旧快照后到达（窗口推送乱序）→ recalled 保持 true
        apply_message_record(&mut s, &key, &message_json("m1", now, false)).unwrap();
        let after: crate::message::MessageRecord =
            serde_json::from_str(&s.get(&key).unwrap().unwrap()).unwrap();
        assert!(after.recalled, "recalled 只增不减，旧快照不得回退");
        // byid 索引仍指向该消息
        let idx = crate::message::types::message_id_index_key("personal", "c1", "m1");
        assert_eq!(s.get(&idx).unwrap().unwrap(), key);
    }

    // ── msg:app 窗口同步（§6.2：与 msg:item 同窗口）─────────────────

    /// 构造一条应用消息记录 JSON（字段对齐 `AppMessageRecord` 的 camelCase）。
    fn app_message_json(id: &str, plugin_id: &str, created_at: i64) -> String {
        format!(
            r#"{{"id":"{id}","pluginId":"{plugin_id}","summary":"s","payload":{{"summary":"s"}},"createdAt":{created_at},"status":"local"}}"#
        )
    }

    #[test]
    fn message_conv_id_parses_app_key() {
        let k = crate::message::types::app_message_key("personal", "plug", 1234567890123, "m1");
        assert_eq!(message_conv_id(&k).unwrap(), "app:plug");
    }

    /// msg:app 采集 + 接收 roundtrip：应用消息随窗口推出并在对端落盘，
    /// 不建 byid 索引（与本地 `append_app_message` 一致）。
    #[test]
    fn app_message_window_collect_apply_roundtrip() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        let now = crate::p2p::node::system_now_ms();
        // A：应用会话 + 一条应用消息
        a.put("msg:conv:personal:app:plug", "{}").unwrap();
        let key = crate::message::types::app_message_key("personal", "plug", now, "am1");
        a.put(&key, &app_message_json("am1", "plug", now)).unwrap();

        let w = MessageWindow { max_per_conv: 100, max_age_ms: i64::MAX, msg_sync_after: None };
        let recs = collect_message_window(&a, &w).unwrap();
        assert_eq!(recs.len(), 1, "msg:app 应随窗口采集");
        assert_eq!(recs[0].key, key);

        for r in &recs {
            apply_message_record(&mut b, &r.key, &r.value.to_string()).unwrap();
        }
        let landed: crate::message::AppMessageRecord =
            serde_json::from_str(&b.get(&key).unwrap().unwrap()).unwrap();
        assert_eq!(landed.id, "am1");
        assert_eq!(landed.plugin_id, "plug");
        // 无 byid 索引（本地应用消息路径同样不建）
        let idx = crate::message::types::message_id_index_key("personal", "app:plug", "am1");
        assert!(b.get(&idx).unwrap().is_none());
    }

    /// msg:item 与 msg:app 混合窗口：同 conv 名前缀互不串扰。
    #[test]
    fn collect_message_window_covers_item_and_app() {
        let mut s = MemoryStorage::new();
        let now = crate::p2p::node::system_now_ms();
        s.put("msg:conv:personal:c1", "{}").unwrap();
        s.put("msg:conv:personal:app:plug", "{}").unwrap();
        let ik = crate::message::types::message_key("personal", "c1", now, "i1");
        s.put(&ik, &message_json("i1", now, false)).unwrap();
        let ak = crate::message::types::app_message_key("personal", "plug", now, "a1");
        s.put(&ak, &app_message_json("a1", "plug", now)).unwrap();

        let w = MessageWindow { max_per_conv: 100, max_age_ms: i64::MAX, msg_sync_after: None };
        let recs = collect_message_window(&s, &w).unwrap();
        let keys: Vec<&str> = recs.iter().map(|r| r.key.as_str()).collect();
        assert!(keys.contains(&ik.as_str()), "缺 msg:item 记录");
        assert!(keys.contains(&ak.as_str()), "缺 msg:app 记录");
    }

    /// 对端 hello 声明极端 maxAgeMs（负值 / i64::MIN）：钳制不 panic。
    #[test]
    fn message_window_extreme_max_age_clamped() {
        for raw in [-1i64, i64::MIN, 0, i64::MAX] {
            let body = json!({"msgWindow": {"maxAgeMs": raw, "maxPerConv": 10}});
            let w = MessageWindow::from_hello(&body);
            assert!(w.max_age_ms >= 0, "maxAgeMs={raw} 应钳到非负");
            // 采集路径（含 cutoff 减法）不得溢出 panic
            let mut s = MemoryStorage::new();
            s.put("msg:conv:personal:c1", "{}").unwrap();
            let now = crate::p2p::node::system_now_ms();
            let key = crate::message::types::message_key("personal", "c1", now, "m1");
            s.put(&key, &message_json("m1", now, false)).unwrap();
            let _ = collect_message_window(&s, &w).unwrap();
        }
        let body = json!({"msgWindow": {"maxAgeMs": i64::MIN}});
        assert_eq!(MessageWindow::from_hello(&body).max_age_ms, 0);
    }

    /// 损坏的本地记录（pmeta 完好、本体非 JSON）：跳过不推——不得以
    /// `null` 冒充空值覆盖对端好数据。
    #[test]
    fn collect_incremental_skips_corrupted_value() {
        let mut s = MemoryStorage::new();
        let good = format!("{FRIEND_PREFIX}good");
        let bad = format!("{FRIEND_PREFIX}bad");
        put_personal(&mut s, NODE_A, &good, r#""ok""#, 1000).unwrap();
        put_personal(&mut s, NODE_A, &bad, r#""will-corrupt""#, 1000).unwrap();
        // 直接写坏本体（pmeta 仍完好）
        s.put(&bad, "not-json{{{").unwrap();
        let inc = collect_incremental(&s, category_friend(), &VersionVector::new(), None, 0).unwrap();
        assert_eq!(inc.len(), 1, "损坏记录应跳过");
        assert_eq!(inc[0].key, good);
    }

    /// 墓碑推送：删除的记录以 `{key, value: null, meta(tombstone=true), dseq}`
    /// 进入增量——由删除日志 ACK 游标驱动（不再看 knownVv：折叠 vv 丢失
    /// key 维度，"knownVv 覆盖该 nodeId"推不出"对端知道这条 key 被删"）。
    #[test]
    fn collect_incremental_includes_tombstones() {
        let mut s = MemoryStorage::new();
        let live = format!("{FRIEND_PREFIX}live");
        let dead = format!("{FRIEND_PREFIX}dead");
        put_personal(&mut s, NODE_A, &live, r#""1""#, 1000).unwrap();
        put_personal(&mut s, NODE_A, &dead, r#""2""#, 1000).unwrap();
        delete_personal(&mut s, NODE_A, &dead, 2000).unwrap();

        let inc = collect_incremental(&s, category_friend(), &VersionVector::new(), None, 0).unwrap();
        assert_eq!(inc.len(), 2);
        let tomb = inc.iter().find(|r| r.key == dead).expect("墓碑应在增量中");
        assert_eq!(tomb.value, Value::Null);
        assert_eq!(tomb.meta.tombstone, Some(true));
        assert_eq!(tomb.dseq, Some(1), "墓碑携带删除日志序号");
        let live_rec = inc.iter().find(|r| r.key == live).unwrap();
        assert_eq!(live_rec.value, json!("1"));

        // knownVv 已覆盖该 nodeId 全部写入（A:2）但 dlogAck=0：活记录被 vv
        // 跳过，墓碑仍推（折叠 vv 不能证明对端知道这条删除——正是原 bug 场景）
        let known: VersionVector = [(NODE_A.to_string(), 2)].into_iter().collect();
        let inc2 = collect_incremental(&s, category_friend(), &known, None, 0).unwrap();
        assert_eq!(inc2.len(), 1, "knownVv 覆盖不等于知道删除，墓碑必须照推");
        assert_eq!(inc2[0].key, dead);

        // 对端回执 dlogAck=1（已收该日志条目）→ 不再重推
        let inc3 = collect_incremental(&s, category_friend(), &known, None, 1).unwrap();
        assert!(inc3.is_empty(), "已确认墓碑不重推");
    }

    /// 端到端墓碑传播：A 删除 → hello/need/data 交换（删除日志 + ACK 回执）
    /// → B 的记录被删 + 墓碑 pmeta 落地；双方回执齐全后收敛无增量。
    #[test]
    fn tombstone_delete_propagates_end_to_end() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        let key = format!("{FRIEND_PREFIX}doomed");
        put_personal(&mut a, NODE_A, &key, r#""v1""#, 1000).unwrap();
        let mut a_seen = 0u64; // A 已收 B 日志的最大 dseq
        let mut b_seen = 0u64; // B 已收 A 日志的最大 dseq

        // 第一轮：A → B，B 获得记录
        let mut hello_a = build_hello(&a, 2_592_000_000, 500, "eager", None, None).unwrap();
        hello_a["dlogAck"] = json!(a_seen);
        for data in exchange(&a, &mut b, &hello_a, None, &mut b_seen, &mut a_seen) {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ =
                    apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        assert!(b.get(&key).unwrap().is_some(), "B 应先获得记录");

        // A 删除记录（写墓碑 + 删除日志 seq=1）
        delete_personal(&mut a, NODE_A, &key, 2000).unwrap();

        // 第二轮：B 折叠 vv 落后 → need → A 按日志（ack=0）推墓碑 → B 删
        // 本体 + 落墓碑 pmeta（B 同时补登接力日志）
        let mut hello_a2 = build_hello(&a, 2_592_000_000, 500, "eager", None, None).unwrap();
        hello_a2["dlogAck"] = json!(a_seen);
        let responses = exchange(&a, &mut b, &hello_a2, None, &mut b_seen, &mut a_seen);
        assert!(!responses.is_empty(), "B 落后应触发 need→data");
        for data in responses {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ =
                    apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        assert!(b.get(&key).unwrap().is_none(), "B 的记录应被墓碑删除");
        let pmeta = get_personal_meta(&b, &key).unwrap().unwrap();
        assert!(is_tombstone(&pmeta), "B 应持久化墓碑 pmeta");
        assert_eq!(pmeta.vv.get(NODE_A), Some(&2));
        assert_eq!(b_seen, 1, "B 已收讫 A 的删除日志 seq=1");

        // 第三轮：折叠 vv 已 Equal，但 B 的接力日志条目未获 A 回执——Equal
        // 分支按 ACK 游标补推（A 应用为幂等 no-op），A 收讫 a_seen=1；B 对
        // A 无 need（diff Equal），responses 为空
        let mut hello_a3 = build_hello(&a, 2_592_000_000, 500, "eager", None, None).unwrap();
        hello_a3["dlogAck"] = json!(a_seen);
        assert!(
            exchange(&a, &mut b, &hello_a3, None, &mut b_seen, &mut a_seen).is_empty(),
            "Equal 无 need"
        );
        assert_eq!(a_seen, 1, "A 收讫 B 的接力日志条目");

        // 第四轮：双方回执齐全（hello 各携带对端 seen）→ 不再互推墓碑，收敛
        let mut hello_b = build_hello(&b, 2_592_000_000, 500, "eager", None, None).unwrap();
        hello_b["dlogAck"] = json!(b_seen);
        assert!(
            exchange(&b, &mut a, &hello_b, None, &mut a_seen, &mut b_seen).is_empty(),
            "墓碑收敛后无增量"
        );
        let mut hello_a4 = build_hello(&a, 2_592_000_000, 500, "eager", None, None).unwrap();
        hello_a4["dlogAck"] = json!(a_seen);
        assert!(
            exchange(&a, &mut b, &hello_a4, None, &mut b_seen, &mut a_seen).is_empty(),
            "墓碑收敛后无增量"
        );
    }

    #[test]
    fn two_device_message_window_converges() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        let now = crate::p2p::node::system_now_ms();
        // 采集按会话枚举：双方都有 c1 的 conv 记录
        a.put("msg:conv:personal:c1", "{}").unwrap();
        b.put("msg:conv:personal:c1", "{}").unwrap();
        // A 有 2 条消息
        for i in 0..2 {
            let key = crate::message::types::message_key(
                "personal",
                "c1",
                now - 1000,
                &format!("a{i}"),
            );
            let val = message_json(&format!("a{i}"), now - 1000, false);
            a.put(&key, &val).unwrap();
        }
        // B 有 1 条不同消息
        let bk = crate::message::types::message_key("personal", "c1", now, "b0");
        b.put(&bk, &message_json("b0", now, false)).unwrap();

        // A → B：按 B 的窗口采集 A 的消息，apply 到 B
        let window = MessageWindow { max_per_conv: 100, max_age_ms: i64::MAX, msg_sync_after: None };
        let a_recs = collect_message_window(&a, &window).unwrap();
        for r in &a_recs {
            apply_message_record(&mut b, &r.key, &r.value.to_string()).unwrap();
        }
        // B → A：反向
        let b_recs = collect_message_window(&b, &window).unwrap();
        for r in &b_recs {
            apply_message_record(&mut a, &r.key, &r.value.to_string()).unwrap();
        }
        // 双方都有全部 3 条
        for (s, name) in [(&a, "a"), (&b, "b")] {
            for id in ["a0", "a1", "b0"] {
                let key = crate::message::types::message_key("personal", "c1", {
                    if id == "b0" { now } else { now - 1000 }
                }, id);
                assert!(
                    s.get(&key).unwrap().is_some(),
                    "{name} 缺少消息 {id}"
                );
            }
        }
    }

    // ── P5 组织数据 ─────────────────────────────────────────────────

    /// P5：组织记录型数据（`org:meta` / `ct:org` 成员 extra / `org:inv`）作为
    /// 独立 category 折叠，且双设备双向交换收敛。
    #[test]
    fn two_device_org_data_exchange_converges() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();

        let org_meta_key = "org:meta:org1";
        let member_key = "ct:org:org1:extra:peer1";
        let invite_key = "org:inv:out:org1:peer1";

        // A：组织记录 + 成员资料 + 邀请
        put_personal(
            &mut a,
            NODE_A,
            org_meta_key,
            r#"{"orgId":"org1","name":"组织甲"}"#,
            1000,
        )
        .unwrap();
        put_personal(
            &mut a,
            NODE_A,
            member_key,
            r#"{"rootId":"peer1","remark":"A的备注"}"#,
            1000,
        )
        .unwrap();
        put_personal(
            &mut a,
            NODE_A,
            invite_key,
            r#"{"id":"inv1","status":"pending"}"#,
            1000,
        )
        .unwrap();
        // B：组织记录（同名，不同字段）
        put_personal(
            &mut b,
            NODE_B,
            org_meta_key,
            r#"{"orgId":"org1","name":"组织乙"}"#,
            2000,
        )
        .unwrap();

        // A → B 交换
        let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", None, None).unwrap();
        let responses = exchange_simple(&a, &mut b, &hello_a, None);
        for data in responses {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        // B → A 反向
        let hello_b = build_hello(&b, 2_592_000_000, 500, "eager", None, None).unwrap();
        let responses_b = exchange_simple(&b, &mut a, &hello_b, None);
        for data in responses_b {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut a, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }

        // B 端应获得 A 的成员资料与邀请（org:meta 单记录并发冲突 → LWW B 胜）
        assert!(b.get(member_key).unwrap().is_some(), "B 缺成员资料");
        assert!(b.get(invite_key).unwrap().is_some(), "B 缺邀请记录");
        // A 端获得 B 的组织记录（org:meta 单记录并发，B ts 大 → B 胜）
        assert!(a.get(org_meta_key).unwrap().is_some());

        // org:meta / ct:org / org:inv 三 category 折叠收敛。
        // A 侧三次受管写依次为 org:meta(A:1) / ct:org 成员(A:2) / org:inv 邀请(A:3)，
        // per-node 序号折叠后 ct:org=A:2、org:inv=A:3。
        for s in [&a, &b] {
            let org_meta_vv = collect_category_vv(s, category_named("org:meta"), None).unwrap();
            assert_eq!(org_meta_vv.get(NODE_B), Some(&1), "org:meta LWW B 胜");
            let ct_org_vv = collect_category_vv(s, category_named("ct:org"), None).unwrap();
            assert_eq!(ct_org_vv.get(NODE_A), Some(&2), "ct:org 含 A 分量");
            let org_inv_vv = collect_category_vv(s, category_named("org:inv"), None).unwrap();
            assert_eq!(org_inv_vv.get(NODE_A), Some(&3), "org:inv 含 A 分量");
        }
    }

    /// P5：组织标签/分组树集合型数据作为单记录（整域）经 pdsync 同步。
    #[test]
    fn org_tags_tree_sync_as_single_record() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();

        let tags_key = "ct:org:org1:tags";
        let tree_key = "ct:org:org1:tree";
        // A 写组织标签数组 + 分组树
        put_personal(&mut a, NODE_A, tags_key, r#"[{"id":"t1","name":"核心"}]"#, 1000).unwrap();
        put_personal(
            &mut a,
            NODE_A,
            tree_key,
            r#"[{"id":"g1","name":"研发","children":[]}]"#,
            1000,
        )
        .unwrap();

        // A → B
        let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", None, None).unwrap();
        let responses = exchange_simple(&a, &mut b, &hello_a, None);
        for data in responses {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        // B 获得整域 tags/tree 记录
        assert!(b.get(tags_key).unwrap().is_some(), "B 缺组织标签");
        assert!(b.get(tree_key).unwrap().is_some(), "B 缺组织分组树");
        // 收敛后无增量
        let hello_a2 = build_hello(&a, 2_592_000_000, 500, "eager", None, None).unwrap();
        assert!(exchange_simple(&a, &mut b, &hello_a2, None).is_empty(), "ct:org 收敛后无增量");
    }

    // ── 自 FriendRecord 排除（`ct:friend:{rootId}`，设备相对 peer 不可互灌）──

    /// 双设备同账号各自持有自记录（同键、peer 各指向对方设备）：带排除键的
    /// hello→need→data 交换后，两端自记录保持各自原值（不被互灌/LWW 收敛），
    /// 普通朋友记录照常同步，收敛后折叠 vv 无伪 diff。
    ///
    /// 回归：无排除时两设备自记录并发互推，LWW 收敛成一份 → 一端 peer
    /// 指向自己（自聊投递拨本机失败）。
    #[test]
    fn self_friend_record_excluded_from_sync() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        let self_key = self_friend_key("root-self");
        let friend_key = format!("{FRIEND_PREFIX}other");
        // A：先写普通朋友（A:1）再写自记录（A:2）——排除后折叠应为 {A:1}，
        // 与不排除的 {A:2} 可区分
        put_personal(&mut a, NODE_A, &friend_key, r#""friend""#, 1000).unwrap();
        put_personal(
            &mut a,
            NODE_A,
            &self_key,
            r#"{"rootId":"root-self","peer":{"peerId":"peer-a","addresses":[]}}"#,
            1000,
        )
        .unwrap();
        // B：只有自记录（B:1，ts 更大——无排除时 LWW 会毒化 A）
        put_personal(
            &mut b,
            NODE_B,
            &self_key,
            r#"{"rootId":"root-self","peer":{"peerId":"peer-b","addresses":[]}}"#,
            2000,
        )
        .unwrap();

        // 折叠：排除键使自记录分量不进摘要
        let folded_a = collect_category_vv(&a, category_friend(), Some(&self_key)).unwrap();
        assert_eq!(folded_a.get(NODE_A), Some(&1), "排除后只折普通朋友记录");
        assert_eq!(folded_a.get(NODE_B), None);
        let folded_b = collect_category_vv(&b, category_friend(), Some(&self_key)).unwrap();
        assert!(folded_b.get(NODE_B).is_none(), "B 排除自记录后 ct:friend 为空");
        // 对照：不排除时自记录进折叠（B 侧 {B:1}）→ 与 A 并发互推
        let folded_b_raw = collect_category_vv(&b, category_friend(), None).unwrap();
        assert_eq!(folded_b_raw.get(NODE_B), Some(&1));

        // A → B、B → A 各一轮 hello→need→data（两侧同一排除键）
        let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", Some(&self_key), None).unwrap();
        for data in exchange_simple(&a, &mut b, &hello_a, Some(&self_key)) {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        let hello_b = build_hello(&b, 2_592_000_000, 500, "eager", Some(&self_key), None).unwrap();
        for data in exchange_simple(&b, &mut a, &hello_b, Some(&self_key)) {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut a, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }

        // 两端自记录保持各自原值（peer 各指向对方设备，未被互灌）
        let a_self = a.get(&self_key).unwrap().expect("A 自记录仍在");
        assert!(a_self.contains("peer-a"), "A 自记录 peer 保持指向 A 设备: {a_self}");
        let b_self = b.get(&self_key).unwrap().expect("B 自记录仍在");
        assert!(b_self.contains("peer-b"), "B 自记录 peer 保持指向 B 设备: {b_self}");
        // 普通朋友记录照常同步到 B
        assert!(b.get(&friend_key).unwrap().is_some(), "普通朋友记录应同步");

        // 收敛：折叠 vv 一致，再交换无 need/data（无伪 diff）
        let hello_a2 = build_hello(&a, 2_592_000_000, 500, "eager", Some(&self_key), None).unwrap();
        assert!(exchange_simple(&a, &mut b, &hello_a2, Some(&self_key)).is_empty(), "A→B 收敛无增量");
        let hello_b2 = build_hello(&b, 2_592_000_000, 500, "eager", Some(&self_key), None).unwrap();
        assert!(exchange_simple(&b, &mut a, &hello_b2, Some(&self_key)).is_empty(), "B→A 收敛无增量");
    }

    /// 自记录墓碑排除：本机删自记录产生的墓碑不进增量、不进折叠——
    /// 不得把对端的自记录（它的设备相对数据）删掉。
    #[test]
    fn self_friend_tombstone_excluded_from_incremental() {
        let mut a = MemoryStorage::new();
        let self_key = self_friend_key("root-self");
        put_personal(&mut a, NODE_A, &self_key, r#""v""#, 1000).unwrap();
        delete_personal(&mut a, NODE_A, &self_key, 2000).unwrap();

        let inc = collect_incremental(
            &a,
            category_friend(),
            &VersionVector::new(),
            Some(&self_key),
            0,
        )
        .unwrap();
        assert!(inc.is_empty(), "自记录墓碑不参与增量推送");
        let folded = collect_category_vv(&a, category_friend(), Some(&self_key)).unwrap();
        assert!(folded.get(NODE_A).is_none(), "自记录墓碑不进折叠");
        // 对照：不排除时墓碑确实会在增量里（防测试本身失效）
        let inc_raw =
            collect_incremental(&a, category_friend(), &VersionVector::new(), None, 0).unwrap();
        assert_eq!(inc_raw.len(), 1);
        assert_eq!(inc_raw[0].meta.tombstone, Some(true));
    }

    // ── P6 插件声明式数据（pdecl/pdoc category）─────────────────────────

    /// P6 写库即同步：插件经声明式 API 写入（版本化句柄自动记账）→
    /// 声明记录（pdecl）与数据（pdoc）随同一套 hello/need/data 收敛到对端，
    /// 插件侧零同步代码。
    #[test]
    fn p6_declared_collection_syncs_end_to_end() {
        use crate::plugindata::{DeclareInput, declare, save};
        use crate::sync::versioned::{VersionedStorage, shared_node_id};

        let mut a = VersionedStorage::new(MemoryStorage::new(), shared_node_id(NODE_A));
        let mut b = MemoryStorage::new();

        // 插件声明 + 写两条数据（声明先行：pdecl 与 pdoc 都进折叠）
        let decl = declare(
            &mut a,
            "ai-chat",
            DeclareInput {
                name: "ai-chat:conversations".to_string(),
                ..Default::default()
            },
            1000,
            None,
        )
        .unwrap();
        save(&mut a, &decl, "c1", r#"{"title":"一"}"#).unwrap();
        save(&mut a, &decl, "c2", r#"{"title":"二"}"#).unwrap();
        let data_key = decl.data_key("c1");

        // hello 折叠摘要包含 pdecl/pdoc 两 category
        let hello_a = build_hello(a.raw(), 2_592_000_000, 500, "eager", None, None).unwrap();
        let cats = parse_hello_categories(&hello_a);
        assert!(cats.iter().any(|(name, _)| name == "pdecl"), "hello 含 pdecl");
        assert!(cats.iter().any(|(name, _)| name == "pdoc"), "hello 含 pdoc");
        // deviceClass 随 hello 声明（驻留裁剪依据）
        assert!(hello_a["deviceClass"].is_string());

        // A → B 交换
        for data in exchange_simple(a.raw(), &mut b, &hello_a, None) {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }

        // B 获得声明与数据
        let decl_b = crate::plugindata::resolve(&b, "ai-chat:conversations", None)
            .expect("B 端声明已同步");
        assert_eq!(decl_b.version, "1");
        assert!(b.get(&data_key).unwrap().is_some(), "B 缺插件数据");
        // local scope 集合不离开 A（ldoc 不在任何 category）
        let local_decl = declare(
            &mut a,
            "ai-chat",
            DeclareInput {
                name: "ai-chat:drafts".to_string(),
                scope: Some(crate::plugindata::Scope::Local),
                ..Default::default()
            },
            2000,
            None,
        )
        .unwrap();
        save(&mut a, &local_decl, "d1", "\"secret\"").unwrap();
        let hello_a2 = build_hello(a.raw(), 2_592_000_000, 500, "eager", None, None).unwrap();
        for data in exchange_simple(a.raw(), &mut b, &hello_a2, None) {
            let (_, records) = parse_data(&data).unwrap();
            for r in &records {
                assert!(!r.key.starts_with("ldoc:"), "local 集合数据不得进入同步流量");
            }
            // 与首轮同口径合入：local 集合的**声明记录**（pdecl:）仍在 pdecl
            // category 内——per-node 序号下它消耗序号、使 A 的 pdecl 折叠领先
            // 对端（旧 per-key 语义下被同值 vv 遮蔽），不合入则收敛断言永不成立。
            for r in records {
                let _ = apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        assert!(b.get(&local_decl.data_key("d1")).unwrap().is_none());

        // 收敛无增量
        let hello_a3 = build_hello(a.raw(), 2_592_000_000, 500, "eager", None, None).unwrap();
        assert!(
            exchange_simple(a.raw(), &mut b, &hello_a3, None).is_empty(),
            "收敛后无增量"
        );
    }

    /// P6 驻留裁剪：pc-only 集合的记录不推给 mobile 设备类对端；
    /// all 集合照常；未知设备类（旧对端）不过滤。
    #[test]
    fn p6_residency_trim_by_remote_device_class() {
        use crate::plugindata::{DeclareInput, Devices, declare, save};
        use crate::sync::versioned::{VersionedStorage, shared_node_id};

        let mut a = VersionedStorage::new(MemoryStorage::new(), shared_node_id(NODE_A));
        let all_decl = declare(
            &mut a,
            "ai-chat",
            DeclareInput {
                name: "ai-chat:conversations".to_string(),
                ..Default::default()
            },
            1000,
            None,
        )
        .unwrap();
        let pc_decl = declare(
            &mut a,
            "ai-chat",
            DeclareInput {
                name: "ai-chat:secrets".to_string(),
                devices: Some(Devices::PcOnly),
                ..Default::default()
            },
            1000,
            None,
        )
        .unwrap();
        save(&mut a, &all_decl, "c1", "\"all\"").unwrap();
        save(&mut a, &pc_decl, "s1", "\"pc-only\"").unwrap();

        let category = category_named("pdoc");
        let records = collect_incremental(a.raw(), category, &VersionVector::new(), None, 0).unwrap();
        assert_eq!(records.len(), 2);

        // 对端是手机：pc-only 记录被裁剪
        let to_mobile = trim_records_by_residency(a.raw(), records.clone(), Some("mobile"));
        assert_eq!(to_mobile.len(), 1);
        assert!(to_mobile[0].key.contains("conversations"));
        // 对端是 PC：全量
        let to_pc = trim_records_by_residency(a.raw(), records.clone(), Some("pc"));
        assert_eq!(to_pc.len(), 2);
        // 未知设备类（旧对端）：不过滤
        let to_legacy = trim_records_by_residency(a.raw(), records, None);
        assert_eq!(to_legacy.len(), 2);
    }

    fn category_named(name: &str) -> &'static Category {
        CATEGORIES.iter().find(|c| c.name == name).unwrap()
    }
