// 好友申请回复（F3c）前端桥接单测：
// - replyOutgoing 在 Tauri 桥接下调用 contacts.replyRequest(requestId, trimmedText)，
//   本地状态先行收敛（replied → pending、thread 追加 from=me）；
// - handleContactsP2pEvent 的 FriendRequestSent 分支：replied 记录置未读
//   （对方新询问 = 未读新变化，与 failed 同口径）。
import { describe, expect, it, vi } from 'vitest';
import type { FriendRequestDto } from '../../api';
import { contactsOf, replyOutgoing } from '../../mock/contacts';
import { handleContactsP2pEvent } from '../../mock/contacts/store';

const PERSONAL = 'personal';

function makeRequest(id: string, status: FriendRequestDto['status']): FriendRequestDto & { createdAt: number } {
  return {
    id,
    rootId: 'peer-root',
    nickname: '对方',
    message: '交个朋友',
    source: 'RootID 搜索',
    status,
    createdAt: 1,
    updatedAt: 1
  };
}

describe('好友申请回复桥接', () => {
  it('FriendRequestSent 携 replied 记录置 unread（对方新询问=未读新变化）', () => {
    const space = contactsOf(PERSONAL);
    space.outgoing.push(makeRequest('req-ev', 'pending'));

    handleContactsP2pEvent({
      kind: 'FriendRequestSent',
      data: {
        request: {
          ...makeRequest('req-ev', 'replied'),
          updatedAt: 2,
          thread: [{ from: 'peer', text: '请问你是哪位？', ts: 2 }]
        }
      }
    });

    const record = space.outgoing.find((item) => item.id === 'req-ev');
    expect(record?.status).toBe('replied');
    expect(record?.unread).toBe(true);
    expect(record?.thread?.[record.thread.length - 1]?.text).toBe('请问你是哪位？');
  });

  it('桥接投递回复：replyRequest 以 (requestId, trim 后 text) 调用，本地先收敛', async () => {
    const replyRequest = vi.fn().mockResolvedValue(makeRequest('req-1', 'pending'));
    const ok = () => Promise.resolve({ success: true });
    const emptyOverview = () =>
      Promise.resolve({
        friends: [],
        requests: [],
        outgoing: [],
        tags: [],
        groups: [],
        groupTree: [],
        memberExtras: {}
      });
    (window as any).__TAURI_INTERNALS__ = {};
    (window as any).electronAPI = {
      contacts: {
        overview: vi.fn(emptyOverview),
        updateProfile: vi.fn(ok),
        setBlocked: vi.fn(ok),
        removeFriend: vi.fn(ok),
        sendRequest: vi.fn(ok),
        replyRequest,
        resolveRequest: vi.fn(ok),
        tagCreate: vi.fn(ok),
        tagRename: vi.fn(ok),
        tagDelete: vi.fn(ok),
        groupCreate: vi.fn(ok),
        groupRename: vi.fn(ok),
        groupDelete: vi.fn(ok),
        groupMove: vi.fn(ok),
        setGroup: vi.fn(ok),
        orgGroupCreate: vi.fn(ok),
        orgGroupRename: vi.fn(ok),
        orgGroupDelete: vi.fn(ok),
        orgGroupMove: vi.fn(ok)
      }
    };
    try {
      const space = contactsOf(PERSONAL);
      // 等异步水合落地（空 overview），避免覆盖手工播种的申请记录
      await new Promise((resolve) => setTimeout(resolve, 0));
      space.outgoing.push(makeRequest('req-1', 'replied'));

      replyOutgoing(PERSONAL, 'req-1', ' 我是张三 ');

      expect(replyRequest).toHaveBeenCalledWith('req-1', '我是张三');
      const record = space.outgoing.find((item) => item.id === 'req-1');
      expect(record?.status).toBe('pending');
      expect(record?.thread?.[record.thread.length - 1]).toMatchObject({ from: 'me', text: '我是张三' });
    } finally {
      delete (window as any).__TAURI_INTERNALS__;
      delete (window as any).electronAPI;
    }
  });

  it('过期 FriendRequestSent 事件被丢弃（本地更新快照不被回退）', () => {
    const space = contactsOf(PERSONAL);
    // 本地已收敛到我的回复之后（pending + updatedAt 较新）
    space.outgoing.push({
      ...makeRequest('req-stale', 'pending'),
      updatedAt: 100,
      thread: [
        { from: 'peer', text: '请问你是哪位？', ts: 1 },
        { from: 'me', text: '我是张三', ts: 100 }
      ]
    });

    // 对方询问的旧快照事件（updatedAt 早于本地）迟到：整体丢弃，不回退状态/丢回答行
    handleContactsP2pEvent({
      kind: 'FriendRequestSent',
      data: { request: { ...makeRequest('req-stale', 'replied'), updatedAt: 2 } }
    });

    const record = space.outgoing.find((item) => item.id === 'req-stale');
    expect(record?.status).toBe('pending');
    expect(record?.thread?.length).toBe(2);
  });

  it('命令拒绝时回滚乐观更新（状态回 replied、thread 摘除）', async () => {
    const replyRequest = vi.fn().mockRejectedValue(new Error('当前状态不可回复'));
    (window as any).__TAURI_INTERNALS__ = {};
    (window as any).electronAPI = { contacts: { replyRequest } };
    try {
      const space = contactsOf(PERSONAL);
      space.outgoing.push(makeRequest('req-rollback', 'replied'));

      replyOutgoing(PERSONAL, 'req-rollback', '我是李四');
      await new Promise((resolve) => setTimeout(resolve, 0));

      const record = space.outgoing.find((item) => item.id === 'req-rollback');
      expect(record?.status).toBe('replied');
      expect(record?.thread?.some((msg) => msg.text === '我是李四')).toBe(false);
    } finally {
      delete (window as any).__TAURI_INTERNALS__;
      delete (window as any).electronAPI;
    }
  });
});
