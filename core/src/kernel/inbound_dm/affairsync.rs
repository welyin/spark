//! dm 入站编排（affairsync 系）：affairsync-hello / affairsync-need /
//! affairsync-data 三信封事务复制面反熵同步（affair-sync §2/§7）。
//!
//! 从 `inbound_dm` 拆出的子模块（对齐 orgsync 先例），共享父模块的
//! [`InboundContext`]/应答助手/[`done`] 等；业务逻辑全部在
//! `crate::sync::affairsync`（纯逻辑层），本模块只做 dm 语义包装：
//! 应答帧 + 出向指令（[`AffairsyncDmOut`]）。
//!
//! 验签规则（与 orgsync「from ∈ 成员表 + 复制组」对照，affair-sync §1）：
//! affair 域**无名册**——hello/need 只对本机已关注（`affair:follow:`）的事务
//! 应答（未关注静默丢弃，应答 `ignored`）；data 的安全边界是 apply 层逐条
//! `verify_op`/`verify_genesis` 签名链 + key 白名单（任一越白名单整批拒收）。
//! 这些门槛都在纯逻辑层 handler/apply 内执行，本模块不重复判定。

use serde_json::Value;

use super::{AffairsyncDmOut, InboundContext, InboundDmResult, Result, done, fail_response};
use crate::storage::StorageBackend;

/// affairsync-hello：收到关注者的摘要。委托纯逻辑层 diff 裁决：
/// - 本机落后/并发 → 回 `affairsync-need`；
/// - 本机领先/并发 → 推 `affairsync-data`（分批）；
/// - 相等 → 不动。
///
/// 未关注该事务或 body 畸形 → `ignored` 静默丢弃（affair-sync §7）。
pub(super) fn handle_affairsync_hello<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let Some(out) = crate::sync::affairsync::handle_affairsync_hello(
        storage,
        from,
        Some(ctx.remote_peer_id),
        body,
        ctx.now_ms,
    )?
    else {
        return done(fail_response("ignored"), Vec::new());
    };
    let mut outs = Vec::new();
    if let Some(need_body) = out.need {
        outs.push(AffairsyncDmOut::Need {
            to_root_id: from.to_string(),
            body: need_body,
        });
    }
    for data_body in out.data {
        outs.push(AffairsyncDmOut::Data {
            to_root_id: from.to_string(),
            body: data_body,
        });
    }
    let mut result = done(super::ok_response(), Vec::new())?;
    result.affairsync_out = outs;
    Ok(result)
}

/// affairsync-need：收到关注者的 diff 请求。采集该事务增量，分批发回
/// `affairsync-data`。未关注或 body 畸形 → `ignored`（affair-sync §7）。
pub(super) fn handle_affairsync_need<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let Some(batches) = crate::sync::affairsync::handle_affairsync_need(
        storage,
        from,
        Some(ctx.remote_peer_id),
        body,
        ctx.now_ms,
    )?
    else {
        return done(fail_response("ignored"), Vec::new());
    };
    let outs = batches
        .into_iter()
        .map(|body| AffairsyncDmOut::Data {
            to_root_id: from.to_string(),
            body,
        })
        .collect();
    let mut result = done(super::ok_response(), Vec::new())?;
    result.affairsync_out = outs;
    Ok(result)
}

/// affairsync-data：收到关注者的增量数据，逐条过校验链合入（幂等；乱序
/// 因果见证未知持久暂存待补，drain 补齐）。
///
/// 门槛（纯逻辑层 apply 内执行，affair-sync §4）：本机未关注该事务 → 整批
/// 拒收；任一记录 key 越白名单（`affair:rec:{affairId}` /
/// `affair:op:{affairId}:` 前缀）→ 整批拒收。应答携带 applied 摘要
/// （accepted/duplicates/pending/rejected/drained）供对端观测。
pub(super) fn handle_affairsync_data<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let _ = from; // 数据面无成员语义：安全边界在逐条验签链（affair-sync §1/§4）
    let Some((affair_id, records)) = crate::sync::affairsync::parse_affairsync_data(body) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let summary = crate::sync::affairsync::apply_affairsync_records(
        storage, &affair_id, &records, ctx.now_ms,
    )?;
    // sdk.affairs.onChange 事件源：复制面入站有新条目接受/暂存补齐时通知
    // （变更通知非可靠队列，插件收到后重读 readLog 收敛）
    let events = if summary.accepted > 0 || summary.drained > 0 {
        vec![crate::p2p::P2pEvent::AffairChanged(serde_json::json!({
            "affairId": affair_id,
            "change": "replicated",
            "accepted": summary.accepted,
            "drained": summary.drained,
        }))]
    } else {
        Vec::new()
    };
    done(
        serde_json::json!({ "ok": true, "applied": serde_json::to_value(summary)? }),
        events,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use crate::sync::meta::{DocMeta, VersionVector};

    fn id(byte: &str) -> String {
        byte.repeat(32)
    }

    fn test_ctx<'a>(
        my_root_id: &'a str,
        remote_peer_id: &'a str,
        online: &'a std::collections::HashSet<String>,
    ) -> InboundContext<'a> {
        InboundContext {
            my_root_id,
            my_nickname: "tester",
            remote_peer_id,
            online_peers: online,
            node_id: "node-t",
            now_ms: 1_720_000_000_000,
            kverify: None,
        }
    }

    /// 给某事务写入一条带 pmeta 的记录（复刻 collect 层数据布局）。
    fn put_op_record(
        storage: &mut MemoryStorage,
        affair_id: &str,
        op_hash: &str,
        node: &str,
        seq: i64,
    ) {
        let key = crate::affair::affair_op_key(affair_id, op_hash);
        let meta = DocMeta {
            vv: VersionVector::from([(node.to_string(), seq)]),
            ts: 1000,
            ..DocMeta::default()
        };
        storage.put(&key, "{\"opV\":1}").unwrap();
        storage
            .put(
                &crate::sync::personal_meta_key(&key),
                &serde_json::to_string(&meta).unwrap(),
            )
            .unwrap();
    }

    /// hello：未关注该事务 → `ignored` 静默丢弃，零出向。
    #[test]
    fn hello_unfollowed_is_ignored() {
        let mut storage = MemoryStorage::default();
        let online = std::collections::HashSet::new();
        let my_root = id("aa");
        let affair_id = id("ab");
        let body = crate::sync::affairsync::build_affairsync_hello(
            &affair_id,
            &VersionVector::new(),
            &[],
            "pc",
        );
        let ctx = test_ctx(&my_root, "peer-x", &online);
        let result = handle_affairsync_hello(&mut storage, &ctx, &id("bb"), &body).unwrap();
        assert_eq!(
            result.response,
            serde_json::json!({"ok": false, "reason": "ignored"})
        );
        assert!(result.affairsync_out.is_empty());
    }

    /// hello：已关注且本机领先 → 回推 data（出向指令含全量记录批）；
    /// 本机落后 → 回 need。
    #[test]
    fn hello_diff_drives_need_or_data_out() {
        let mut storage = MemoryStorage::default();
        let online = std::collections::HashSet::new();
        let my_root = id("aa");
        let affair_id = id("ab");
        let from = id("bb");
        crate::sync::affairsync::follow_affair(&mut storage, &affair_id, 1).unwrap();

        // 本机有一条 op（node-t 分量 1）：对端空 vv → 本机领先 → 推 data
        put_op_record(&mut storage, &affair_id, &id("c1"), "node-t", 1);
        let body = crate::sync::affairsync::build_affairsync_hello(
            &affair_id,
            &VersionVector::new(),
            &[],
            "pc",
        );
        let ctx = test_ctx(&my_root, "peer-x", &online);
        let result = handle_affairsync_hello(&mut storage, &ctx, &from, &body).unwrap();
        assert_eq!(result.response, serde_json::json!({"ok": true}));
        assert_eq!(result.affairsync_out.len(), 1);
        assert!(
            matches!(result.affairsync_out[0], AffairsyncDmOut::Data { .. }),
            "本机领先应推 data"
        );
        assert_eq!(result.affairsync_out[0].to_root_id(), from);

        // 对端 vv 领先（含本机未见分量）→ 本机落后 → 回 need
        let remote_vv = VersionVector::from([("node-r".to_string(), 5)]);
        let body =
            crate::sync::affairsync::build_affairsync_hello(&affair_id, &remote_vv, &[], "pc");
        let result = handle_affairsync_hello(&mut storage, &ctx, &from, &body).unwrap();
        assert!(
            result
                .affairsync_out
                .iter()
                .any(|o| matches!(o, AffairsyncDmOut::Need { .. })),
            "本机落后应回 need"
        );
    }

    /// data：body 畸形 → `invalid-body`；key 越白名单 → applied 摘要整批
    /// rejected（红线在纯逻辑层 apply 执行，本测试锚定 dm 编排透传）。
    #[test]
    fn data_invalid_body_and_whitelist_rejection() {
        let mut storage = MemoryStorage::default();
        let online = std::collections::HashSet::new();
        let my_root = id("aa");
        let affair_id = id("ab");
        let from = id("bb");
        crate::sync::affairsync::follow_affair(&mut storage, &affair_id, 1).unwrap();
        let ctx = test_ctx(&my_root, "peer-x", &online);

        let result =
            handle_affairsync_data(&mut storage, &ctx, &from, &serde_json::json!({})).unwrap();
        assert_eq!(
            result.response,
            serde_json::json!({"ok": false, "reason": "invalid-body"})
        );

        // 白名单越界（org: 键恒拒收，affair-sync §3.3 红线）
        let body = serde_json::json!({
            "affairId": affair_id,
            "records": [{
                "key": "org:meta:xxx",
                "value": {},
                "meta": serde_json::to_value(DocMeta::default()).unwrap(),
            }],
            "batchSeq": 0,
            "batchTotal": 1,
        });
        let result = handle_affairsync_data(&mut storage, &ctx, &from, &body).unwrap();
        assert_eq!(result.response["ok"], serde_json::json!(true));
        assert_eq!(result.response["applied"]["rejected"], serde_json::json!(1));
        assert_eq!(result.response["applied"]["accepted"], serde_json::json!(0));
    }
}
