# personal-data-sync 协议规格（pdsync · M3 epoch 选择性密钥轮换）

本文是 pdsync（个人域自设备同步）的**字节级权威规格**。语义层叙述见
`wiki/architecture/sync/personal-data-sync.md`；方案背景见
`wiki/architecture/identity/m3-epoch-rotation-plan.md`（下称「方案」）。
本文与代码冲突时以本文为准（协议守护底线：规格-实现-向量三者一致）。

实现锚点：`code/core/src/epoch/mod.rs`（纯逻辑原语）、
`code/core/src/sync/pdsync.rs`（收发面）、`code/core/src/kernel/epoch_ops.rs`（编排）。
golden vectors：`code/spec/vectors/epoch.json`（消费测试
`code/core/tests/epoch_vectors.rs`）。

---

## 1. 适用范围与密钥模型

- epoch 密钥（ikey）= **随机 32B**（AES-256-GCM 密钥），与身份 seed 无关
  （方案决策①：非 KDF(seed)）。换密码不使旧密钥不可用。
- 分发通道 = pdsync 自设备流量（root 内设备互推），永不进 orgsync 组织流量。
- epoch 密钥**不做 at-rest 封装**（方案 §5.2：内核存储已是解锁后才可读的
  加密分区；再封装一层不增安全只增复杂度）。
- 包裹（ikey 记录）**不内嵌签名**：pdsync 签名信封已证发送方身份，wrap 值内
  嵌 writerPeer 与信封 from 交叉验证（方案 §5.2 末尾）。

## 2. hello.epoch 字段

pdsync hello body 新增可选字段：

```json
{
  "vv": { ... },
  "dlogAck": { ... },
  "deviceClass": "desktop",
  "epoch": 2
}
```

- `epoch`：发送方**本机生效 epoch**（`p2p:epoch:effective`，见 §6），
  u64，缺省/缺失 = 0（等同明文时代）。
- 接收方解析后持久化到本地键 `pdsync:epoch:{peer}` = 十进制 ASCII 数字
  （本地键，不进同步流量；与 `deviceClass` 同型先例）。
- 老版本（M2 及以前）不读不写该字段 → 对端 epoch 视为 0（盲区，见 §9）。

## 3. 密文值线形

加密记录的 value 为如下 JSON 对象（字段名与类型逐字节固定）：

```json
{ "$enc": "ikey", "epoch": 2, "nonce": "<base64 12B>", "ct": "<base64>" }
```

- `$enc`：判别标记，恒为字符串 `"ikey"`。
- `epoch`：加密所用 epoch 号（u64，JSON number）。
- `nonce`：12B 随机 nonce，base64 标准表（含 padding）。
- `ct`：`AES-256-GCM(ikey, nonce, plaintext, aad)` 输出（密文 || 16B tag），
  base64 标准表。
- `plaintext` = 原 value JSON 序列化 UTF-8（紧凑无空白）。
- `aad` = **记录完整键的 UTF-8 字节**（如 `ct:friend:{rootId}`，含 category
  前缀），防密文跨记录搬迁。

判别规则（接收侧 `is_ikey_ciphertext`）：value 为 JSON 对象，且
`$enc == "ikey"`，且 `epoch` 为 u64 number、`nonce`/`ct` 为 string。
插件明文值契约上不含 `$enc` 字段，误判不可能。

## 4. epoch:state 记录

- 键：`epoch:state`（pdsync category `epoch:` 下唯一记录，whole 单记录 LWW）。
- 值线形（字段序逐字节固定，camelCase）：

```json
{ "current": 2, "rotatedAt": 1755000000000, "rotatedBy": "<peerId>", "reason": "revoke" }
```

- `current`：当前 epoch 号（u64）。全网「当前 epoch」以该记录为唯一事实源。
- `rotatedAt`：轮换时刻（Unix 毫秒，i64）。
- `rotatedBy`：发起轮换的设备 peerId（base58）。
- `reason`：`"init" | "revoke" | "password_change" | "password_reset" | "heal"`
  之一（snake_case；`password_reset`/`heal` 为 E1 口令校验器扩展，见 §13）。
- **serde 兜底**：本规格自 E1 起要求 `RotationReason` 实现带 `#[serde(other)]`
  的 `Unknown` 变体（`reason: "..."` 未知串 → 反序列化为 `Unknown`，不失败）。
  跨版本矩阵见 §9——**无兜底的旧实现收到未知 reason → epoch:state 反序列化
  失败 → fail-closed 停摆**（停在旧 effective，不泄密，属预期边界，须在 §9 注明）。
- **Unknown 回环序列化**：`Unknown` 仅在反序列化时产生；若携带 `Unknown` 的
  `epoch:state` 被重新序列化回同步流量，其 `reason` 落为字面串 `"unknown"`
  （`Unknown::as_str()`）——该值**不在**上述 wire 枚举五值内，属本地兜底形态，
  接收侧按其语义即「未知原因」处理；任何新实现不得依赖 `"unknown"` 作为正式
  reason 值（新增正式原因应扩展枚举而非复用 Unknown）。
- 合并语义：走 pdsync 既有 whole LWW（dseq/dlog 大者胜），无特殊分支。

## 5. ikey 包裹记录

- 键：`ikey:{epoch}:{writerPeer}:{recipientPeer}`
  （pdsync category `ikey:`；三段均不含冒号，左起定长切分）。
- 值线形（camelCase）：

```json
{ "wrappedKey": "<base64>", "nonce": "<base64 24B>", "ts": 1755000000000 }
```

- `wrappedKey` = 设备 DH 包裹的 32B epoch 密钥（见 §5.1）。
- `nonce` = 24B 随机 nonce（base64），**前 12B** 作 AES-GCM nonce
  （与 orgkey box 同族）。
- `ts`：包裹写入时刻（Unix 毫秒）。
- 合并语义：pdsync whole LWW。同键重复包裹幂等（同 epoch 同密钥，重写无害）。

### 5.1 包裹算法（box/unbox）

逐字节规格（实现：`epoch::box_ikey` / `epoch::unbox_ikey`）：

1. Ed25519→X25519 转换（TweetNaCl `crypto_sign_ed25519_*_to_curve25519`
   口径，复用 orgsync access 层 `ed_pk_to_x25519`/`ed_sk_to_x25519`）：
   - 公钥：Edwards 点转 Montgomery u 坐标；
   - 私钥：`clamp(sha512(seed)[0..32])`（clamp：b0&=248, b31&=127, b31|=64）。
2. DH：`shared = X25519(writer_x25519_priv, recipient_x25519_pub)`
   （`MontgomeryPoint.mul_clamped`；接收侧对称互换）。
3. **H1a 低阶点拒绝**：`shared` 全零 → 失败（防强制 DH 落低阶子群）。
4. **H1b 域分隔**：

   ```
   domainInfo = "ikey-box" 0x00 {rootId} 0x00 {epoch} 0x00 {writerPeer} 0x00 {recipientPeer}
   ```

   - `0x00` 为单字节分隔符；
   - `epoch` 为**十进制 ASCII**（无前导零）；
   - `rootId` 为 64 位小写 hex 的 UTF-8 字节；peerId 为 base58 UTF-8 字节。
5. `boxKey = sha256(shared || domainInfo)`（shared 32B 与 domainInfo 直接拼接）。
6. `wrappedKey = AES-256-GCM(boxKey, nonce24[0..12], epochKey)`（无 AAD——
   上下文已由 domainInfo 绑定），base64 输出。

无 PFS 取舍：ikey 包裹是密钥投递而非会话加密，前向保密不在威胁模型内
（对齐 orgkey-deliver §20.6 同族决策）。

## 6. 本地键（不进同步流量）

| 键 | 值 | 说明 |
| --- | --- | --- |
| `p2p:epoch:key:{N}` | base64(32B) | 本机密钥表，**全历史保留**（解历史密文） |
| `p2p:epoch:effective` | 十进制数字 | 本机生效 epoch；缺省 = 0（不加密） |
| `pdsync:epoch:{peer}` | 十进制数字 | 对端 hello 宣告的 epoch（§2） |

effective 语义：本机出站加密的 epoch 号。`min(本机 effective, 对端宣告 ?? 0)`
为逐对端实际加密 epoch；任一侧为 0 → 明文（§9 跨版本矩阵）。

## 7. DeviceRecord.devicePubKey

DeviceRecord（`device:` category）新增可选字段：

```json
{ "peerId": "...", "label": "...", "addedAt": ..., "revokedAt": null,
  "devicePubKey": "<base64 32B ed25519 公钥>" }
```

- serde `default` + `skip_serializing_if = Option::is_none`，camelCase
  `devicePubKey`；老版本写入的无该字段记录正常解析，新版本读老记录得 `None`。
- 采集链路：`p2p/identity_store.rs` 只读 `load_libp2p_pub_key`（protobuf
  解码 libp2p 公钥取 32B）→ `DeviceService::upsert_self` 加参 →
  `device_ops.rs` 兜底采集点。
- DeviceView（对前端视图）**不加**该字段（内部用途，不暴露）。

## 8. pdecl.sensitivity 声明

插件集合声明（`pdecl:` category，plugindata `CollectionDeclaration`）
新增字段：

```json
{ "collection": "...", "scope": "personal", "sensitivity": "sensitive", ... }
```

- 取值：`"normal"`（缺省）| `"sensitive"`；serde `#[default]` Normal。
- personal scope 专有，与 org scope 的 confidentiality 轴正交。
- 消费点：出站 `should_encrypt` 对 `pdoc:` 键查 pdecl 声明；
  **声明缺失 → 跳过不推**（宁缺勿滥：未声明集合不进加密推送）。

## 9. 老版本盲区与跨版本矩阵

老版本（M2 及以前）行为：不读 `hello.epoch`、不识别 `$enc` 密文值、
不写 `devicePubKey`、不同步 `epoch:`/`ikey:` category（category 白名单
不含即不推不收）。

| 发送方 \ 接收方 | 老版本（epoch 盲区） | 新版本（M3+） |
| --- | --- | --- |
| **老版本** | 明文，正常 | 明文（对端宣告缺失按 0） |
| **新版本** | **明文**（`min(eff, 0)=0`）；新版本不向盲区对端发明文敏感数据的保证**不在本期**（方案 §6 取舍） | `min(双端 effective)` 加密；任一侧为 0 → 明文 |

密钥表刷新钩子保证后升级设备能补解历史密文；老设备永远看不到
`epoch:`/`ikey:` 记录（白名单外 category 不传输），不会误存密文值。

**口令校验器（E1，§13）跨版本行为**：

| 场景 | 语义 |
| --- | --- |
| `pwv:self` 缺失（老设备/未设口令） | 门控**恒过**（不可回归项，§13.5） |
| 老设备收 `pwv:`/`pwack:` 记录 | category 白名单外不传输，无影响 |
| 无 serde 兜底实现收未知 `reason` | **fail-closed 停摆**（epoch:state 反序列化失败，停在旧 effective，不泄密）——预期边界，E1 起新实现带 `Unknown` 兜底消除 |
| `pwv:`/`pwack:` 值 | **明文豁免**（不参与 `$enc` 加密），须对口令过期设备可读以自愈（§13.3） |

## 10. 接收侧「不解不推进」语义（裁决 A，对齐 M2）

`handle_pdsync_data` 逐条处理时，先判别 `$enc`：

1. 是密文 → 用 `p2p:epoch:key:{value.epoch}` 试解（AAD = 记录完整键）。
2. **解不开（无密钥 / AEAD 失败 / UTF-8 失败）→ `continue` 跳过该条**：
   不写值、不推进 `pmeta`、dseq 不计入 `max_dseq`（防墓碑丢失 + 防洗白，
   M2 教训）。下轮对端重发或密钥表刷新后自然补解。
3. 解开后的明文 value_str 续走既有**全部**分支（pdsync/device: 粘性、
   tombstone、profile 等）——本地落库形态不变，读路径零改动；解密先于
   `device:` 粘性分支（二者正交）。

密钥表刷新钩子（批处理尾段）：合入 `epoch:`/`ikey:` 后若
`effective < epoch:state.current` → 扫 `ikey:{N}:*:{myPeer}` 试 unbox →
成功则写密钥表 + 提升 effective + 安全日志 `epoch_key_activated`；
全失败不报错，下轮重试。unbox 失败逐条记 `epoch_key_unwrap_failed`。

## 11. 安全日志事件

复用 `append_security_log`（DeviceService），事件：

- `epoch_rotated`：本机发起轮换（detail 含 `epoch`/`reason`/`rotatedBy: writerPeerId`）。
- `epoch_key_activated`：密钥表获得新 epoch 密钥并提升 effective。
- `epoch_key_unwrap_failed`：ikey 包裹解包失败（detail 含 epoch/writer）。
- `epoch_key_granted`：为晚到/新授权设备补发 ikey 包裹（detail 含 `epoch`/`recipientPeer`/`delay_hours`）。

## 12. golden vectors

`code/spec/vectors/epoch.json` 锁定以下字节级行为（消费测试
`code/core/tests/epoch_vectors.rs`）：

1. **box/unbox 往返**：固定双端 ed25519 密钥对 → peerId（libp2p 交叉验证）、
   域分隔串字节、固定 nonce24 的 wrappedKey 精确值、unbox 还原、
   错密钥/错域/低阶点拒绝。
2. **密文值**：固定 nonce12 的 `{$enc:"ikey",...}` 精确值、unwrap 还原、
   错 AAD/错密钥失败、判别规则正/反例。
3. **epoch:state 样例**：serde 线形字节级（字段序、camelCase、reason 枚举值）。

口令校验器线形由 `code/spec/vectors/pwv.json` 锁定（消费测试
`code/core/tests/pwv_vectors.rs`，随 E2 实现落地启用），详见 §13.8。

## 13. 口令校验器（pwv / pwack，E1）

设计定稿：`wiki/architecture/identity/password-change-propagation.md` §10-§13。
本节约定字节级线形与推导，为「乙 + V + D′」三件套的事实源。

### 13.1 概述

口令校验器在 **D′ 已启停（含 D′ 生效、无 ikey 包裹、设备长期失联后复联）** 时，
提供一个 **与口令绑定的、跨设备最终一致的证明**，使口令校验器（乙）能向普通设备
**证明「我仍持有本账号口令」**，从而在 D′ 启停场景下继续获得新 epoch 密钥。

线形三件套：

- **V（口令验证器）**：本地键 `pwv:self`，全网唯一，LWW 收敛。
- **ack（口令知识证明）**：键 `pwack:{peer}`，每设备一条，证明该设备已持有口令。
- **D′（门控）**：writer 把持新 epoch 密钥包，直到目标设备 `pwack:{peer}.vTs`
  覆盖最新 V。

### 13.2 V：`pwv:self` 线形（字节级）

- 键：`pwv:self`（pdsync 新 category `pwv:`，whole 单记录 LWW，明文豁免）。
- 值线形（字段序逐字节固定，camelCase）：

```json
{ "v": 1, "kdf": "scrypt", "salt": "<base64 16B>", "nonce": "<base64 12B>",
  "ct": "<base64>", "changedAt": <u64 毫秒>, "changedBy": "<peerId base58>" }
```

- `v`：格式版本，恒为 `1`（u64）。
- `kdf`：`"scrypt"`（本规格唯一取值）。
- `salt`：16B 随机盐，base64 标准表（含 padding）。
- `nonce`：12B AES-GCM nonce，base64 标准表。
- `ct`：AES-256-GCM 密文（明文 8B `"spark-pwv1"` + 16B tag），base64 标准表。
- `changedAt`：本次口令变更时刻（Unix 毫秒，u64）。**水位/回放基准**。
- `changedBy`：发起变更的设备 peerId（base58）。
- 合并语义：pdsync whole LWW（changedAt 大者胜；同 changedAt → changedBy
  lexicographic 大者胜），**全网最终收敛为一个口令赢家**（单口令不变量）。

**Kverify 与 ct 推导**（实现：E2 `core::pw::verify_value` / `build_value`）：

```
Kverify = scrypt(P2, salt, N=32768, r=8, p=1, dkLen=32)   // N=32768 同身份文件
ct      = AES-256-GCM(Kverify, nonce12, "spark-pwv1")     // 无 AAD
```

- `P2` = 变更后的口令 UTF-8。
- N=32768 与身份文件 scrypt 参数一致（`identity` 模块；maxmem 取同值 64MB）。
- `"spark-pwv1"` 为公开常量明文（8 字节），非秘密——V 的正确性靠
  **scrypt 计算成本**（防离线路由伪造）而非明文保密。
- 无 AAD：上下文由记录自身（salt/nonce 随 V 存储）绑定。
- **verify**：给定候选口令 X 与 V，算 `scrypt(X, salt)` 试解 `ct`，
  还原 `"spark-pwv1"` 则 V 对 X 成立。

### 13.3 明文豁免分类

- `pwv:` / `pwack:` category 走 **`should_encrypt` 明文豁免**（与 `epoch:`/`ikey:`/`pdecl:` 同族）。
- 理由：口令变更后，**对口令过期/长期失联设备必须可读 V** 以自愈（§13.6）；
  若 V 用旧 epoch 密钥加密，过期设备恰恰解不开，死锁。
- 密文值判别（§3）不受影响：V 明文对象不含 `$enc` 字段，误判不可能。

### 13.4 ack：`pwack:{peer}` 线形（字节级）

- 键：`pwack:{peer}`（pdsync 新 category `pwack:`，明文豁免，每设备一条）。
- 值线形（字段序逐字节固定，camelCase）：

```json
{ "v": 1, "vTs": <u64 毫秒>, "mac": "<base64 32B>" }
```

- `v`：格式版本，恒为 `1`（u64）。
- `vTs`：被证明的 V 的 `changedAt`（u64 毫秒）。
- `mac`：口令知识证明，HMAC-SHA256 32B，base64 标准表。

**Kack 与 mac 推导**（实现：E2 `core::pw::derive_kack` / `compute_ack_mac`）：

```
Kack = sha256(Kverify || "spark-pwack")                          // Kverify 32B 直接拼接 12B 标签
mac  = HMAC-SHA256(Kack, "pwack" || peer_b58_bytes || decimal_ascii(vTs))
```

- `Kverify` = §13.2 中 V 验证所得 32B。
- `"spark-pwack"` 为公开常量标签（12B）。
- MAC 输入为**直接拼接无分隔符**：固定前缀 `"pwack"`（5B）后接
  `peer` 的 base58 UTF-8 字节，再接 `vTs` 的**十进制 ASCII**（无前导零）。
  前缀定长、base58 与十进制数字域互斥，解析无歧义。
- 验证：同法重算 `mac` 比对；`vTs` 必须 ≥ 对应 V 的 `changedAt`
  （防对旧 V 的过期 ack 冒充当前口令知识）。

**可信锚模型（D2 裁定，§13.6 设计）：ack 记录本身永远不可信**。

- 同 root 自设备共享根签名管道，任何一台都能向 `pwack:{peer}` 写任意值
  （设备钥签名不构成门槛，同盘可提取）；故 **ack 记录只做 MAC 校验，不做门控判定输入**。
- 新增纯逻辑 `verify_and_anchor_ack(storage, peer, kverify, now_ms)`：
  读 `pwack:{peer}` → `derive_kack(kverify)` → `verify_ack_mac` →
  **通过则** 本地锚 `p2p:pw:lastVerifiedVTs:{peer} = max(现有, ack.vTs)`
  （单调只增不减）；**失败则零状态写**（伪造 ack 的最大危害 = 触发一次失败的
  MAC 校验，零污染）。
- `should_gate` **只读可信锚，ack 记录退出判定**（见 §13.5）。

**Kverify 派生时机**：writer 角色只在解锁态承担（pdsync 仅解锁态运行），
解锁即持会话口令；Kverify = `scrypt(会话口令, pwv.salt)` 在门控检查点现派
（每检查点一次 scrypt，与 unlock 同级成本），**永不落盘**，与口令同生命周期。
纯逻辑以派生密钥入参，不以口令入参（不污染签名）。

**锚定时机（双钩子，幂等）**：① pdsync-data 批尾钩子——本批合入 `pwack:` 键后
逐条 `verify_and_anchor_ack`（与 `try_refresh_keys` 同型，解锁态保证口令在场）；
② 门控判定前兜底——rotate/maybe_grant 内对 `ack.vTs > 锚` 的设备先试锚定
（防「ack 到了钩子未跑」时序窗）。锁定期间到达的 ack 在下次解锁后锚定
（ack 是持久记录，无丢失）。

### 13.5 应用守卫与门控（乙 侧 / writer 侧）

**乙（V 自证 + 回放防护 + 入站防伪造，D2/🟠1 裁定）**：

1. **入站专用分支**（`handle_pdsync_data` 对 `pwv:self` 独立处理，device: 粘性同型）：
   - **解析失败 → continue，不推进 pmeta**（M2 裁决 A 惯例；fail-closed）。
   - **水位单调**：`changedAt > appliedVTs` 才接受；否则回放，忽略。
   - **未来 ts 拒收**：`changedAt > now + 10min` → **拒**（伪造者要推高水位
     必须用未来 ts；合法 V 的 changedAt≈当前时刻，不受反熵延迟影响）。
     缺此条，伪造 V（changedAt=9e15）推死水位 → 真 V 永被挡 → 不可自愈 DoS。
   - **last-good 保留**：接受新 V 前把当前已验证 V 副本写 `p2p:pw:lastGoodV`
     （本地键）；`verify_password_ticket` 对从未成功验证的 V 失败 →
     记 `pw_verifier_mismatch` + UI 警示（三段式文案③「回设密设备重改」逃生）。
     **自动回退不做**（回退动作本身可被伪造 V 触发，语义复杂化；保留+警示足够，
     列为后续增强）。
2. 用**本机会话口令 X** 试 verify（§13.2）。
   - 成功 → 应用 V、推进水位 `p2p:pw:appliedVTs = changedAt`，落 last-good。
   - 失败/无口令 → **保持 last-good，不推进水位**，记安全日志
     `pw_verifier_mismatch`（detail 含 `changedAt`——实现字段名
     `expectedChangedAt`，与 `changedAt` 为同一值）。V 的正确性在首次使用时自证。
3. last-good：本地保留最后一个已验证成功的 V 的 `changedAt` 与 `ct`
   （回退兜底，防伪造 V 顶掉真口令）。

**D′（writer 门控，§13.1，D2 可信锚模型）**：

- writer 把持新 epoch 密钥包，直到目标设备可信锚追上最新 V。
- **门控只读可信锚 `p2p:pw:lastVerifiedVTs:{peer}`，ack 记录退出判定**：
  - **Pass ⇔ `lastVerifiedVTs:{peer} >= pwv:self.changedAt`**。
  - **曾验证 ⇔ `lastVerifiedVTs:{peer} > 0`**，grace 从 **`lastVerifiedVTs:{peer}`**
    起算（非 ack.vTs——伪造大 vTs 推高 grace anchor 的次生洞同根关闭）。
  - **从未验证 ⇔ `lastVerifiedVTs:{peer} == 0`** → 立即暂扣，无 grace
    （含新配对/重配对，须先完成一次口令 verify + MAC 校验锚定才解锁）。
  - grace 判定：`now < lastVerifiedVTs:{peer} + graceMs` 则宽限放行，否则暂扣。
- **graceMs 宽限（最终决定：默认 7 天）**：分两类设备（曾验证吃 grace /
  从未验证不吃），heal 24h 与 graceMs 7d **解耦**（两套独立计时，互不影响）。
- 不满足（含锚定失败）→ **暂扣不发**（不删 ikey 包裹，可补发），记安全日志
  `pw_grant_gated`。重放旧合法 ack 无害（vTs 语义自带单调，旧 ack 不覆盖新 V；
  锚点只增不减，旧 ack 的 vTs < 锚点，锚定无效果）。
- **不可回归项**：`pwv:self` 缺失（含老设备/未设口令）→ 门控**恒过**，
  不得因 V 缺失而停摆（旧版语义，§9 矩阵）。

**水位/状态本地键**（不进同步流量）：

| 键 | 值 | 说明 |
| --- | --- | --- |
| `p2p:pw:appliedVTs` | 十进制数字 | 已应用 V 的 changedAt；单调不减，回放基准 |
| `p2p:pw:lastVerifiedVTs:{peer}` | 十进制数字 | 每设备可信锚；仅 `verify_and_anchor_ack` 在 MAC 校验通过时单调推进；0=从未验证（决定是否吃 grace） |
| `p2p:pw:lastGoodV` | 序列化 V | 接受新 V 前保留的最后一个已验证 V 副本（本地回退兜底） |
| `p2p:pw:graceMs` | 十进制数字 | D′ 宽限默认 `604800000`（7 天）；可配置覆盖 |

**安全日志 kind（本规格为字节级权威，实现须对齐此名单）**：

| kind | 触发 | 典型 detail |
| --- | --- | --- |
| `pw_verifier_published` | 发布新 V（改密 / 重置 / 创世 / unlock 懒发布） | `{deviceId, changedAt}` |
| `pw_ack_auto` | unlock 后自动回执 ack（证明口令知识，非重封） | `{deviceId, vTs}` |
| `pw_verified_resealed` | 验票 + 重封（unify 流程中 V 验通过并重封身份文件） | `{deviceId, vTs}` |
| `password_unified_after_reset` | 重置后口令收敛（unify 完成，reason=password_reset） | `{deviceId, vTs}` |
| `pw_grant_gated` | D′ 暂扣新密钥（超 graceMs / 从未验证设备） | `{peer, epoch, latestVTs, ackVTs, graceRemainingMs}` |
| `pw_verifier_mismatch` | verify 失败（含伪造 V 拒用 / 无口令） | `{expectedChangedAt}` |

- 该名单对齐设计定稿 `password-change-propagation.md` §13.1（E1/E5 行「安全日志四/五类」）并增补
  `pw_ack_auto`（实现产物，语义独立：unlock 自动 ack 与验票重封是两次可观测动作）。
- **实现终态（E3 需按此回改）**：`pwv_published`→`pw_verifier_published`、
  `password_unified`→`password_unified_after_reset`；`pw_verified_resealed` 补出；
  `pw_ack_auto` 保留。

### 13.6 heal 规则

- 触发：某设备**已验证**一个正确 V（非 last-good 回退），但其 epoch 密钥包
  **包裹超时（24h）未达**。
- 动作：该设备以 `reason = "heal"` 自行 `rotate(Heal)`（复用 M3 轮换）+
  **重发 V**（重新 publish `pwv:self`，触发全网再次 LWW）。
- 目的：自愈 D′ 启停后无人补发密钥的停摆；新 epoch 密钥随重发后的门控重新发放。
- 不改变 V 的口令内容，仅重发触发收敛。

### 13.7 并发收敛

- 多设备并发改口令 → 多份 `pwv:self` 发布 → pdsync whole LWW
  （changedAt 大者胜，同值按 changedBy 决），**最终一个口令赢**（§13.2）。
- 未赢者的设备凭自身 last-good 继续工作；其 ack 的 `vTs` 落后于赢家 V 时，
  D′ 门控会暂扣其新密钥，直到其 ack 覆盖赢家 V——触发重新 verify/提示重新输入。

### 13.8 golden vectors（pwv.json）

`code/spec/vectors/pwv.json` 由 `code/core/examples/gen_pwv_vectors.rs` 生成，
锁定 13 条字节级行为（实现须用同推导复现，消费测试 `pwv_vectors.rs` 随 E2 启用）：

1. `pwv_build_verify_roundtrip`：固定 P2/salt/nonce 的 Kverify/ct 精确值，
   正确口令还原明文、错口令失败。
2. `pwv_kack_and_ack_mac`：Kack 派生 + ack MAC 精确值。
3. `pwv_forged_rejected_last_good`：错口令密文用真口令解不开 → 拒用，last-good 保留。
4. `pwv_watermark_monotonic`：`vTs <= appliedVTs` 水位拒应用，水位只增不减。
5. `pwv_self_serde_bytes`：`pwv:self` 线形字节级（字段序 v,kdf,salt,nonce,ct,changedAt,changedBy）。
6. `pwack_wire_bytes`：`pwack:{peer}` 值线形字节级（v,vTs,mac）。
7. `pwv_missing_gate_pass`：无 V 旧版语义——门控恒过（不可回归项）。
8. `epoch_state_reason_password_reset`：`reason:"password_reset"` serde 线形样例。
9. `epoch_state_unknown_reason_fallback`：未知 reason → `Unknown` 兜底反序列化，
   不 fail-closed（对应 §4 serde 兜底与 §9 边界）。
10. `pseudo_ack_not_anchored`：伪 ack（错 MAC）`verify_and_anchor_ack` → **不锚定**，
    锚 `lastVerifiedVTs:{peer}` 不变，门控不 Pass（D2 攻击链回归）。
11. `valid_ack_anchored_gate_pass`：合法 ack（正确 MAC）锚定后锚=ack.vTs，
    `should_gate` Pass（`锚 >= pwv.changedAt`）。
12. `pwv_future_ts_rejected`：`changedAt > now + 10min` → **拒收**，水位不推进
    （防伪造 V 推死水位 DoS）。
13. `pwv_replay_rejected`：`changedAt <= appliedVTs` → **回放拒收**，水位不变。

> **实现契约（E2+D2-F1）**：`core::pw` 须暴露
> `build_value / verify_value / derive_kack / compute_ack_mac / verify_and_anchor_ack /
> should_gate(trusted-anchor) / apply_value(watermark+future-ts) / ack_covers /
> grace_remaining`，并按 §13.2/§13.4/§13.5 推导复现上述向量。**门控只读可信锚**：
> `should_gate` 以 `p2p:pw:lastVerifiedVTs:{peer}` 为唯一输入（Pass ⇔ 锚 >= pwv.changedAt；
> grace 从锚起算，默认 7 天）；**绝不读 ack.vTs 做判定**。`verify_and_anchor_ack`
> 仅 MAC 校验通过时单调推进锚点（失败零污染）。`apply_value` 增 future-ts 拒
> （`changedAt > now+10min` → reject）。`should_encrypt` 明文豁免加入
> `pwv:`/`pwack:` category；`RotationReason` 加 `PasswordReset`/`Heal` +
> `#[serde(other)] Unknown`。

## 14. blob 层：内容寻址分块 + presence 账本 + 按需回补（A1）

设计权威：`docs/architecture/foundation/personal-data.md` §4.1–4.3（数据二分、
blob 层、按需回补）。本节约定字节级线形。实现锚点：
`code/core/src/sync/blob/`（纯逻辑）、`code/core/src/kernel/inbound_dm/blob_fetch.rs`
（dm 入站编排）。golden vectors：`code/spec/vectors/blob.json`
（消费测试 `code/core/tests/blob_vectors.rs`，生成器
`code/core/examples/gen_blob_vectors.rs`）。

### 14.1 寻址与分块

- `cid = sha256hex(blob 内容)`：SHA-256 摘要的小写 hex（64 字符）。
- 分块阈值 `CHUNK_THRESHOLD = 1 MiB`（1048576 字节）；定长块
  `CHUNK_SIZE = 256 KiB`（262144 字节）。分块规则（`size` = 内容字节数）：
  - `size == 0` → 0 块（`chunkCids = []`）；
  - `0 < size ≤ CHUNK_THRESHOLD` → **单块**：整块即全部内容，
    `chunkCids = [cid]`（单块 chunkCid 恒等于 cid）；
  - `size > CHUNK_THRESHOLD` → 按 256 KiB 定长切分，`n = ceil(size / CHUNK_SIZE)`
    块，第 `i` 块为 `data[i*CHUNK_SIZE .. min((i+1)*CHUNK_SIZE, size)]`，
    `chunkCids[i] = sha256hex(第 i 块)`；尾块长度为 `size - (n-1)*CHUNK_SIZE`（≥1）。
- 分块规则是内容的纯函数：任何设备对同一内容重算，cid 与 chunkCids 逐字节一致
  （manifest 无需随记录传输即可被持有者独立复算）。

### 14.2 manifest 线形（字节级）

```json
{"v":1,"cid":"<64hex>","size":1048577,"chunkSize":262144,"chunkCids":["<64hex>",...]}
```

- 字段序逐字节固定：`v, cid, size, chunkSize, chunkCids`；紧凑 JSON（无空白）。
- `v` 恒 1；`chunkSize` 恒 262144（名义块长，单块 blob 同样填 262144）；
  `size` 为 u64 JSON number。
- 结构校验（入库前必过，任一不符即拒）：
  `v == 1`；`cid`/`chunkCids` 各项均为 64 位小写 hex；`chunkSize == 262144`；
  `|chunkCids| == 分块规则(size)`；`size == 0` 时 `chunkCids` 为空；
  单块（`0 < size ≤ 1 MiB`）时 `chunkCids[0] == cid`。
- manifest 本体不签名、不哈希自指——其正确性由「逐块 sha256 == chunkCid +
  拼接整体 sha256 == cid」在装配读出时终验（§14.6）。

### 14.3 存储键

| 键 | 值 | 性质 |
| --- | --- | --- |
| `blob:meta:{cid}` | manifest 规范 JSON 字符串 | 本地键，不进同步流量 |
| `blob:chunk:{chunkCid}` | 块内容 base64（标准表含 padding） | 本地键，不进同步流量 |
| `blob:asm:{chunkCid}` | 装配中暂存（base64 追加式） | 本地键 |
| `blob:freq:{chunkCid}` | 上次拉取请求时间戳 ms（十进制 ASCII） | 本地键，节流用 |
| `blob:presence:{cid}:{deviceUid}` | presence 记录（§14.4） | **pdsync 同步**（核心数据通道） |

消息/文件记录只存 `cid` 引用，不内嵌内容。`blob:meta:`/`blob:chunk:` 等
本体键不在任何 pdsync category 前缀内（与 P6 `blob:data:` 先例同型；
前缀互不重叠——`blob:meta:`/`blob:chunk:`/`blob:asm:`/`blob:freq:`/`blob:presence:`
与既有 `blob:data:`/`blob:part:`/`blob:req:`/`blob:want:`/`blob:unref:` 两两无包含）。

### 14.4 presence 账本线形与 pdsync category

- 记录键：`blob:presence:{cid}:{deviceUid}`（deviceUid = 设备稳定标识，
  32 位小写 hex，见 DeviceRecord.deviceUid；每设备每 blob 至多一条，天然无并发冲突）。
- 值线形（字段序逐字节固定，camelCase）：

```json
{"v":1,"cid":"<64hex>","deviceUid":"<32hex>","chunks":"<base64>"}
```

- `chunks` 是持有位图：bit `i` 置位 = 本机持有第 `i` 块；字节序
  `byte[i/8]`、位序 LSB-first（bit `i%8`）；编码为**最短长度**（尾部零字节
  裁剪）的 base64 标准表；空位图（无任何块）编码为空字符串 `""`。
- 记录经 `put_personal` 受管写入（pmeta 版本向量 + LWW），纳入 pdsync
  category `blob:presence`（前缀 `blob:presence:`）随反熵扩散——**任何设备
  都能凭相同记录集合确定性复算每个 blob 的域内副本数**（§14.7 向量锁定）。
- 持有变更即刷新：本机落块/弃块后按 manifest 重算位图重写本机记录；
  位图全零时以 `delete_personal` 墓碑化该记录（墓碑随 dlog 传播，即
  「presence 同步标记移除」）。
- 灰度推送门控（与 `mkt:ann` 同先例）：`blob:presence` 是新增 category，
  老端入站白名单（`category_for_key`）不认识会整批拒收——仅当对端 hello
  声明该 category 才主动推；老端不声明即不推（老端语义=无 K 上限的 eager
  全量副本，安全降级）。epoch 生效时按默认规则加密（自设备共享 epoch 密钥）。
- 副本计数（纯函数，A2 驱逐选择器与 A3 健康度共用输入）：
  - **完整副本数** = presence 记录中位图完整（全部 n 位置位）的设备按
    deviceUid 去重计数；
  - **逐块副本数** = 各记录位图按位求和（块 i 的副本数 = 持有块 i 的设备数）。
  计数需要本机持有 manifest（块数 n 的唯一来源）；无 manifest 不可计数。

### 14.5 blob-fetch / blob-chunk 信封

复用 dm 直连通道（自设备间，验签与 `from == 自己 rootId` 口径同 pdsync）。
老版本不识别按未知 kind 丢弃（既有先例）。两个 kind 均入限流豁免名单
（背靠背多块拉取，与 `pdsync-attachment-req/resp` 同族）。

- `blob-fetch` 请求体（二选一）：
  - `{"chunkCid":"<64hex>","offset":<u64>}` — 拉取块内容；`offset` 缺省 0。
  - `{"cid":"<64hex>"}` — 拉取 manifest。
- `blob-chunk` 响应体（三选一）：
  - `{"chunkCid":"<64hex>","data":"<base64>"}` — 块内容整块应答
    （`offset` 缺省 0、`totalBytes` 缺省 = data 解码长度，即设计钉定的最简形态）；
  - `{"chunkCid":"<64hex>","offset":<u64>,"data":"<base64>","totalBytes":<u64>}` —
    分片应答（仅单块 blob 超过传输切片时使用，见下）；
  - `{"cid":"<64hex>","manifest":<manifest 对象>}` — manifest 应答；
  - `{"chunkCid":"<64hex>","missing":true}` / `{"cid":"<64hex>","missing":true}` —
    诚实否定（本地无此块/manifest；请求方下轮重试或换持有者）。
- **传输单元**：块 ≤256 KiB 时单信封整块传输（base64 约 341 KiB，远低于
  dm 单帧 1 MiB 上限）；单块 blob（256 KiB < size ≤ 1 MiB）按 240 KiB
  切片以 offset 顺序续拉（240 KiB = 3 的倍数，接收侧 base64 直接追加即
  完成拼接；线形同 P6 `pdsync-attachment-req/resp` 先例）。
- **持有即做种**：任何持有 `blob:chunk:{chunkCid}`（或 `blob:meta:{cid}`）
  的设备均可应答，不论其 presence 位图是否已传播。

### 14.6 校验与降级语义

- 块入库：收齐（offset 续拉完成）后 `sha256(内容) == chunkCid` 才落
  `blob:chunk:`，不符即弃（不落库、不计 presence）；装配暂存 `blob:asm:` 清除。
- manifest 入库：过 §14.2 结构校验才落 `blob:meta:`；落库后按本机已持块
  重算位图刷新 presence（块可能先于 manifest 到达）。
- 装配读出：本地齐块 → 按 manifest 顺序拼接 → `sha256(拼接) == cid` 终验
  通过才返回字节；终验失败按本地数据损坏报错（不静默返回）。
- **诚实降级**：读 blob 时本机缺块/manifest 且 presence 账本中无（在线）
  持有者 → 返回「暂不可用」，不落假数据；持有者上线后由读路径重新规划
  拉取（拉取是幂等可重入的，offset 续拉天然断点续传）。
- 回补成功（落块 + 刷新 presence）后本机即成为新副本——副本数可能瞬时
  >K，允许（K 语义落点是 A2 配额驱逐收敛，不拦回补路径）。

### 14.7 golden vectors（blob.json）

`code/spec/vectors/blob.json` 由 `code/core/examples/gen_blob_vectors.rs`
生成，锁定以下字节级行为（消费测试 `code/core/tests/blob_vectors.rs`）：

1. `manifest_small_blob`：小 blob（1000B，确定性内容）的 cid 与 manifest
   规范 JSON 逐字节。
2. `chunking_empty_blob`：空 blob —— cid = sha256("")，0 块，manifest 逐字节。
3. `chunking_threshold_minus_1`：1048575B → 单块（chunkCids=[cid]）。
4. `chunking_threshold_exact`：1048576B（恰阈值）→ 单块。
5. `chunking_threshold_plus_1`：1048577B → 5 块（4×262144 + 1B），
   逐块 chunkCid 精确值。
6. `presence_record_bytes`：presence 记录规范 JSON 逐字节 + 位图编解码
   （9 块持有 {0,3,8} → `CQE=` 等）+ 最短长度裁剪规则。
7. `presence_deterministic_count`：3 设备 presence 记录集合（2 完整 + 1 部分），
   两种插入顺序下完整副本数与逐块副本数一致。
8. `fetch_envelope_bodies`：blob-fetch（chunk/manifest）与 blob-chunk
   （整块/分片/missing/manifest）请求响应体线形逐字节。

确定性内容生成规则（向量不内嵌大内容）：`byte[i] = (i % 251) as u8`
（与 `plugindata::blob` 测试同口径）。

## 15. 配额与驱逐（A2）

设计权威：`docs/architecture/foundation/personal-data.md` §4.4（驱逐不杀最后副本）。
实现锚点：`code/core/src/sync/blob/quota.rs`（配额/水位/K 语义/访问键）、
`code/core/src/sync/blob/evict.rs`（驱逐选择器与执行、GC 机制）。
golden vectors：`code/spec/vectors/blob_quota.json`（消费测试
`code/core/tests/blob_quota_vectors.rs`，生成器
`code/core/examples/gen_blob_quota_vectors.rs`）。

### 15.1 配额声明：`blobQuota`

- hello 扩展字段 `blobQuota`（u64，字节；老端不读该字段天然兼容，
  与 §5.1 各扩展字段同先例）。
- 取值：本地键 `blob:quota`（十进制 ASCII 字节数；**本地键，不进同步**）
  的用户配置；未配置时按设备类默认：PC = 10 GiB（10737418240），
  移动 = 1 GiB（1073741824）。
- 对端收到的配额持久化为 `pdsync:blobquota:{peer}`（本地键，`devclass`
  先例），备健康度等后续消费。

### 15.2 访问跟踪与水位

- 访问键 `blob:access:{cid}`（十进制 ASCII ms；**本地键**）：
  写入（save_blob）、回补落块/落 manifest（ingest）、读出装配成功
  （read_blob）即刷新；驱逐/GC 时随 chunk 一并删除。
- **配额水位** = 本机 `blob:chunk:` 全部值的原始字节数之和
  （base64 长度折算：`len/4*3` 减 padding）；manifest 与 access 等
  小键不计入（量级可忽略，口径固定）。

### 15.3 K 语义

- `K = min(3, 域内未撤销设备数)`（设备清单 `device:` 记录中
  `revokedAt` 为空者计数，含本机；至少为 1）。
- 设备 1/2/3/5 台的副本目标分别为 1/2/3/3——设备 ≤3 台时 K 退化
  为全量（副本上限自然等于设备数），驱逐选择器此时恒无候选
  （完整副本数 ≤ 设备数 = K，硬规则 1 永不放行）。

### 15.4 驱逐选择器（位次规则，内核纯逻辑）

输入：本机 manifest 集合、presence 账本、`blob:access:`、本机
deviceUid、K、待释放字节数。输出：驱逐 cid 有序列表。

- **候选**：本机**完整持有**（全部 chunk 在库）的 blob；部分持有是
  回补瞬时态，不做驱逐候选。
- **完整持有者集合** `H(cid)`：presence 账本中位图完整的 deviceUid，
  **升序排序**。
- **硬规则 1（副本保护）**：`|H| ≤ K` → 永不驱逐（驱逐不杀最后副本，
  在选择器内强制，不靠调用方自觉）。
- **硬规则 2（位次规则）**：本机 deviceUid 在 `H` 中的位次
  `rank ≥ K` 才可驱逐（本机是富余副本）；`rank < K` 或本机不在 `H`
  （账本未覆盖）→ 永不驱逐。位次规则是硬规则 1 的并发安全强化：
  `|H| = r > K` 时恰好第 K..r-1 位的 `r-K` 个持有者各自独立得出
  「我该驱逐」，**无需任何时序协调即收敛到 K 副本**；前 K 位持有者
  即使自身超配额也不动该 blob。
- **排序**：候选按 `access` 升序（最久未访问优先），同值按 cid
  字典序（确定性）。
- **截断**：按序累计 `manifest.size` 直到 ≥ 待释放字节数即停；
  候选耗尽仍未凑够则保持超配额（诚实，不越规则驱逐）。
- **执行**：逐 cid 调 `drop_local_chunks`（§14：只删 chunk 留
  manifest，presence 全零墓碑化随 dlog 传播）；执行键
  `blob:evict:last`（十进制 ASCII ms；本地键）节流，周期触发点挂在
  pdsync hello 调和（P6 `gc_blobs` 同位置），间隔 10 分钟。

### 15.5 blob 删除与 GC（机制；周期接线待 A4）

- 消息/文件记录删除（dlog 传播）后其 cid 引用消失，本机该 blob 的
  chunk 进入「无引用」集合，由本地 GC 清理——**不占驱逐通道、不受
  硬规则 1/2 约束**（记录已删则副本语义随记录生命周期结束）；
  清理删除 chunk + manifest + access 键，presence 墓碑化传播；
  远端副本是否清理由各设备自主（与「已发出的副本收不回」一致）。
- A2 交付机制：`plan_gc(held, referenced)`（纯函数：held − referenced）
  与 `gc_unreferenced(storage, referenced, …)`（引用集合显式注入）。
  **引用收集器的完备性依赖 A4**（存量补登定义消息/文件 → cid 的引用
  形态；当前无任何记录引用 blob 层 cid，空引用集会误清全部——
  故 A2 不把 GC 挂周期触发，A4 落地引用形态后接线）。

### 15.6 golden vectors（blob_quota.json）

1. `k_target_semantics`：设备 1/2/3/5 台 → K = 1/2/3/3。
2. `quota_defaults`：PC/移动默认配额字节数。
3. `eviction_k_equal_never`：`|H| = K` 任何龄期都无候选（硬规则 1）。
4. `eviction_over_k_by_age`：`|H| = 4, K = 3`，本机 rank 3（富余），
   多 blob 不同龄期 → 按龄选至凑够待释放量；同龄按 cid 序。
5. `eviction_i_am_keeper`：`|H| = 4, K = 3`，本机 rank 0 → 无候选
   （硬规则 2，即使超配额）。
6. `eviction_concurrent_converges`：`|H| = 5, K = 3`，分别以
   rank 2/3/4 三个设备视角跑选择器 → rank 2 不逐、rank 3/4 逐 →
   并发独立决策后副本精确收敛到 3 = K。

## 16. 存量附件补登与引用收集（A4）

设计权威：`docs/architecture/foundation/personal-data.md` §五.1（先例：
dlog 引入时的墓碑补登 §5.6）。实现锚点：`code/core/src/sync/blob/migrate.rs`。

### 16.1 补登形态：持续调和（幂等稳态），非一次性标记

- 存量附件存于 P6 `blob:data:{hash}`；P6 hash 与 blob 层 cid 同为
  `sha256hex(内容)`——**同一寻址，既有 `$blob` 引用天然即 cid 引用，
  记录零改写**。
- 补登以**持续调和**落地（挂在 pdsync hello 调和，P6 `gc_blobs` 同位置）：
  每轮扫描「`pdoc:` 引用 ∩ `blob:data:` 在库」的 hash——
  - blob 层已完整持有 → 删除 `blob:data:` 条目（去重）；
  - 否则读内容经 `save_blob` 注册（manifest + chunk + presence，幂等），
    然后删除 `blob:data:` 条目；注册不写 `blob:access:`（存量访问时间
    未知 = 最老，驱逐龄期诚实）。
- 持续调和而非一次性 done-flag 的理由：新写入（插件 saveBlob / P6 拉取
  落库）仍先落 `blob:data:`，持续调和把**存量与增量统一收口**进 blob 层，
  幂等稳态（每轮只处理「`blob:data:` 在库且层内未完整」的项，稳态零写）。
- 仅 `pdoc:` 引用驱动的补登：feed 收存（跨联系人，非个人域数据）不迁入，
  继续走 feed-blob 通道（`blob:data:` 保留）。

### 16.2 读穿语义（`plugindata::blob`）

`read_blob` / `has_blob` / `serve_chunk` 在 `blob:data:` 未命中时回退
blob 层（hash == cid 同一寻址；层内完整持有即命中）：

- 插件读附件、P6 `pdsync-attachment` 服务方、feed `feed-blob` 服务方
  三条既有路径零改动获得层内容源——**混跑期旧设备照常从新设备的
  blob 层拉取**（旧设备协议不变，新设备只是内容源多了一层）；
- P6 拉取落库仍写 `blob:data:`（协议线形不变），下一轮补登调和迁入
  blob 层并删除 `blob:data:`——稳态后 `blob:data:` 仅存在于
  「feed 收存」与「补登调和间隙」。

### 16.3 引用收集形态与 GC 周期接线（§15.5 落地）

- **引用收集器** `collect_references`：扫描 `pdoc:`、`feed:inbox:`、
  `msg:item:` 三个前缀的全部记录值，递归收集 `{"$blob": hash}` 引用
  （`plugindata::blob::blob_refs_in` 既有口径）。这是 v1 的全部引用
  形态；**新增引用形态必须先修本节再扩收集器**（GC 安全前提）。
- **GC 周期触发**：挂 pdsync hello 调和（节流键 `blob:gc:last`，
  10 分钟），对 `collect_references` 之外的层内 blob 执行
  `gc_unreferenced`（清 chunk + manifest + access，presence 墓碑化
  传播；不占驱逐通道、不受位次规则约束——§15.5）。
- 负向保证：凡被三个前缀任一记录引用的 cid，GC 必不清。

### 16.4 驱逐标记（防驱逐—重拉死循环）

补登调和与 P6 eager 调和并存引入一个闭环风险：配额驱逐弃块 → P6
调和（`missing_blobs`）发现 `$blob` 引用缺失 → 立即重拉回
`blob:data:` → 补登调和再迁入 blob 层 → 驱逐失效成死循环。以
**驱逐标记**断开：

- 标记键 `blob:evicted:{cid}`（本地键，值 `"1"`）：**仅配额驱逐**
  （`evict_over_quota`）设置；`drop_local_chunks` 的手动调用不设置。
- P6 调和 `missing_blobs` 跳过带标记的 hash（配额语义接管该 blob 的
  副本生命周期，P6 不再自动重拉）；
- 标记解除（显式意图优先于配额驱逐）：
  - `mark_want`（插件读未命中 → 用户显式要读）；
  - `save_blob`（同 cid 重新写入）；
  - blob 层回补落块完成（`ingest_chunk` 收齐，即 read_or_plan/blob-fetch
    驱动的回补成功）；
  - `gc_unreferenced` 清理时一并删除标记键。

### 16.5 协议兼容

- 旧设备不识别 `blob-fetch`/`blob-chunk` 按未知信封丢弃（既有先例）；
  混跑期旧设备仍是 eager 全量副本（`blob:data:` 语义不变）。
- 新设备对旧设备：P6 服务方读穿 blob 层，旧设备拉取无感；旧设备对
  新设备：`blob:data:` 照常服务。

### 16.6 健康度聚合口径（A3，personal-data §4.5）

域级摘要（`core::sync::blob::blob_health`，壳层 `root-blob-health`）——
**确定性计算，任何节点凭相同 presence 账本 + manifest 集合复算一致**；
只提醒不处置（Q06 口径，零机制性干预）：

- `deviceCount`：未撤销设备数（核心数据全量副本数即此值）；
- `kTarget = min(3, deviceCount)`（§15.3）；
- `totalBlobs` / `totalBytes`：本机有 manifest 的 blob 数与内容字节总量
  （presence 计数以本机 manifest 为块数来源，无 manifest 不可计数，
  不纳入统计全集——诚实口径）；
- `underKBlobs`：完整副本数 <K 的 blob 数；
- `minFullReplicas`：全部 blob 完整副本数最小值（无 blob 为 null）；
- `quota`：本机配额水位（§15.2 QuotaStatus 直通）。

UI 头部「你当前只有 N 份副本」：有 blob 取 `minFullReplicas`，无 blob
按核心数据口径取 `deviceCount`（单设备用户 N=1）；设备 ≤3 台退化
全量在 K 行如实表达。
