# spark-verify-hoa · 业主资格验证（示例）

C11 验证插件参考实现（业主场景）：申请人材料提交引导 + 验证人审核签发 +
凭证出示/验证演示。对齐 wiki/product/community-model.md §十（验证即插件）。

## 能力面与权限

- `sdk.credentials`（已落地只读面，`credentials:read` 高级权限）：
  `listHeld`（本机持有的协议凭证）/ `presentHolderProof`（对持有凭证出示
  holderProof，域身份由桥注入）/ `queryVerifiers`（组织验证人信任声明）/
  `verify`（内核验证链 §6 第 1–5 步结构化裁决，仅适用协议线形）/
  `queryRevocations`（按签发人身份查本地注销快照，缺失如实报
  `available:false`）。**无签发接口**——内核凭证体系刻意不对插件开放签发。
- `identity:sign`：签发/注销签名（见下「诚实口径」）。
- `docs`：申请/演示凭证/注销记录（append-only）+ 材料原文（`__sync:false`
  本地留存，证据最小披露）。

## 诚实口径（务必先读）

本插件是**演示级**参考实现，两处口径必须如实理解：

1. **签名主体是插件域身份，不是验证人个人身份。** 平台 SDK 只有
   `identity:sign`（以 `plugin:spark-verify-hoa` 域私钥签名），插件拿不到
   用户个人身份签名能力。因此任何「验证人签发的凭证」，其签名公钥都是同一把
   插件域钥匙，密码学上不证明验证人个人身份；`issuerRootId` / `revokedBy`
   只是自报文本。信任声明绑定、按验证人的注销链、既往不咎时间线在此模型下
   均不成立——需平台层提供个人身份签名路径后方可重做。
2. **HoaCredential 不是协议凭证。** 其线形（credentialId/subjectRootId/
   issuerRootId/unitNo/自定义拼接签名载荷）与 credential §2 协议线形不同：
   不进 `cred:held:` 键域、过不了内核验证链（结构→credId 复算→验签→信任
   匹配→注销检查），与内核凭证体系**零互操作**。视图中的「演示自查」只是
   本插件集合内的重算载荷 + 验插件域签名 + 查本地注销表，不核对签发人
   信任链，也不等于内核验证。

真正的内核凭证互操作面是「内核凭证面」分区里的五个只读方法（含 `verify`
内核验证链与 `queryRevocations` 注销快照查询）；本插件签发的演示凭证不会
出现在 `listHeld` 结果中，也过不了 `verify`——演示凭证的「演示自查」只是
本插件集合内的本地核对，不与内核验证链混用。
