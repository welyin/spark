# spark-threshold-vouch · 担保链门槛（示例）

C11 门槛插件参考实现（担保链）：被担保人发起请求，N 名已有参与者担保，
收齐后组装「是否满足门槛」的签名证明（ThresholdProof）。对齐
wiki/architecture/community-affairs.md §7.3：门槛插件产出产物，内核/事务
客户端只验证产物，不执行担保流程。

## 能力面与权限

- `docs`：请求/担保/证明三个 append-only 集合（storage:read/write）。
- `identity:sign`：担保与组装签名（高级权限）。
- `identity.verify`：证明产物的免权限验签（基础权限）。

## 诚实口径（务必先读）

本插件是**演示级**参考实现：**所有担保与组装签名的主体都是插件域身份**
（`plugin:spark-threshold-vouch` 域私钥）。平台 SDK 只有 `identity:sign`
（插件域签名面），插件拿不到用户个人身份签名能力——因此：

- 签名在密码学上**不证明担保人/组装人的个人身份**；`voucherRootId`、
  `assembledBy` 只是自报文本，恶意用户可以为任意「担保人」伪造担保且
  验签照样通过；
- 「免权限验签」验的是**插件域签名有效 + 载荷未被改动**（任何节点对同一
  证明得到同一结论），而不是「担保人本人签过」。

在平台层提供个人身份签名路径之前，本插件的 ThresholdProof 不得被当作
「N 个具体个人担保」的证据使用；届时应以个人身份签名路径重做担保签发。
