# 身份模块规格（identity）

> 来源：反向提取自 `desktop/src/main/identity/root-id.ts`。Rust 实现必须逐字节对齐本规格，
> 全部算法以 golden vectors 验收（向量文件在 code 仓库 `spec/vectors/identity.json`）。

## 1. 助记词

- BIP39，256 位熵，24 词
- 词表：`chinese_simplified`（生成默认）；恢复时必须同时接受 `english`（v1 遗产）
- passphrase 固定字符串：`Polykey`
- 种子 = BIP39 mnemonicToSeed(mnemonic, "Polykey")，64 字节

## 2. SLIP-0010（ed25519 派生）

- master：I = HMAC-SHA512(key="ed25519 seed", data=seed)；key=I[0:32]，chainCode=I[32:64]
- 子节点（仅强化派生）：
  - data = 0x00 ‖ parent.key ‖ ser32(index + 0x80000000)（大端 u32）
  - I = HMAC-SHA512(key=parent.chainCode, data=data)
  - child.key = I[0:32]，child.chainCode = I[32:64]

## 3. Root 身份

- 派生路径：`m/44'/607'/0'/0'/0'`（逐层强化）
- keypair = ed25519 fromSeed(末级节点 key)（nacl 兼容）
- publicKeyHex = hex(publicKey)
- **rootId = sha256(publicKey) 的 hex**（64 字符小写）

## 4. 域身份（domain identity）

- h = sha256(utf8(domain))
- idxA = readUInt32BE(h, 0) & 0x7fffffff
- idxB = readUInt32BE(h, 4) & 0x7fffffff
- 路径 = root 路径后继续 `/{idxA}'/{idxB}'`（即完整路径 `m/44'/607'/0'/0'/0'/{idxA}'/{idxB}'`）
- 域 keypair 同 Root 方式从末级节点 key 生成；域签名用于 P2P 消息签名

## 5. 身份文件存储格式

文件：`{rootId}.json`，UTF-8 JSON。

### v2（当前版本）

```
kdf:    scrypt(password, salt, N=32768, r=8, p=1, keyLen=32, maxmem=64*1024*1024)
salt:   16 字节随机，hex 存储
cipher: aes-256-gcm，iv 12 字节随机
payload 明文 JSON: { mnemonic, derivationPath, version?: 2, wordlist?, nickname?, avatar?, gender?, region?, signature?, createdAt }
（字段名以真实落盘格式为准：`derivationPath`；version 字段可选，Rust 侧写入带 2、读取缺省兼容；
gender/region/signature 为资料扩展字段，Option 缺省不序列化，旧文件向后兼容）
存储字段:  { version:2, kdf:'scrypt', salt, iv, data, authTag, publicKeyHex, rootId, nickname?, avatar?, gender?, region?, signature?, createdAt, updatedAt }
```
密文布局（TS 实现）：data/authTag 均为 hex；GCM authTag 单独存储。

### v1 legacy（只读兼容，解锁后迁移到 v2）

```
kdf:    pbkdf2(password, salt, 210000, sha512, keyLen=32)
cipher: aes-256-cbc，iv 16 字节
```

### 二维码备份载荷

> **磁盘 `IdentityFile` 格式不变（字节级权威）**：本节的压缩只作用于**二维码备份
> 载荷的编码表示**，**磁盘 `{rootId}.json` 与完整文件备份 `backup_payload` 的
> `IdentityFile` 线形一律不改**（仍为 hex + `publicKeyHex` 必填）。二者由 `IdentityFile`
> Serialize 路径承担，与本节紧凑结构体完全解耦。已登录设备零迁移、零风险。
> 字节级定义以架构设计文档
> `qr-backup-payload-compression.md`（wiki architecture/identity/） §4.2 为准。

二维码备份载荷经**顶层封装 `v` 判别两种格式**（详见 §5.3 兼容矩阵）：

- **`v:1`（旧）**：`i` = 字段裁剪后的紧凑身份文件 JSON（`IdentityFile` v2 结构不变，
  hex + `publicKeyHex` 必填），`recoverFromBackup` 同一入口恢复，无需区分载荷类型。
  完整文件备份载荷含头像 data URL（实测可达数十 KB），远超 QR 码容量，故二维码路径
  单独产出紧凑载荷：
  - 裁剪规则：
    - payload 内与文件外层的 `avatar` 一律剔除（体积大头）；
    - payload 内的 `gender`/`region`/`signature` 一并剔除（可选扩展字段）；
    - 保留 `mnemonic`/`derivationPath`/`version`/`wordlist`/`nickname`/`createdAt`
      （身份恢复必需），文件外层保留 `nickname`；
  - 加密：同口令重新加密，新随机 salt/iv（不复用原文件密文）；
- **`v:2`（新）**：`i` = 独立紧凑结构体 `CompactBackupFile`（见 §5.4），**base64 编码、
  无 `publicKeyHex`**，恢复端解析后重建磁盘 `IdentityFile`。

两者共用约束：
- **尺寸预算：必须 <3KB（QR 物理上限），建议 <2KB**（无头像实测 <1KB）；
- 恢复后头像为 `null`：经个人设备同步（profile-sync）找回或由用户重新设置；
- 导出需验密（解密当前身份文件即验密），密码错误报 `Invalid password`；
- **数据密钥恢复（§13.7/§13.8）**：恢复设备在口令未分叉时自动重获数据密钥
  （on_unlock 自动自锚 + 自动 ack，无需操作）；口令已分叉时敏感数据暂缓同步
  （D′ ack 门控），输一次新口令 `verify_ticket` + `unify` 完成重封即恢复。

### v2 紧凑格式 `CompactBackupFile`（备份码专用）

`v:2` 载荷的 `i` 是一个**独立结构体**（非 `IdentityFile` 的变体/子集），自带独立
Serialize/Deserialize，与 `IdentityFile` 的 serde 属性完全解耦——改它不会动磁盘线形。
字段集（字节级，见架构文档 §4.2）：

```
struct CompactBackupFile {
  v: 2,                  // 内层格式版本（与顶层 v 对齐，独立字段便于单测）
  kdf: "scrypt",
  salt: "<b64>",         // 16B 随机 salt，base64 编码
  iv: "<b64>",           // 12B 随机 iv，base64 编码
  data: "<b64>",         // 密文（mnemonic+derivationPath 明文 JSON），base64 编码
  authTag: "<b64>",      // 16B GCM authTag，base64 编码
  rootId: "<hex>",       // 64 字符，保留作防篡改校验锚点（= sha256hex(publicKey)）
  nickname: "..."?,      // 保留（恢复后直接有昵称）
  createdAt: <ms>,       // 保留（profile-sync LWW 时间戳锚点）
  updatedAt: 0,          // 资料状态未知（profile-sync LWW：残缺快照永不赢）
}
```

- **编码规则**：`salt/iv/data/authTag` 四字段在 `CompactBackupFile` 的
  Serialize/Deserialize 中一律用 `base64::engine::general_purpose::STANDARD`。
  磁盘 `IdentityFile` 的 hex 编码**原样保留**，两套结构体各自维护编码，互不牵连。
- **删除**：`publicKeyHex`（64 hex）、`version`（内层格式版本由 `v` 承担）。
- **不携带 publicKey**（架构决策已砍掉可选 `pk`，见架构文档 §5）：恢复端从助记词
  派生公钥补全（`identity.public_key_hex()`）。理由：恢复主流程必先 KDF 解密才能拿
  助记词派生身份，`pk` 的「不解密先确认身份」钩子无消费点，属纯冗余，为瘦身砍掉；
  若未来需要可顶层 `v` 再 bump 演进加回。
- **`updatedAt` 恒为 0**：紧凑备份资料字段残缺，不能携带源文件资料时间戳，否则残缺
  快照会在 profile-sync LWW 裁决中挤掉对端完整资料；置 0 表示「资料状态未知」，
  恢复端收到的任何全量快照都严格更新、必然被应用。

**恢复端重建**：解析 `CompactBackupFile` → 解锁 payload 派生身份 → 校验
`identity.id() == rootId` → 补全 `publicKeyHex`（派生公钥 hex 化）、把 base64 字段回
hex、补 `version:2` → 落盘 `IdentityFile`（磁盘格式对外不变，旧版软件仍能读恢复出的文件）。

### 顶层封装 `v` 判别与兼容矩阵

- `v:1`（旧）：`i` = 磁盘格式 `IdentityFile` JSON（hex + `publicKeyHex` 必填），恢复端走
  `IdentityFile::from_json`。
- `v:2`（新）：`i` = `CompactBackupFile`（base64 + 无 `publicKeyHex`），恢复端走新解析函数。

判别用顶层 `v`，**不用字段前缀或值域启发**（hex vs base64 值域可能重叠，启发式脆弱）。

| 生成端 | 恢复端 | 行为 |
|---|---|---|
| 新（v2 码） | 新 | 顶层 `v:2` → `CompactBackupFile` 解析，正常恢复 |
| 新（v2 码） | 旧（只懂 v1） | 旧版 `recover_backup` 把 payload 当 IdentityFile 解析，因 `i` 缺 `publicKeyHex`（且 `publicKeyHex` 必填、无 `#[serde(default)]`）→ 反序列化失败 → 报「备份数据无效或已损坏」（fail-closed，不误恢复） |
| 旧（v1 码） | 新 | 顶层 `v:1` → 现有 `IdentityFile::from_json`，正常恢复（向后兼容无条件成立） |

> 无 P2P 路径生成的「纯 `IdentityFile` JSON」形态（顶层无 `v`）**保持原样不改造**，
> 是旧版兼容遗留形态，新恢复端按现有非 v1 兜底分支照常解析（本就是磁盘格式，
> 含 `publicKeyHex`）。

**`pwv` 注入链路不受影响**：`pwv`（口令校验器）是**顶层可选字段**（QR-F1/§13.8 注入），
`i` 内部格式从 `IdentityFile` 升级为 `CompactBackupFile` 不触及它。`v:1` 与 `v:2` 的
`backup_payload_qr` 顶层均按需携带 `pwv`，恢复端 `recover_backup` 的 pwv 注入守卫
（水位 + 未来 ts）不变。

## 6. 资料字段

- `nickname`：必填，trim 后 1–24 字符；注册与助记词恢复时录入
- `avatar`：可空，必须 `data:image/` 前缀，序列化后 ≤200KB
- 扩展字段（F1）：`gender` ≤16 字符、`region` ≤64 字符、`signature` ≤128 字符；均可空，
  值 trim 后落库，超长报错
- `updateProfile`：改昵称/头像/扩展字段；三态口径——缺省 = 不变、空串（trim 后为空）= 清除、
  其余 = 设置；avatar 传 null 清除
- `recoverFromBackup`：写入前必须 sanitize 外部资料字段（去非法值）

## 7. 验收向量

`spec/vectors/identity.json`（code 仓库）至少覆盖：

1. 固定 mnemonic（中文词表）→ rootId / publicKeyHex
2. 同一 mnemonic 派生两个不同 domain 的域公钥
3. 英文 mnemonic（v1）→ rootId（恢复兼容路径）
4. scrypt v2 加解密往返（固定 password+salt+iv → 固定密文）
5. pbkdf2 v1 解密（固定密文 → 明文 payload）
6. **备份码 v2 载荷**：固定 mnemonic + password + 固定 salt/iv → 固定
   `CompactBackupFile` JSON（顶层 `v:2`，base64 + 无 `publicKeyHex`），
   复用 scryptV2 的 `publicKeyBase64/saltBase64/ivBase64/ciphertextBase64/authTagBase64`
   作为 base64 线形基准
7. **新旧互读**：v1 码（hex + `publicKeyHex`）→ 新恢复端还原磁盘 `IdentityFile`
   字节级一致；v2 码 → 重建磁盘 `IdentityFile` 的 `publicKeyHex` 补全正确性
8. **编码往返**：`CompactBackupFile` base64 字段 ↔ `IdentityFile` hex 字段
   同源字节往返一致（对照 scryptV2 `ciphertextBase64`）
