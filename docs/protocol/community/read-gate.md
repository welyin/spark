# read-gate（读授权门禁）

> 状态：**已落地**——§3–§4 验证纯逻辑在 `core/src/credential/read_gate.rs`；readPolicy 声明扩展（`core/src/plugindata/mod.rs` `CollectionDeclaration.readPolicy`）与 orgq 集成（`core/src/kernel/inbound_dm/orgq.rs`：查询面按 kind 分流，credential 门禁 = `verify_read_auth` 含 A15 城门名册回查 + policyRef 存在时按开放声明求值）已接线；注销证明暂存 `cred:rev:{issuer}` 本地键域（分发承载面仍随 C10 定）。**A15 求值口径一次性切换**（membership §五.2）：§4 验证链追加城门名册回查（第 4 步前），第 5 步由 B1 文档求值切换为开放声明求值（[policy](policy.md) §8）——readAuth 段线形不变、旧「名册三档 + 字段掩码」判定映射为「仅组织」默认档。依据：`总体方案`（wiki architecture/community-affairs.md） §7.1「读授权凭证门禁」、§6 方案 B1；产品语义：`共同体模型`（wiki product/community-model.md） 第七/八节（托管域 × 公开发布、读授权、零密钥轮换）。
> 通用编码约定见 `community 总约`（wiki protocol/community/README.md）。凭证线形与验证链见 [credential](credential.md)。
>
> ⚠️ 决策变更（2026-09-10）：组织与共同体已合并为统一概念（成员分个人/组织两类，加入类型由创世策略声明；副本按全副本设备计数）。本篇涉及"叶组织/共同体域"二分的规则以 [docs/product/community/model.md](../../product/community/model.md) 修订为准，全量改写见架构任务 A53。

## 1. 定位

共同体托管集合的读取凭**成员资格凭证**放行（filtered 轴的共同体扩展：orgq 查询面的钩子换为凭证校验）。内容保护 = 读授权：成员退出即凭证注销、立即生效、**零密钥轮换**（离队者已下载的副本仍可读——本来是其合法获得；产品第八节）。平台不做集合级加密；保密边界 = 域边界。

与策略引擎 B1 最小规则集（C5）的对齐：本规格只消费 B1 求值结果的**引用形态**（策略文档哈希），不内嵌规则语法——规则集本身归 C5 规格。

## 2. 集合声明扩展：readPolicy（org:coll 记录追加可选字段）

```json
{ "readPolicy": { "kind": "credential", "credTypes": ["household-owner", "resident"],
                  "verifierDomain": "org_<…>", "policyRef": "<64hex>" } }
```

| kind | 语义 |
| --- | --- |
| `members` | **缺省（现状）**：组织成员可读（orgq 既有资格检查） |
| `public` | 公开发布：任何节点无需凭证可拉取（进入公共目录；与托管域不互斥，产品第七节） |
| `credential` | 持凭证放行：`credTypes` 任一 + issuer ∈ `verifierDomain` 的现行信任声明（[credential](credential.md) §4）+ 城门名册回查在册（§4 第 4 步）；`policyRef` 可省——A15 起槽位语义 = 「本集合受开放声明约束」开关（存在时按 [policy](policy.md) §8 求值），缺省即「凭证类型匹配 + 在册即可读全集合」 |

- 变更注记：readPolicy 是 `org-orgsync`（待 A53 改写，wiki protocol/org/） §20.2.1 声明记录的**可选追加字段**——代际内不可变纪律不变（声明冻结；改 readPolicy = 同名新 version 声明）；旧端解析忽略未知键无损（nodeInfo 先例）；
- 与 `confidentiality` 轴正交（encrypted 轴退役归 C7，本规格不依赖它）。

## 3. orgq-req 扩展：凭证呈现段（readAuth）

`orgq-req`（`org-orgsync`（待 A53 改写，wiki protocol/org/） §20.5）body 增加可选段：

```json
{
  "readAuth": {
    "gateV": 1,
    "credentials": [ { "…": "凭证全文 [credential](credential.md) §2" } ],
    "holderProofs": [ { "credId": "<64hex>", "sig": "<b64 64B>" } ],
    "presentedAt": 1720000000000
  }
}
```

- **holderProof**：逐凭证证明请求者持有 holder 私钥——签名密钥 = 凭证 `holder.publicKey` 对应私钥，载荷 = `canonical({ "credId":…, "requestId":…, "orgId":…, "collection":…, "presentedAt":… })`（绑定本次请求，防重放转投）；
- readAuth 段随 orgq-req body 入 **dm 信封既有签名面**（信封由 from root 身份签，[p2p-dm](../p2p/p2p-dm.md)），不再独立签名；`presentedAt` ±10 min 新鲜度门槛同总约；
- 持有证明与信封身份解耦是有意的：住户以其**在共同体的域身份**持证读取，不暴露 rootId 与户号的关联（资格可验、行为匿名，产品第九节）。

## 4. 数据账号侧验证链（内核，fail-closed）

对 `readPolicy.kind == 'credential'` 的集合，orgq-req 处理在既有资格检查之后追加：

1. readAuth 结构 + presentedAt 新鲜度；
2. 逐凭证走 [credential](credential.md) §6 验证链（结构 → credId → 验签 → 信任匹配 → 注销检查）；
3. credType ∈ readPolicy.credTypes 且 subjectDomain 匹配 verifierDomain 口径；
4. **城门名册回查**（A15，membership §4.3）：持有者**当时确为**凭证 subjectDomain 成员——读该域名册按 `holder.identity` 回查（双键兼容：rootId 或 org_user_id 命中任一即在册，A16 双写过渡口径）；退队即拒（**零密钥轮换**——名册回查时刻语义，无轮换窗），名册数据不可用同样 fail-closed；
5. holderProof 逐凭证验签通过（载荷绑定 requestId）；
6. policyRef 存在时（线形槽位不变，充当「本集合受开放声明约束」开关）：按**开放声明**求值（A15 / [policy](policy.md) §8）——属主组织对凭证 subjectDomain 的**已生效** disclosure 记录 `collections` 覆盖本集合才放行；声明缺失/未生效（公示延迟窗口内）/未覆盖一律拒绝（未声明即「仅组织」默认档，存量组织默认全隐）。旧 B1 文档读取点求值已随口径一次性切换下线（membership §五.2，切换点以版本发布对齐）。

任一失败 → `denied: true` 空集应答（非授权者连元数据都不给，orgq §20.5 既有口径）；插件未运行类降级语义同 filtered fail-closed。**同步面不验签**分工不变：以上全部在 orgq 消费点执行，复制面照既有规则流动。

## 5. 与既有轴的关系

| 场景 | 路径 |
| --- | --- |
| 组织内部集合（现状） | `members` 缺省，零变化 |
| 共同体托管集合（业委会账本托管小区域） | `credential`：住户持成员资格凭证读，无需加入来源组织；写权限仍属来源组织（orgq 写入路径不变） |
| 全网公开集合 | `public`：无需凭证；发现面归 [affair-metadata](affair-metadata.md) 公共目录 |
| 名册三档 / 字段级掩码 / 向上开放矩阵 | A15 起 = 名册开放声明（`org:disclosure:`，[policy](policy.md) §8：档位 + 字段授权 + 开放集合并入同一发布物）；B1 策略文档与 `evaluate_read` 保留（求值器重排归 A30），orgq 读取点不再消费 |

## 6. 验收向量（登记：`code/spec/vectors/community.json`）

| case 组 | 内容 | 状态 |
| --- | --- | --- |
| `readGate.envelope` | 固定 orgq-req + readAuth → holderProof 载荷逐字节 + sig 固定值 | 已产出（生成器自产） |
| `readGate.verifyChain` | 验证链通过/逐环失败用例（凭证无效/类型不符/已注销/proof 不符/呈现过期 → denied）+ A15 城门真值表（持有者在册/退队/名册不可用） | 已产出（C2 生成器自产；A15 回填名册回查 case） |
| `readGate.declExt` | readPolicy 追加字段的声明记录 canonical 往返（旧字段一字节不变回归） | 已产出 |
| `policy.disclosure` | 开放声明线形 + 暴露面扩大静态分析真值表 + `eval_disclosure` 求值（登记与说明见 [policy](policy.md) §7/§8） | 已产出（A15） |
