// 未读角标系统桥接（F4）单测：
// - Tauri 环境（stub __TAURI_INTERNALS__ + electronAPI.system）下 totalUnread
//   变化除更新 document.title 外，fire-and-forget 调用 system.setBadge(n)；
// - 免打扰会话不计入桥接的未读数；清零后桥接 setBadge(0)（命令侧清除徽标）。
// spaces/totalUnread 是模块级单例：每个用例用唯一空间 key 驱动，避免相互污染。
import { describe, expect, it, vi } from 'vitest';
import { nextTick } from 'vue';
import { markRead, onChatReceived, totalUnread } from '../../stores/messages';

function pushUnread(spaceKey: string, peerId: string, unread: number, muted = false): string {
  const convId = `dm:${peerId}`;
  onChatReceived({
    spaceKey,
    conversation: {
      id: convId,
      kind: 'direct',
      title: '对方',
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
      senderName: '对方',
      type: 'text',
      content: '你好',
      createdAt: Date.now(),
      recalled: false
    }
  });
  return convId;
}

describe('未读角标系统桥接（F4）', () => {
  it('Tauri 环境下 totalUnread 变化调用 system.setBadge(n)，清零后桥接 0', async () => {
    const setBadge = vi.fn().mockResolvedValue(undefined);
    (window as any).__TAURI_INTERNALS__ = {};
    (window as any).electronAPI = { system: { setBadge } };
    try {
      const key = 'org:f4-badge';
      const convId = pushUnread(key, 'peer-f4-a', 3);
      await nextTick();
      expect(totalUnread.value).toBe(3);
      expect(setBadge).toHaveBeenLastCalledWith(3);
      expect(document.title).toBe('(3) 星火 Spark');

      markRead(key, convId);
      await nextTick();
      expect(totalUnread.value).toBe(0);
      expect(setBadge).toHaveBeenLastCalledWith(0);
      expect(document.title).toBe('星火 Spark');
    } finally {
      delete (window as any).__TAURI_INTERNALS__;
      delete (window as any).electronAPI;
    }
  });

  it('免打扰会话不计入桥接未读数', async () => {
    const setBadge = vi.fn().mockResolvedValue(undefined);
    (window as any).__TAURI_INTERNALS__ = {};
    (window as any).electronAPI = { system: { setBadge } };
    try {
      const key = 'org:f4-badge-muted';
      const convId = pushUnread(key, 'peer-f4-muted', 5, true);
      await nextTick();
      expect(totalUnread.value).toBe(0);
      // totalUnread 未变化（免打扰不计），watch 不触发，徽标不更新
      expect(setBadge).not.toHaveBeenCalled();

      markRead(key, convId);
    } finally {
      delete (window as any).__TAURI_INTERNALS__;
      delete (window as any).electronAPI;
    }
  });

  it('非 Tauri 环境不触达 system.setBadge（仅更新标题）', async () => {
    const setBadge = vi.fn();
    delete (window as any).__TAURI_INTERNALS__;
    (window as any).electronAPI = { system: { setBadge } };
    try {
      const key = 'org:f4-badge-web';
      const convId = pushUnread(key, 'peer-f4-web', 2);
      await nextTick();
      expect(setBadge).not.toHaveBeenCalled();
      expect(document.title).toBe('(2) 星火 Spark');

      markRead(key, convId);
      await nextTick();
    } finally {
      delete (window as any).electronAPI;
    }
  });
});
