# org-invite
> 来源：organization/invite.ts。
> 网络层见 p2p，rootId 见 identity。

## 1. 身份锚点

- `rootId = sha256hex(原始 32 字节 Ed25519 根公钥)`（64 字符小写 hex，identity/root-id.ts:292）
- 根公钥在协议载荷中的编码：**base64（原始 32 字节，非 PEM）**
- 组织内所有 rootId 字段统一 `trim().toLowerCase()` 并须匹配 `^[0-9a-f]{64}$`


## 2. 邀请码（organization/invite.ts）

### 2.1 payload 字段（invite.ts:9-20）

```json
{
  "type": "spark-org-invite",
  "version": 1,
  "orgId": "org_<16hex>",
  "orgName": "<组织名，可空串>",
  "inviter": { "rootId": "<64hex 小写>", "peerId": "<可省>", "addresses": ["<multiaddr>", ...] },
  "createdAt": 1720000000000
}
```

### 2.2 编码（invite.ts:29-41）

- `base64url(JSON.stringify(payload) 的 UTF-8)`：`+`→`-`、`/`→`_`、**去掉 `=` padding**；紧凑 JSON 无空格
- 解码时 `-`→`+`、`_`→`/`，按 `(4 - len%4) % 4` 补 `=` 后 base64 解码
- **不签名、不含密钥**——邀请码不是 capability，仅作引导线索（invite.ts:1-7）

### 2.3 解析校验（invite.ts:44-89）

按序校验，任一不符抛出中文错误：

1. base64url 可解码且为合法 JSON
2. `type === 'spark-org-invite' && version === 1`
3. `orgId` 为非空字符串（trim 后使用）
4. `inviter.rootId` 匹配 `^[0-9a-f]{64}$`（先 trim 再 lowercase 后校验）
5. `inviter.addresses` 过滤非字符串/空串；`peerId` 非空才保留；**peerId 与 addresses 至少其一**
6. 有效期：`createdAt` 为 number 且 `createdAt > 0` 且 `Date.now() - createdAt ≤ 24h`
   （`ORG_INVITE_MAX_AGE_MS = 24*60*60*1000`，invite.ts:27）。
   ⚠️ 只查"过去 24h"，**未来的 createdAt 不设上限**（无 `Math.abs`）
- 归一化返回：`orgName` 缺省为 `''`，rootId 小写，addresses 过滤后数组
