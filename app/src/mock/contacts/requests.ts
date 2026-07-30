/**
 * 好友申请模拟：发出/重试/回复我发出的申请，以及非 Tauri 环境下模拟对方反应
 * （4 秒后置 replied/accepted/failed/ignored）。依赖 store（spaces/contactsOf）。
 */
import type { FriendRequest } from './types';
import { emptyProfile } from './types';
import { contactsApi, contactsOf, demoContacts, spaces } from './store';

/**
 * 发出添加请求（添加朋友三入口 / 组织成员转个人联系人 §9.3）。
 * 本地先写 outbox（同步语义），Tauri 下由内核投递：sendRequest 命令立即返回
 * pending，投递终态经 FriendRequestSent 事件异步按 id 回写（送达等确认 pending /
 * 投递失败 failed）；.catch 只在命令立即报错（已是朋友/寻址失败）时触发，届时置
 * failed 可重试。两侧都按 id 作用于同一条记录，catch 有 pending 守卫，若事件已先
 * 回写终态则 catch 空转，以事件为准收敛。
 * 等待对方确认后由 FriendRequestAccepted 事件落成朋友。
 */
export function sendFriendRequest(
  spaceKey: string,
  input: { rootId: string; raw: string; peerId?: string; addresses?: string[]; source: string; message: string }
): void {
  const space = contactsOf(spaceKey);
  const id = `out-${Date.now()}-${space.outgoing.length}`;
  const record: FriendRequest = {
    id,
    rootId: input.rootId || input.raw,
    nickname: input.rootId ? `${input.rootId.slice(0, 8)}...` : input.raw.slice(0, 24),
    message: input.message,
    source: input.source,
    status: 'pending',
    // 新发出的申请置未读：「新的朋友」入口角标即时提示（查看详情后 markRequestRead 清除）
    unread: true,
    createdAt: Date.now(),
    updatedAt: Date.now()
  };
  space.outgoing.push(record);
  const api = demoContacts() ? undefined : contactsApi();
  if (api) {
    api
      .sendRequest({
        id,
        rootId: input.rootId,
        raw: input.raw,
        // 名片来源的申请：前端已解析出 peerId/addresses，上行给内核直接寻址
        // （内核只认签名节点名片，解析不了 spark-card JSON / 名片内容文本）
        peerId: input.peerId,
        addresses: input.addresses,
        source: input.source,
        message: input.message
      })
      .catch(() => {
        // 投递失败：对方可能离线，记录保留 + 未读提醒，允许重试
        if (record.status === 'pending') {
          record.status = 'failed';
          record.updatedAt = Date.now();
          record.unread = true;
        }
      });
  } else {
    simulatePeerReaction(spaceKey, id);
  }
}

/** 重试投递失败的发出申请：重置为 pending 重新投递；终态同样由 FriendRequestSent
 *  事件回写，.catch 仅覆盖命令立即报错（守卫 pending，与事件按 id 收敛） */
export function retryOutgoing(spaceKey: string, requestId: string): void {
  const request = contactsOf(spaceKey).outgoing.find((item) => item.id === requestId);
  if (!request || request.status !== 'failed') {
    return;
  }
  request.status = 'pending';
  request.updatedAt = Date.now();
  const api = demoContacts() ? undefined : contactsApi();
  if (api) {
    api
      .sendRequest({
        id: request.id,
        rootId: request.rootId,
        raw: request.rootId,
        source: request.source,
        message: request.message
      })
      .catch(() => {
        if (request.status === 'pending') {
          request.status = 'failed';
          request.updatedAt = Date.now();
          request.unread = true;
        }
      });
  } else {
    simulatePeerReaction(spaceKey, requestId);
  }
}

/**
 * 回复对方的询问（对方回复「你是谁」后，双方可继续互复，直到对方拒绝/接受）。
 * 我回复后状态回到 pending（等待对方回应），对方再回复时回到 replied。
 * TODO(api): 真实环境应由内核把回复投递给对方（contacts 暂无此接口）。
 */
export function replyOutgoing(spaceKey: string, requestId: string, text: string): void {
  const request = contactsOf(spaceKey).outgoing.find((item) => item.id === requestId);
  const trimmed = text.trim();
  if (!request || !trimmed || request.status !== 'replied') {
    return;
  }
  (request.thread ??= []).push({ from: 'me', text: trimmed, ts: Date.now() });
  request.status = 'pending';
  request.updatedAt = Date.now();
  if (!contactsApi() || demoContacts()) {
    simulatePeerFollowUp(spaceKey, requestId);
  }
}

/**
 * 本地记录一条我发出的邀请（组织添加成员等不走 contacts.sendRequest 的场景）。
 * TODO(mock): 组织邀请的发出/对方反应应由内核组织模块回传事件，本地记录仅供展示。
 */
export function recordOutgoing(
  spaceKey: string,
  input: { rootId: string; source: string; inviteCode?: string }
): void {
  const space = contactsOf(spaceKey);
  const id = `out-${Date.now()}-${space.outgoing.length}`;
  space.outgoing.push({
    id,
    rootId: input.rootId,
    nickname: '待加入成员',
    message: '',
    source: input.source,
    status: 'pending',
    createdAt: Date.now(),
    updatedAt: Date.now(),
    inviteCode: input.inviteCode
  });
  if (!contactsApi() || demoContacts()) {
    simulatePeerReaction(spaceKey, id);
  }
}

// ------------------------------------------------------------------
// 模拟对方反应（仅非 Tauri mock 演示；真实环境由内核事件驱动 outbox 状态）
// 按发出顺序确定性轮换：回复询问 -> 接受 -> 连接失败 -> 拒绝，4 秒后生效；
// 任何新变化都置未读并刷新 updatedAt（列表冒泡 + 入口红点）
// ------------------------------------------------------------------

const SIMULATED_REACTIONS: Array<{ status: 'accepted' | 'ignored' | 'replied' | 'failed'; question?: string }> = [
  { status: 'replied', question: '请问你是哪位？' },
  { status: 'accepted' },
  { status: 'failed' },
  { status: 'ignored' }
];

let simulatedReactionSeq = 0;

/** 对方接受：落成朋友（对齐 FriendRequestAccepted 事件语义，仅个人空间） */
function acceptAsFriend(spaceKey: string, request: FriendRequest): void {
  const space = spaces[spaceKey];
  if (!space || spaceKey !== 'personal') {
    return;
  }
  if (!space.friends.some((friend) => friend.rootId === request.rootId)) {
    space.friends.push({
      ...emptyProfile(),
      rootId: request.rootId,
      nickname: request.nickname,
      signature: '',
      addedAt: Date.now()
    });
  }
}

function simulatePeerReaction(spaceKey: string, requestId: string): void {
  const reaction = SIMULATED_REACTIONS[simulatedReactionSeq % SIMULATED_REACTIONS.length];
  simulatedReactionSeq += 1;
  setTimeout(() => {
    const request = spaces[spaceKey]?.outgoing.find((item) => item.id === requestId);
    if (!request || request.status !== 'pending') {
      return;
    }
    request.status = reaction.status;
    request.updatedAt = Date.now();
    request.unread = true;
    if (reaction.question) {
      (request.thread ??= []).push({ from: 'peer', text: reaction.question, ts: Date.now() });
    }
    if (reaction.status === 'accepted') {
      acceptAsFriend(spaceKey, request);
    }
  }, 4000);
}

/**
 * 模拟对方对我回复的跟进（4 秒后）：第一次跟进对方想起我是谁（继续 replied），
 * 第二次跟进对方接受。仅当我回复后等待中（pending）且对方先问过话才生效。
 */
function simulatePeerFollowUp(spaceKey: string, requestId: string): void {
  setTimeout(() => {
    const request = spaces[spaceKey]?.outgoing.find((item) => item.id === requestId);
    if (!request || request.status !== 'pending') {
      return;
    }
    const peerCount = (request.thread ?? []).filter((msg) => msg.from === 'peer').length;
    if (peerCount === 0) {
      return;
    }
    request.updatedAt = Date.now();
    request.unread = true;
    if (peerCount === 1) {
      (request.thread ??= []).push({ from: 'peer', text: '原来是你啊，不好意思一时没认出来', ts: Date.now() });
      request.status = 'replied';
    } else {
      request.status = 'accepted';
      acceptAsFriend(spaceKey, request);
    }
  }, 4000);
}
