// 链接预览前端桥接（F5，设计 §6）单测：
// - Tauri stub 下 sendText 含 URL：先上诚实占位（域名白名单站点名 + 空描述），
//   dto.link（壳层抓取的真实元数据）回来后替换本地占位；
// - dto 无 link（抓取失败/内网）时保留诚实占位，不出现编造内容；
// - 非 Tauri 环境 buildLinkPreview 直接给诚实占位。
// spaces 是模块级单例：每个用例用唯一空间 key 驱动，避免相互污染。
import { describe, expect, it, vi } from 'vitest';
import { buildLinkPreview, getMessages, onChatReceived, sendText } from '../../mock/messages';

/** 播种一个 direct 会话（借对端消息事件建会话，免 stub ensureDirect） */
function seedConversation(spaceKey: string, peerId: string): string {
  const convId = `dm:${peerId}`;
  onChatReceived({
    spaceKey,
    conversation: {
      id: convId,
      kind: 'direct',
      title: '对方',
      peerId,
      unreadCount: 0,
      pinnedAt: 0,
      muted: false,
      online: true,
      draft: '',
      updatedAt: Date.now()
    },
    message: {
      id: `seed-${peerId}`,
      senderId: peerId,
      senderName: '对方',
      type: 'text',
      content: '在吗',
      createdAt: Date.now(),
      recalled: false
    }
  });
  return convId;
}

function stubMessagesApi(sendTextImpl: ReturnType<typeof vi.fn>): void {
  (window as any).__TAURI_INTERNALS__ = {};
  (window as any).electronAPI = {
    messages: {
      listConversations: vi.fn().mockResolvedValue([]),
      listMessages: vi.fn().mockResolvedValue([]),
      sendText: sendTextImpl
    }
  };
}

function unstub(): void {
  delete (window as any).__TAURI_INTERNALS__;
  delete (window as any).electronAPI;
}

describe('链接预览桥接（F5）', () => {
  it('Tauri 下 dto.link 真实元数据替换本地占位', async () => {
    const realLink = {
      url: 'https://zhihu.com/question/1',
      title: '真实页面标题',
      description: '真实描述',
      siteName: '知乎',
      domain: 'zhihu.com'
    };
    const sendTextApi = vi.fn().mockResolvedValue({
      id: 'm-dto',
      senderId: 'me',
      senderName: '我',
      type: 'text',
      content: '看 https://zhihu.com/question/1',
      createdAt: 1,
      status: 'failed',
      recalled: false,
      link: realLink
    });
    stubMessagesApi(sendTextApi);
    try {
      const key = 'org:f5-link-merge';
      const convId = seedConversation(key, 'peer-f5-a');

      const msg = sendText(key, convId, '看 https://zhihu.com/question/1');
      // 同步语义：先上诚实占位（siteName=知乎，空描述），不含编造的示例文案
      expect(msg?.link?.siteName).toBe('知乎');
      expect(msg?.link?.title).toBe('知乎');
      expect(msg?.link?.description).toBe('');
      expect(msg?.link?.title).not.toContain('示例');

      await new Promise((resolve) => setTimeout(resolve, 0));
      const stored = getMessages(key, convId).find((m) => m.id === msg!.id);
      expect(sendTextApi).toHaveBeenCalledWith(key, convId, msg!.id, '看 https://zhihu.com/question/1', undefined);
      expect(stored?.link).toEqual(realLink);
      expect(stored?.status).toBe('failed');
    } finally {
      unstub();
    }
  });

  it('Tauri 下 dto 无 link 时保留诚实占位', async () => {
    const sendTextApi = vi.fn().mockResolvedValue({
      id: 'm-dto',
      senderId: 'me',
      senderName: '我',
      type: 'text',
      content: '看 https://weibo.com/u/1',
      createdAt: 1,
      status: 'failed',
      recalled: false
    });
    stubMessagesApi(sendTextApi);
    try {
      const key = 'org:f5-link-placeholder';
      const convId = seedConversation(key, 'peer-f5-b');

      const msg = sendText(key, convId, '看 https://weibo.com/u/1');
      await new Promise((resolve) => setTimeout(resolve, 0));
      const stored = getMessages(key, convId).find((m) => m.id === msg!.id);
      expect(stored?.link?.siteName).toBe('微博');
      expect(stored?.link?.title).toBe('微博');
      expect(stored?.link?.description).toBe('');
      expect(JSON.stringify(stored?.link)).not.toContain('示例');
    } finally {
      unstub();
    }
  });

  it('非 Tauri 环境 buildLinkPreview 给诚实占位（白名单/未知域名/www 前缀）', () => {
    unstub();
    expect(buildLinkPreview('https://zhihu.com/question/1')).toEqual({
      url: 'https://zhihu.com/question/1',
      domain: 'zhihu.com',
      siteName: '知乎',
      title: '知乎',
      description: ''
    });
    // www. 前缀归一后命中白名单
    expect(buildLinkPreview('https://www.github.com/spark').siteName).toBe('GitHub');
    // 未知域名显示域名本身
    const unknown = buildLinkPreview('https://blog.example.com/a');
    expect(unknown.siteName).toBe('blog.example.com');
    expect(unknown.title).toBe('blog.example.com');
    expect(unknown.description).toBe('');
    expect(JSON.stringify(unknown)).not.toContain('示例');
  });
});
