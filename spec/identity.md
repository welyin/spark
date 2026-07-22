# 身份模块规格（identity）

> 来源：反向提取自 `desktop/src/main/identity/root-id.ts`。Rust 实现必须逐字节对齐本规格，
> 全部算法以 `vectors/identity.json` golden vectors 验收。

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
payload 明文 JSON: { mnemonic, derivationPath, version?: 2, wordlist?, nickname?, avatar?, createdAt }
（字段名以真实落盘格式为准：`derivationPath`；version 字段可选，Rust 侧写入带 2、读取缺省兼容）
存储字段:  { version:2, kdf:'scrypt', salt, iv, data, authTag, publicKeyHex, rootId, nickname?, avatar?, createdAt, updatedAt }
```
密文布局（TS 实现）：data/authTag 均为 hex；GCM authTag 单独存储。

### v1 legacy（只读兼容，解锁后迁移到 v2）

```
kdf:    pbkdf2(password, salt, 210000, sha512, keyLen=32)
cipher: aes-256-cbc，iv 16 字节
```

## 6. 资料字段

- `nickname`：必填，trim 后 1–24 字符；注册与助记词恢复时录入
- `avatar`：可空，必须 `data:image/` 前缀，序列化后 ≤200KB
- `updateProfile`：改昵称/头像；avatar 传 null 清除
- `recoverFromBackup`：写入前必须 sanitize 外部资料字段（去非法值）

## 7. 验收向量

`vectors/identity.json` 至少覆盖：

1. 固定 mnemonic（中文词表）→ rootId / publicKeyHex
2. 同一 mnemonic 派生两个不同 domain 的域公钥
3. 英文 mnemonic（v1）→ rootId（恢复兼容路径）
4. scrypt v2 加解密往返（固定 password+salt+iv → 固定密文）
5. pbkdf2 v1 解密（固定密文 → 明文 payload）
