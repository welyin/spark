/**
 * M11/M17 消息 store 补充：
 * - unreadConversationCountOf：未读会话数口径（免打扰会话不计；清零未读后不计）；
 * - toggleFavoriteMessage / isFavoriteMessage：消息收藏本机持久化（内核无收藏存储），
 *   切换往返 + localStorage 落盘。
 * spaces 是模块级单例：用唯一空间 key 驱动，避免相互污染。
 */
import { beforeEach, describe, expect, it } from 'vitest';
import {
  isFavoriteMessage,
  markRead,
  onChatReceived,
  toggleFavoriteMessage,
  unreadConversationCountOf
} from '../../stores/messages';

const KEY = 'org:m11-m17-test';

const pushUnreadConv = (peerId: string, unread: number, muted = false): string => {
  const convId = `dm:${peerId}`;
  onChatReceived({
    spaceKey: KEY,
    conversation: {
      id: convId,
      kind: 'direct',
      title: peerId,
      peerId,
      unreadCount: unread,
      pinnedAt: 0,
      muted,
      online: true,
      draft: '',
      updatedAt: Date.now()
    },
    message: {
      id: `msg-${peerId}`,
      senderId: peerId,
      senderName: peerId,
      type: 'text',
      content: '你好',
      createdAt: Date.now(),
      recalled: false
    }
  });
  return convId;
};

beforeEach(() => {
  localStorage.clear();
});

describe('unreadConversationCountOf（M11 消息角标口径）', () => {
  it('按会话条数计数而非消息总数；免打扰会话不计；清零未读后不计', () => {
    const a = pushUnreadConv('m11-a', 3);
    pushUnreadConv('m11-b', 5);
    pushUnreadConv('m11-c', 9, true); // 免打扰
    expect(unreadConversationCountOf(KEY)).toBe(2);

    markRead(KEY, a);
    expect(unreadConversationCountOf(KEY)).toBe(1);
  });
});

describe('消息收藏（M17，本机持久化）', () => {
  it('切换往返 + 落盘 localStorage', () => {
    expect(isFavoriteMessage(KEY, 'conv-1', 'm-1')).toBe(false);
    expect(toggleFavoriteMessage(KEY, 'conv-1', 'm-1')).toBe(true);
    expect(isFavoriteMessage(KEY, 'conv-1', 'm-1')).toBe(true);

    const persisted = JSON.parse(localStorage.getItem('spark:message-favorites') ?? '{}');
    expect(persisted[`${KEY}|conv-1|m-1`]).toBe(true);

    expect(toggleFavoriteMessage(KEY, 'conv-1', 'm-1')).toBe(false);
    expect(isFavoriteMessage(KEY, 'conv-1', 'm-1')).toBe(false);
  });

  it('收藏按 空间+会话+消息 定位，互不串扰', () => {
    toggleFavoriteMessage(KEY, 'conv-1', 'm-1');
    expect(isFavoriteMessage(KEY, 'conv-1', 'm-2')).toBe(false);
    expect(isFavoriteMessage('org:other', 'conv-1', 'm-1')).toBe(false);
  });
});
