/**
 * 问题反馈应用（spark-feedback）· 协议线形构造（feedback.md v0.2 §3.1 / affair.md §10）。
 *
 * 分层（与 spark-git-repo/wire.ts 同口径）：
 * - 通用协议线形（canonical/哈希/身份推导/创世草稿构造）上收
 *   packages/plugin-sdk/src/affair-wire.ts，经本文件 re-export；
 *   子事务创建走 sdk.affairs.create（SDK 承载创世构造 + 签名 + follow）；
 * - 本文件只承载插件语义层：反馈子事务创世输入构造（type=bug|proposal +
 *   refs parent → 目标项目议题，载荷入 extra.feedback）与手动兜底导出草稿；
 * - 签名主体诚实口径（同 spark-affairs / spark-git-repo）：actor 为本插件
 *   域身份（kind:person 线形），不代表操作者个人身份。
 *
 * 本文件不依赖 SDK 运行时/Vue（affair-wire 同为纯函数模块），全部可单测。
 */

import type { AffairGenesisInput } from '../../packages/plugin-sdk/src/affair-wire';
import { affairTypeOf, type FeedbackInput, type FeedbackPayload } from './model';

// 通用线形助手 re-export（唯一实现在 SDK affair-wire，与内核 canonical.rs 同向量）
export {
  base64Decode,
  buildGenesisDraft,
  deriveIdentity,
  normalizeObject,
  sha256Hex,
  sha256HexBytes,
  signPayload,
  validateAffairRefs
} from '../../packages/plugin-sdk/src/affair-wire';
export type { AffairActor, AffairGenesisInput, AffairRef } from '../../packages/plugin-sdk/src/affair-wire';

/** 反馈子事务 → 目标项目议题的 parent 引用（affair.md §10 四种引用之父子） */
export function buildParentRef(targetAffairId: string): { target: string; rel: 'parent' } {
  return { target: targetAffairId, rel: 'parent' };
}

/**
 * 反馈子事务的默认规则文档（与 spark-git-repo 子事务同模板）：
 * 反馈的受理/采纳/关闭全部归目标项目议题的规则文档管（§1 边界），
 * 子事务自身规则只是容器合法性的最小声明——delayed-veto 单票否决兜底。
 */
export function defaultFeedbackRules(): Record<string, unknown> {
  return {
    engine: 'b1',
    closeConditions: [],
    pubPeriod: { delayMs: 86_400_000, vetoThreshold: { count: 1 } },
    ruleChange: { kind: 'delayed-veto', delayMs: 86_400_000, vetoThreshold: { count: 1 } },
    exec: null
  };
}

/**
 * 反馈子事务创世输入（档一-1 MVP 主路径）：
 * type = 'bug' | 'proposal'（对齐「项目」插件 affairTypes，本插件只生产不注册）；
 * refs = [parent → 目标项目议题]；title 单一承载于创世顶层字段——设计稿 §3.1
 * 载荷清单中的 title 与创世 title 同义，不重复注入 extra.feedback（canonical
 * 承诺最小化）；body/environment/attachments/feedbackChannel 经 extra.feedback
 * 显式声明并入创世承诺。
 */
export function buildFeedbackGenesisInput(input: {
  type: FeedbackInput['type'];
  title: string;
  targetAffairId: string;
  payload: FeedbackPayload;
}): AffairGenesisInput {
  const title = input.title.trim();
  return {
    type: affairTypeOf(input.type),
    title,
    // summary 取自正文摘要（先 trim 去首尾空白再截断），空正文回退标题
    summary: input.payload.body.trim().slice(0, 200) || title,
    tags: ['feedback', input.type],
    refs: [buildParentRef(input.targetAffairId)],
    rules: defaultFeedbackRules(),
    extra: { feedback: input.payload }
  };
}

// ------------------------------------------------------------------
// 手动兜底导出（档一-1 兜底路径；对齐 project.md §8⑤ 口径）
// ------------------------------------------------------------------

/**
 * 预填导出草稿（未安装/未就绪时的兜底）：生成含 type/title/summary/refs/rules/
 * extra 的创世输入 JSON，引导用户到「项目」插件内确认提交。
 * 导出即复制出本插件——内容一经他处提交，公开性归目标议题规则管，
 * 视图层须与提交路径同等明示不可撤回。
 */
export function buildExportDraft(genesisInput: AffairGenesisInput): string {
  return JSON.stringify(
    {
      $schema: 'spark-feedback/export-draft@1',
      hint: '在「项目」插件中打开目标议题，选择「新建子事务」，将本 JSON 粘贴进导入框确认提交。',
      genesisInput
    },
    null,
    2
  );
}
