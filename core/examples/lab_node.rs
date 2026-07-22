//! lab_node：p2p 互通实验例程（阶段②收官实验：TS↔Rust 真实互通）。
//!
//! 进程模型 = stdio JSON 行协议（每行一个 JSON 对象）：
//! - stdout：`{"type":"ready",...}` 启动完成 / `{"type":"event",...}` 节点事件 /
//!   `{"type":"applied",...}` 宿主落库回调 / `{"type":"result",...}` 命令响应；
//! - stdin 命令（带 `"id"` 原样回显）：`info` / `broadcast-update` / `broadcast` /
//!   `announce` / `exchange` / `connect` / `overlay-pool` / `seed-overlay` / `shutdown`。
//!
//! 用法：`cargo run --example lab_node -- --port 16200`（端口 0 = OS 分配，见 ready 行）。
//! 搭建方式参考 tests/p2p_integration.rs（SharedStorage + 最小内存宿主）。

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use spark_core::p2p::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use spark_core::p2p::peer_targets::PeerNodeInfo;
use spark_core::p2p::{P2pConfig, P2pEvent, P2pHost, P2pNode, build_update_body};
use spark_core::storage::{BatchOperation, MemoryStorage, ScanOptions, StorageBackend};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// 共享存储（主循环经它查询/预置邻居池，事件循环持有另一份克隆）
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct SharedStorage(Arc<Mutex<MemoryStorage>>);

impl StorageBackend for SharedStorage {
    fn get(&self, key: &str) -> spark_core::storage::Result<Option<String>> {
        self.0.lock().unwrap().get(key)
    }
    fn put(&mut self, key: &str, value: &str) -> spark_core::storage::Result<()> {
        self.0.lock().unwrap().put(key, value)
    }
    fn delete(&mut self, key: &str) -> spark_core::storage::Result<()> {
        self.0.lock().unwrap().delete(key)
    }
    fn batch(&mut self, operations: Vec<BatchOperation>) -> spark_core::storage::Result<()> {
        self.0.lock().unwrap().batch(operations)
    }
    fn scan(&self, options: &ScanOptions) -> spark_core::storage::Result<Vec<(String, String)>> {
        self.0.lock().unwrap().scan(options)
    }
}

// ---------------------------------------------------------------------------
// 最小内存宿主：update/delete/history-response 落库回调 → 通知主循环打印
// ---------------------------------------------------------------------------

struct LabHost {
    root_id: Option<String>,
    notify: mpsc::UnboundedSender<Value>,
}

impl P2pHost for LabHost {
    fn current_root_id(&mut self) -> Option<String> {
        self.root_id.clone()
    }

    fn apply_remote_update(
        &mut self,
        domain: &str,
        collection: &str,
        id: &str,
        payload: Value,
        _meta: Value,
        _schema: Option<Value>,
    ) -> Result<(), String> {
        let _ = self.notify.send(json!({
            "type": "applied",
            "domain": domain,
            "collection": collection,
            "id": id,
            "payload": payload,
        }));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 输出
// ---------------------------------------------------------------------------

fn print_line(value: &Value) {
    use std::io::Write;
    let text = serde_json::to_string(value).expect("lab output serializable");
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{text}");
    let _ = lock.flush();
}

fn event_json(event: &P2pEvent) -> Option<Value> {
    let map = match event {
        P2pEvent::ListenPortPersisted { port } => json!({"event": "listen-port", "port": port}),
        P2pEvent::PeerConnected { peer_id } => json!({"event": "peer-connected", "peerId": peer_id}),
        P2pEvent::PeerDisconnected { peer_id } => {
            json!({"event": "peer-disconnected", "peerId": peer_id})
        }
        P2pEvent::PeerVersion { peer_id, app_version } => {
            json!({"event": "peer-version", "peerId": peer_id, "appVersion": app_version})
        }
        P2pEvent::AnnouncePublished { addresses } => {
            json!({"event": "announce-published", "addresses": addresses})
        }
        P2pEvent::AnnounceAccepted { peer_id } => {
            json!({"event": "announce-accepted", "peerId": peer_id})
        }
        P2pEvent::PeerExchangeCompleted { responder, merged } => {
            json!({"event": "peer-exchange-completed", "responder": responder, "merged": merged})
        }
        P2pEvent::OrgShareAccepted { org_id, sync_id, source } => {
            json!({"event": "org-share-accepted", "orgId": org_id, "syncId": sync_id, "source": source})
        }
        P2pEvent::SyncMessageApplied { msg_type, domain } => {
            json!({"event": "sync-applied", "msgType": msg_type, "domain": domain})
        }
        P2pEvent::MessageDropped { reason } => json!({"event": "message-dropped", "reason": reason}),
        P2pEvent::Warning(msg) => json!({"event": "warning", "message": msg}),
        P2pEvent::Stopped => json!({"event": "stopped"}),
        // ready 行单独打印；keepalive tick 在本例程禁用
        P2pEvent::Started { .. } | P2pEvent::KeepaliveTick(_) => return None,
    };
    let mut obj = Map::new();
    obj.insert("type".to_string(), Value::String("event".to_string()));
    if let Value::Object(inner) = map {
        obj.extend(inner);
    }
    Some(Value::Object(obj))
}

// ---------------------------------------------------------------------------
// 命令处理
// ---------------------------------------------------------------------------

enum CmdOutcome {
    Continue,
    Shutdown,
}

fn cmd_result(id: &Value, ok: bool, data: Value) -> Value {
    if ok {
        json!({"type": "result", "id": id, "ok": true, "data": data})
    } else {
        json!({"type": "result", "id": id, "ok": false, "error": data})
    }
}

async fn handle_command(
    text: &str,
    node: &P2pNode,
    storage: &SharedStorage,
    self_peer_id: &str,
) -> CmdOutcome {
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            print_line(&cmd_result(&Value::Null, false, Value::String(format!("invalid json: {e}"))));
            return CmdOutcome::Continue;
        }
    };
    let id = parsed.get("id").cloned().unwrap_or(Value::Null);
    let cmd = parsed.get("cmd").and_then(Value::as_str).unwrap_or("");
    let now = spark_core::p2p::node::system_now_ms();

    let outcome: Result<Value, String> = match cmd {
        "info" => match node.local_node_info().await {
            Ok(info) => Ok(json!({
                "started": info.started,
                "peerId": info.peer_id,
                "addresses": info.addresses,
                "connectedPeers": info.connected_peers,
                "sparkSyncSubscribers": info.spark_sync_subscribers,
            })),
            Err(e) => Err(e.to_string()),
        },
        "broadcast-update" => {
            let domain = parsed.get("domain").and_then(Value::as_str).unwrap_or("notes");
            let collection = parsed.get("collection").and_then(Value::as_str).unwrap_or("items");
            let doc_id = parsed.get("docId").and_then(Value::as_str).unwrap_or("doc-rust-1");
            let payload = parsed.get("payload").cloned().unwrap_or_else(|| json!({"text": "hello from rust"}));
            let meta = json!({
                "vv": { self_peer_id: 1 },
                "ts": now,
                "nodeId": self_peer_id,
            });
            let body = build_update_body(domain, collection, doc_id, payload, meta, None);
            node.broadcast("spark-sync", body).await.map(|()| json!({"sent": true})).map_err(|e| e.to_string())
        }
        "broadcast" => {
            let topic = parsed.get("topic").and_then(Value::as_str).unwrap_or("spark-sync");
            let body = parsed.get("body").and_then(Value::as_object).cloned().unwrap_or_default();
            node.broadcast(topic, body).await.map(|()| json!({"sent": true})).map_err(|e| e.to_string())
        }
        "announce" => node.announce_now().await.map(|published| json!({"published": published})).map_err(|e| e.to_string()),
        "exchange" => {
            let peer = parsed.get("peerId").and_then(Value::as_str).unwrap_or("");
            node.exchange_with_peer(peer).await.map(|merged| json!({"merged": merged})).map_err(|e| e.to_string())
        }
        "connect" => {
            let info = PeerNodeInfo {
                peer_id: parsed.get("peerId").and_then(Value::as_str).map(ToString::to_string),
                addresses: parsed
                    .get("addresses")
                    .and_then(Value::as_array)
                    .map(|arr| arr.iter().filter_map(Value::as_str).map(ToString::to_string).collect())
                    .unwrap_or_default(),
            };
            node.connect_peer(&info).await.map(|()| json!({"connected": true})).map_err(|e| e.to_string())
        }
        "overlay-pool" => {
            let mut guard = storage.0.lock().unwrap();
            let mut store = OverlayPeerStore::new(&mut *guard);
            match store.list_all() {
                Ok(records) => Ok(serde_json::to_value(records).expect("records serialize")),
                Err(e) => Err(e.to_string()),
            }
        }
        "seed-overlay" => {
            let peer = parsed.get("peerId").and_then(Value::as_str).unwrap_or("");
            let addresses: Vec<String> = parsed
                .get("addresses")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(Value::as_str).map(ToString::to_string).collect())
                .unwrap_or_default();
            let verified = parsed.get("verified").and_then(Value::as_bool).unwrap_or(false);
            let mut guard = storage.0.lock().unwrap();
            let mut store = OverlayPeerStore::new(&mut *guard);
            store
                .remember(peer, &addresses, OverlayPeerSource::Announce, verified, now)
                .map(|()| json!({"seeded": true}))
                .map_err(|e| e.to_string())
        }
        "shutdown" => {
            print_line(&cmd_result(&id, true, json!({"stopping": true})));
            return CmdOutcome::Shutdown;
        }
        other => Err(format!("unknown cmd: {other}")),
    };

    match outcome {
        Ok(data) => print_line(&cmd_result(&id, true, data)),
        Err(err) => print_line(&cmd_result(&id, false, Value::String(err))),
    }
    CmdOutcome::Continue
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

fn parse_args() -> (u16, Option<String>) {
    let mut port: u16 = 0;
    let mut root_id: Option<String> = None;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                if let Some(value) = args.get(i + 1) {
                    port = value.parse().unwrap_or(0);
                    i += 1;
                }
            }
            "--root-id" => {
                root_id = args.get(i + 1).cloned();
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    (port, root_id)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let (port, root_id) = parse_args();
    let config = P2pConfig {
        app_version: "rust-lab-node".to_string(),
        preferred_port: Some(port),
        // 实验例程用独占端口，不做端口扫描；port=0 时 OS 分配
        port_scan: false,
        enable_tcp: true,
        enable_ws: true,
        enable_ipv6: false,
        enable_mdns: false,
        enable_upnp: false,
        // 由驱动脚本按需触发（announce/exchange 命令），保持输出确定性
        keepalive_interval: None,
        now_fn: Arc::new(spark_core::p2p::node::system_now_ms),
    };

    let (notify_tx, mut notify_rx) = mpsc::unbounded_channel::<Value>();
    let host = LabHost { root_id, notify: notify_tx };
    let storage = SharedStorage::default();
    let mut node = P2pNode::start(config, storage.clone(), Box::new(host))
        .await
        .expect("lab node starts");
    let self_peer_id = node.peer_id().to_string();

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut shutdown = false;

    while !shutdown {
        tokio::select! {
            event = node.next_event() => {
                match event {
                    Some(P2pEvent::Started { peer_id, listen_addresses }) => {
                        print_line(&json!({"type": "ready", "peerId": peer_id, "addresses": listen_addresses}));
                    }
                    Some(P2pEvent::Stopped) => {
                        print_line(&json!({"type": "event", "event": "stopped"}));
                        break;
                    }
                    Some(other) => {
                        if let Some(line) = event_json(&other) {
                            print_line(&line);
                        }
                    }
                    None => break,
                }
            }
            note = notify_rx.recv() => {
                if let Some(value) = note {
                    print_line(&value);
                }
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(text)) => {
                        if text.trim().is_empty() {
                            continue;
                        }
                        if let CmdOutcome::Shutdown = handle_command(&text, &node, &storage, &self_peer_id).await {
                            shutdown = true;
                        }
                    }
                    // stdin 关闭（驱动脚本退出）→ 同步关停
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
    }

    node.stop().await;
}
