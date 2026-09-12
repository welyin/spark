# policy（策略引擎 B1：最小声明式规则集）

> 状态：**C5 纯逻辑已落地**（`core/src/policy/`：策略文档 schema、求值入口、静态分析）；**A15 名册开放声明已落地**（§8：`org:disclosure:` 记录 + 公示延迟 + `eval_disclosure` 求值）；**A17 准入策略声明已落地**（§9：`org:accept:` 记录 + 公示延迟 + 生效门控；免预录加入验证见 `org-join`（待 A53 改写，wiki protocol/org/） §8）。依据：`总体方案`（wiki architecture/community-affairs.md） §6 方案 B（决策点 2 = **B1** 自研最小集起步）；产品语义：`共同体模型`（wiki product/community-model.md） §九（名册三档、字段级控制、向上开放矩阵）；消费方：[read-gate](read-gate.md) §4 第 5 步（A15 起 = 开放声明求值）。
> 通用编码约定见 `community 总约`（wiki protocol/community/README.md）。
>
> ⚠️ 决策变更（2026-09-10）：组织与共同体已合并为统一概念（成员分个人/组织两类，加入类型由创世策略声明；副本按全副本设备计数）。本篇涉及"叶组织/共同体域"二分的规则以 [docs/product/community/model.md](../../product/community/model.md) 修订为准，全量改写见架构任务 A53。

## 1. 定位与升级路径

内核内置**最小声明式规则集**：域边界默认 + 名册可见性三档 + 逐字段掩码 + 向上开放矩阵，一台求值器共用（产品 §九「共用同一台策略引擎」）。策略插件只产出声明式文档，不得在读取执行点运行插件自有代码。

- **schema 预留 `engine` 字段**：当前唯一合法值 `"b1"`；出现真实超表达力需求时以 B2（Cedar）升级——策略文档形态不变、求值器按 `engine` 分派，非 `"b1"` 引擎的文档在 B1 求值器上 **fail-closed 拒绝**（`unsupported-engine`）；
- 纯逻辑：不碰网络与存储，不读本地时钟；求值输入（策略文档、凭证呈现摘要、请求上下文）全部参数注入；
- **与 `org-genesis`（待 A53 改写，wiki protocol/community/） §5 组织签名策略文档的区别**：那是 `signingPolicy` 修订链（`policyHash`，组织签名验证用，归 C3）；本文是**读授权/可见性策略文档**（经 `readPolicy.policyRef` 引用，归 read-gate）。两套文档、两个哈希槽位，互不替代。

## 2. 策略文档线形

```json
{
  "policyV": 1,
  "engine": "b1",
  "orgId": "org_<…>",
  "roster": {
    "tier": "org-only" | "representatives" | "public",
    "fields": [ { "field": "phone", "audience": "org-members" } ]
  },
  "upward": [ { "collection": "finance:monthly@v1", "to": "org_<上级域>" } ],
  "updatedAt": 1720000000000,
  "sigSet": null
}
```

| 字段 | 约束 |
| --- | --- |
| `policyV` | 恒 1 |
| `engine` | 恒 `"b1"`（升级路径出口，见 §1） |
| `orgId` | 策略主体组织（数据属主；orgId 双形态，org-genesis §2） |
| `roster.tier` | 名册可见性三档（产品 §九第一层）：`org-only` 仅组织（跨组织只见组织条目、无人员行——⚠️ "无人员行"的二分前提随 2026-09-10 决策失效，档位语义待 A53 改写）/ `representatives` 代表可见 / `public` 名册公开 |
| `roster.fields[]` | 字段级掩码（产品 §九：名册公开不等于手机号公开）：逐字段独立受众；**未声明字段缺省不对外暴露**（fail-closed） |
| `upward[]` | 向上开放矩阵（产品 §三：逐「数据集合 × 上级域」配置）：`collection` 开放的数据集合名，`to` 上级共同体域 orgId——持 `to` 域已验证成员资格凭证者可读该集合；**无条目 = 不向任何上级开放**（fail-closed 缺省） |
| `updatedAt` | 更新时刻（Unix 毫秒，展示/LWW 用，无求值语义） |
| `sigSet` | 可省（缺省键省略）：组织签名包（org-signature §2），保存合入校验归 C3；**求值入口不验签**——文档按哈希自认证（同 credId 先例），`policyRef` 复算即完整性校验 |

```
policyDocHash = sha256hex(normalizeObject(策略文档剔除 sigSet))   // 64 hex 小写
```

`readPolicy.policyRef`（read-gate §2）的值 = 文档的 `policyDocHash`：声明只存哈希槽位，文档本体经声明/同步面送达后由消费方复算比对。

## 3. 受众模型与请求者上下文

受众阶梯（rank 从小到大，静态分析比较用）：

| 受众 | 成员资格（请求者 ∈ 受众 ⇔） | rank |
| --- | --- | --- |
| `org-members` | 是数据属主组织成员 | 0 |
| `representatives` | 是属主组织成员，**或**是属主（共同体）某成员组织的代表 | 1 |
| `public` | 恒真（含无凭证的外部节点） | 2 |

> B1 最小集不设独立的「域成员」受众：共同体成员资格以「呈现凭证 `subjectDomain` 匹配」的形态只出现在向上开放矩阵判定中（§4 第 5 条）。

请求者上下文（求值输入，由数据账号侧在 read-gate §4 第 1–4 步全部通过后装配）：

- `isOrgMember` / `isRepresentative`：请求者与数据属主组织的关系（来自名册视图；关系判定本身不在本模块）；
- `credentials`：已通过验证链的呈现凭证摘要 `[{credType, subjectDomain}]`（credential §6 全链 + holderProof 之后的可信摘要）。

请求分类 `ReadRequest`：`roster-row`（带 `rowIsRepresentative` 行标记）/ `roster-field`（带字段名）/ `collection`（带集合名）。

## 4. 求值语义（`evaluate_read`，fail-closed）

1. **引用绑定**：`policyRef` 复算 ≠ `policyDocHash` → 拒绝（`policy-ref-mismatch`）——文档任何字段被篡改即失配；完整性校验先于引擎分派（篡改文档的 `engine` 字段不可信）；
2. **引擎**：`engine != "b1"` → 拒绝（`unsupported-engine`）；
3. **名册行**（`roster-row`）：`isOrgMember` → 放行（域边界基线：本组织成员总见本名册）；`tier = org-only` → 拒绝（`row-hidden`）；`tier = representatives` → 仅 `rowIsRepresentative` 行放行；`tier = public` → 放行；
4. **名册字段**（`roster-field`）：`isOrgMember` → 放行（基线）；未声明字段 → 拒绝（`field-hidden`）；已声明字段按受众：请求者 ∈ audience → 放行，否则拒绝（`field-hidden`）。字段判定**不回看档位**（档位管行可见性，字段规则管字段，两条正交；行不可见时字段判定无实际意义，矛盾组合已在保存期被 §5 `field-rule-shadowed` 挡下）；
5. **数据集合**（`collection`）：`upward` 存在 `(collection, to)` 条目**且**请求者呈现凭证中有 `subjectDomain == to` 的已验证凭证 → 放行；否则拒绝（`not-covered`）。

拒绝统一由 read-gate 落 `denied: true` 空集应答（read-gate §4）。

## 5. 静态分析（保存策略时，`analyze(doc, prev)`，求值器之外的纯函数）

严重级：**Error 阻断保存**（规则冲突/不可达）；**Warning 需显式确认**（暴露面较上一版扩大）。`prev` = 上一版策略文档（同 `orgId` 链，可选——首版传 `None`）。

| 级别 | code | 触发 |
| --- | --- | --- |
| Error | `invalid-structure` / `unsupported-engine` | 结构校验失败（policyV/orgId 形状/字段名/集合名/`to` 形状） |
| Error | `duplicate-field-rule` | 同名字段规则重复声明 |
| Error | `duplicate-upward-entry` | 同一 `(collection, to)` 重复声明 |
| Error | `field-rule-redundant` | 字段受众 `org-members`——域边界基线已保证成员可见，规则无效 |
| Error | `field-rule-shadowed` | `tier = org-only` 时一切字段规则不可达（行对外全隐） |
| Warning | `roster-tier-raised` | 档位较 `prev` 提高（rank 比较，§3 阶梯序） |
| Warning | `field-audience-widened` | 同名字段受众较 `prev` 放宽 |
| Warning | `field-exposed` | 新增字段规则且受众非 `org-members`（上一版该字段缺省不对外） |
| Warning | `new-upward-entry` | 新增向上开放条目（新「集合 × 上级域」暴露） |

收窄（档位降低、条目/字段移除）不告警。code 逐字稳定（golden vectors 与跨层上报按此对齐）。

## 6. 与 read-gate 的对接（A15 求值口径一次性切换，membership §五.2）

**A15 起 read-gate §4 第 5 步 = 开放声明求值（§8），不再是本节 B1 文档的读取点求值。** 过渡口径：`readPolicy.policyRef` 线形槽位不变（存在与否充当「本集合受开放声明约束」开关）；旧「名册三档 + 字段掩码」读取点判定映射为「仅组织」默认档（未声明即全隐，fail-closed）；存量组织默认档位 = 仅组织，首个开放声明须经公示延迟（§8）。

本节 `evaluate_read` 求值器与 `readGate.policyRef` 向量组保留（求值器重排归 A30；B1 文档求值仍服务保存期静态分析等场景），但 orgq 读取点已切换为 §8 求值。

## 7. 验收向量（登记：`code/spec/vectors/community.json`)

| case 组 | 内容 | 状态 |
| --- | --- | --- |
| `readGate.policyRef` | `policyRef` 求值矩阵：名册三档 × 请求者关系、字段掩码、向上开放矩阵（命中/未覆盖/域不符）、`engine` 不匹配、`policyRef` 篡改；静态分析冲突/扩大告警码 | 已产出（C5 `core/examples/gen_policy_vectors.rs` 自产回填；消费测试 `core/tests/community_policy_vectors.rs`） |
| `policy.disclosure` | 开放声明线形（canonical → `disclosureHash` 固定值，sigSet 剔除自认证）；暴露面扩大静态分析（`disclosure_widening`）真值表；`eval_disclosure` 求值（生效门控 / 最高 version 胜 / 默认仅组织档） | 已产出（A15 `core/examples/gen_disclosure_vectors.rs` 自产回填；消费测试 `core/tests/policy_disclosure_vectors.rs`） |
| `policy.acceptCredentials` | 准入策略声明线形（canonical → `acceptPolicyHash` 固定值，sigSet 剔除自认证）；准入面扩大判定（`accept_policy_widening`）真值表；生效门控（公示延迟窗口内不采信） | 已产出（A17 `core/examples/gen_accept_vectors.rs` 自产回填；消费测试 `core/tests/accept_vectors.rs`） |
| `joinRequest.merge` | 免预录合入双路径验证（`adjudicate_join_request`）：认领受理 / 有效凭证受理 / 已注销 / 签发者不受信任 / 类型不匹配 / 策略缺失 / 未附凭证各必败 | 已产出（A17 同生成器；消费测试 `core/tests/accept_vectors.rs`） |

## 8. 名册开放声明（A15：`org:disclosure:` 记录）

下级组织对自己名册与数据集合向上级域的**发布物**（组织级动作，OrgSigSet 背书）。名册开放（tier/fields）与数据集合开放（collections）正交——名册开放不蕴含数据开放；B1 `upward` 矩阵按 targetDomain 并入本记录（`to` 由键隐含）。

```
键：org:disclosure:{orgId}:{targetDomain}        （org:structure@v1 键域，随 orgsync 全员流动）
```

```json
{
  "disclosureV": 1,
  "orgId": "org_<下级组织>",
  "targetDomain": "org_<上级域>",
  "tier": "org-only" | "representatives" | "public",
  "fields": [ { "field": "phone", "audience": "representatives" } ],
  "collections": [ "finance:monthly@v1" ],
  "version": 1,
  "updatedAt": 1720000000000,
  "effectiveAt": 1720086400000,
  "sigSet": { "…": "OrgSigSet（org-signature §2），subject = disclosureHash" }
}
```

| 字段 | 约束 |
| --- | --- |
| `disclosureV` | 恒 1 |
| `orgId` / `targetDomain` | orgId 双形态；不得相同（self-disclosure 拒绝） |
| `tier` / `fields[]` | 复用 §2 档位与 FieldRule 线形；未声明字段不对外（fail-closed） |
| `collections[]` | 向上开放的数据集合全名 `name@v{version}`；空 = 不开放任何数据集合 |
| `version` | 声明代际，单调递增（≥1），LWW 裁决键（同 version 保留本地现状） |
| `updatedAt` / `effectiveAt` | 发布时刻 / 生效时刻（`effectiveAt ≥ updatedAt`） |
| `sigSet` | 发布件必携；`subject` 必须绑定 `disclosureHash`（防搬签） |

```
disclosureHash = sha256hex(normalizeObject(声明记录剔除 sigSet))   // 64 hex 小写
```

**公示延迟**（governance §4.1「开放面扩大必须公示」同原则，membership §4.3 裁定）：暴露面扩大方向 `effectiveAt = updatedAt + 24h`（发布即公示——键域随 orgsync 全员流动——生效由 `effectiveAt` 门控）；收窄或持平即时生效（`effectiveAt = updatedAt`）。扩大判定（`disclosure_widening(prev, next)`）：档位 rank 升 ‖ 新增字段授权 ‖ 字段受众 rank 升 ‖ 新增开放集合；`prev` 缺席（首次声明）时任何非「仅组织 + 空授权 + 空集合」形态皆为扩大。发布面另要求**扩大必须显式确认**（interpretation §4.2 静态分析同族语义，未确认如实报错）。

**求值（`eval_disclosure`，写时/查询装配时的确定性纯函数）**：取 `targetDomain` 匹配且 `effectiveAt ≤ now` 的最高 `version` 记录投影为视图 `{tier, fields, collections}`；无生效声明 → 仅组织默认档（存量组织零声明即全隐，fail-closed 最保守）。求值不验签——记录按 `disclosureHash` 自认证，OrgSigSet 五步链是发布件进入同步键域的唯一闸门（入站合入 `adjudicate_incoming_disclosure`：结构校验 → sigSet 存在且 subject 绑定 → 五步链 → version LWW，与 trustDecl/policyDoc 同一模式）。

## 9. 准入策略声明（A17：`org:accept:` 记录）

组织声明「接受哪些凭证可免预录入册」的**发布物**（membership §4.5 免预录凭证入册；组织级动作，OrgSigSet 背书；governance §4.1 公示延迟禁令适用范围之一——准入面扩大方向公示 + 延迟生效、收窄即时）。

```
键：org:accept:{orgId}        （org:structure@v1 键域，随 orgsync 全员流动）
```

```json
{
  "acceptV": 1,
  "orgId": "org_<本组织>",
  "acceptCredentials": [ { "credType": "household-owner", "issuerTrust": "org_<信任声明域>" } ],
  "version": 1,
  "updatedAt": 1720000000000,
  "effectiveAt": 1720086400000,
  "sigSet": { "…": "OrgSigSet（org-signature §2），subject = acceptPolicyHash" }
}
```

| 字段 | 约束 |
| --- | --- |
| `acceptV` | 恒 1 |
| `orgId` | 声明主体组织（orgId 双形态；与键分量一致） |
| `acceptCredentials[]` | 准入规则：接受 `credType` 类型（credential §2 形状）、且签发者信任按 `issuerTrust` 域（orgId 双形态）的 `org:verifiers:` 信任声明（credential §4）判定的凭证；`(credType, issuerTrust)` 对不得重复；空数组 = 不接受任何免预录入册（缺省最保守形态） |
| `version` | 声明代际，单调递增（≥1），LWW 裁决键（同 version 保留本地现状） |
| `updatedAt` / `effectiveAt` | 发布时刻 / 生效时刻（`effectiveAt ≥ updatedAt`） |
| `sigSet` | 发布件必携；`subject` 必须绑定 `acceptPolicyHash`（防搬签） |

```
acceptPolicyHash = sha256hex(normalizeObject(声明记录剔除 sigSet))   // 64 hex 小写
```

**公示延迟**（governance §4.1，与 §8 开放声明同族裁定）：准入面扩大方向（新增 `(credType, issuerTrust)` 规则，含首份非空声明）`effectiveAt = updatedAt + 24h`（发布即公示——键域随 orgsync 全员流动——生效由 `effectiveAt` 门控）；收窄（移除规则）或持平即时生效。扩大判定（`accept_policy_widening(prev, next)`）：`next` 存在 `prev` 没有的规则对即为扩大；`prev` 缺席（首次声明）时任何非空 `acceptCredentials` 皆为扩大。发布面另要求**扩大必须显式确认**（未确认如实报错，与 disclosure 发布同口径）。

**消费**：免预录加入验证（`org-join`（待 A53 改写，wiki protocol/org/） §8）只采信 `effectiveAt ≤ now` 的现行记录；无记录或未生效 = 不接受免预录（fail-closed，预录-认领路径不受影响）。入站合入 `adjudicate_incoming_accept_policy`：结构校验 → sigSet 存在且 subject 绑定 → 五步链 → version LWW（与 disclosure 同一模式）。
