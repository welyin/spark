/**
 * 非 Tauri 环境（单测/纯前端开发）的本地种子数据。
 * Tauri 环境不使用：空间一律从空状态建、经 overview 水合为内核真实数据。
 * 注意：渲染单测（contact-groups.test.ts 等）直接依赖种子朋友与分组树，勿删。
 */
import type { MockFriend, SpaceContacts } from './types';
import { emptyProfile } from './types';

// 假 RootID 仅供展示（真实 RootID 为 64 位十六进制）
const mockRootId = (seed: string): string => (seed.repeat(64) + '0'.repeat(64)).slice(0, 64);

export function seedPersonal(): SpaceContacts {
  const friend = (nickname: string, seed: string, patch: Partial<MockFriend> = {}): MockFriend => ({
    ...emptyProfile(),
    rootId: mockRootId(seed),
    nickname,
    signature: '',
    addedAt: Date.now() - 86400000 * 30,
    ...patch
  });
  /** n 分钟前（申请列表按时间倒序混排收到的与我发出的，时间交错便于看效果） */
  const ago = (minutes: number): number => Date.now() - minutes * 60000;
  return {
    friends: [
      friend('阿强', 'a1', { signature: '越努力越幸运', gender: 'male', groupId: 'group-classmate' }),
      friend('Alice', 'a2', {
        remark: '爱丽丝',
        tagIds: ['tag-colleague'],
        gender: 'female',
        signature: 'Curiouser and curiouser'
      }),
      friend('博哥', 'b1', { phones: ['138****1234'], tagIds: ['tag-neighbor'], gender: 'male' }),
      friend('陈静', 'c1', { groupId: 'group-family', gender: 'female', signature: '静水流深' }),
      friend('大力', 'd1', {
        memo: '周三值班',
        tagIds: ['tag-neighbor', 'tag-volunteer'],
        groupId: 'group-family',
        gender: 'male'
      }),
      friend('Emma', 'e1', { permission: 'chatOnly', gender: 'female', signature: 'Less is more' }),
      friend('高翔', 'g1', { tagIds: ['tag-colleague'], gender: 'male', signature: '山高人为峰' }),
      friend('Jack', 'j1', { gender: 'male' }),
      // 资料完整丰满的种子朋友：备注/电话/标签/备忘/照片占位均有值，用于展示资料面板全字段效果
      friend('李雷', 'l1', {
        remark: '老李',
        phones: ['139****5678', '010-6255****'],
        tagIds: ['tag-colleague'],
        memo: '市场部，周二例会对接人；孩子今年上小学',
        photos: ['photo-1', 'photo-2', 'photo-3'],
        signature: 'Stay hungry, stay foolish',
        gender: 'male'
      }),
      friend('韩梅梅', 'h2', {
        remark: '梅梅',
        phones: ['137****2468'],
        tagIds: ['tag-neighbor', 'tag-volunteer'],
        memo: '同小区 3 栋，社区拼单群群主，周末羽毛球搭子',
        photos: ['photo-4', 'photo-5'],
        signature: '热爱生活，热爱分享',
        gender: 'female',
        groupId: 'group-classmate'
      }),
      // 拉黑示例：出现在「黑名单」分组与个人设置「朋友权限-通讯黑名单」中
      friend('123客服', 'x1', { blocked: true }),
      // 与下方「收到的申请」req-6（已接受）同一 rootId：对方已成朋友，详情里「查看资料」可达
      friend('阿May', 'r6', { gender: 'female', signature: '社区团购找我', addedAt: Date.now() - 86400000 * 2 })
    ],
    // 收到的申请：覆盖 待处理（多来源/长消息/无消息）/ 已接受 / 已忽略，
    // 便于测试「新的朋友」列表与详情 UI
    requests: [
      {
        id: 'req-1',
        rootId: mockRootId('r1'),
        nickname: '小米粥',
        message: '我是小米，上周活动见过',
        source: 'RootID 搜索',
        status: 'pending',
        createdAt: ago(10),
        updatedAt: ago(10),
        unread: true
      },
      {
        id: 'req-2',
        rootId: mockRootId('r2'),
        nickname: '老周',
        message: '通过名片扫码添加',
        source: '扫码',
        status: 'pending',
        createdAt: ago(70),
        updatedAt: ago(70),
        unread: true
      },
      {
        id: 'req-3',
        rootId: mockRootId('r3'),
        nickname: 'Sara',
        message: '邀请码添加',
        source: '邀请码',
        status: 'pending',
        createdAt: ago(180),
        updatedAt: ago(180)
      },
      {
        id: 'req-4',
        rootId: mockRootId('r4'),
        nickname: '大熊',
        message:
          '你好，我是上周末爬山活动群里的大熊，就是带了无人机那个，加个好友以后约路线，顺便把拍的视频发你一份',
        source: '名片',
        status: 'pending',
        createdAt: ago(40),
        updatedAt: ago(40),
        unread: true
      },
      {
        id: 'req-5',
        rootId: mockRootId('r5'),
        nickname: 'Momo',
        message: '',
        source: '名片',
        status: 'pending',
        createdAt: ago(120),
        updatedAt: ago(120)
      },
      {
        id: 'req-6',
        rootId: mockRootId('r6'),
        nickname: '阿May',
        message: '邻居群看到的，咨询一下社区团购',
        source: '名片',
        status: 'accepted',
        createdAt: ago(200),
        updatedAt: ago(190)
      },
      {
        id: 'req-7',
        rootId: mockRootId('r7'),
        nickname: '推广-理财顾问',
        message: '你好，我是你的专属理财顾问',
        source: 'RootID 搜索',
        status: 'ignored',
        createdAt: ago(300),
        updatedAt: ago(290)
      }
    ],
    tags: [
      { id: 'tag-neighbor', name: '邻居' },
      { id: 'tag-volunteer', name: '志愿者' },
      { id: 'tag-colleague', name: '同事' }
    ],
    groups: [
      { id: 'group-family', name: '家人' },
      { id: 'group-classmate', name: '同学' }
    ],
    groupTree: [],
    // 我发出的申请：覆盖 等待确认 / 对方回复询问 / 连接失败 / 对方拒绝 / 已接受。
    // 已接受的一条与上方朋友「高翔」同一 rootId（对方同意后落成朋友）
    outgoing: [
      {
        id: 'out-seed-1',
        rootId: mockRootId('out1'),
        nickname: '小王',
        message: '我是上周分享会坐你旁边的',
        source: '名片',
        status: 'pending',
        createdAt: ago(25),
        updatedAt: ago(25)
      },
      {
        id: 'out-seed-2',
        rootId: mockRootId('out2'),
        nickname: 'Tom',
        message: '你好，朋友介绍认识的',
        source: '名片',
        status: 'replied',
        createdAt: ago(55),
        updatedAt: ago(50),
        unread: true,
        thread: [{ from: 'peer', text: '请问你是哪位？我们好像没见过？', ts: ago(50) }]
      },
      {
        id: 'out-seed-5',
        rootId: mockRootId('out5'),
        nickname: '老吴',
        message: '同学聚会上说好的，加一下',
        source: '名片',
        status: 'failed',
        createdAt: ago(90),
        updatedAt: ago(85),
        unread: true
      },
      {
        id: 'out-seed-3',
        rootId: mockRootId('out3'),
        nickname: '阿芳',
        message: '',
        source: '名片',
        status: 'ignored',
        createdAt: ago(150),
        updatedAt: ago(140)
      },
      {
        id: 'out-seed-4',
        rootId: mockRootId('g1'),
        nickname: '高翔',
        message: '同事介绍的，认识一下',
        source: '名片',
        status: 'accepted',
        createdAt: ago(240),
        updatedAt: ago(230)
      }
    ],
    memberExtras: {}
  };
}

export function seedOrg(): SpaceContacts {
  /** n 分钟前（同 seedPersonal：时间交错便于看混排效果） */
  const ago = (minutes: number): number => Date.now() - minutes * 60000;
  return {
    friends: [],
    // 组织空间「新的成员」申请，真实流程走邀请码（§4.2）：
    // 覆盖 待确认 / 已接受 / 已忽略，便于测试「新的成员」列表与详情 UI
    requests: [
      {
        id: 'req-org-1',
        rootId: mockRootId('o1'),
        nickname: '待加入成员',
        message: '凭邀请码加入，等待管理员确认',
        source: '邀请码',
        status: 'pending',
        createdAt: ago(15),
        updatedAt: ago(15),
        unread: true
      },
      {
        id: 'req-org-2',
        rootId: mockRootId('o2'),
        nickname: '小赵',
        message: '市场部新同事，邀请码加入',
        source: '邀请码',
        status: 'pending',
        createdAt: ago(60),
        updatedAt: ago(60),
        unread: true
      },
      {
        id: 'req-org-3',
        rootId: mockRootId('o3'),
        nickname: 'Kevin',
        message: '',
        source: '名片',
        status: 'pending',
        createdAt: ago(110),
        updatedAt: ago(110)
      },
      {
        id: 'req-org-4',
        rootId: mockRootId('o4'),
        nickname: '钱多多',
        message: '上月加入的实习生',
        source: '邀请码',
        status: 'accepted',
        createdAt: ago(200),
        updatedAt: ago(190)
      },
      {
        id: 'req-org-5',
        rootId: mockRootId('o5'),
        nickname: '不明账号',
        message: '请求加入组织',
        source: 'RootID 搜索',
        status: 'ignored',
        createdAt: ago(260),
        updatedAt: ago(250)
      }
    ],
    tags: [{ id: 'tag-core', name: '核心成员' }],
    groups: [],
    groupTree: [
      {
        id: 'og-hq',
        name: '总部',
        children: [
          { id: 'og-tech', name: '技术部', children: [] },
          { id: 'og-market', name: '市场部', children: [] }
        ]
      },
      { id: 'og-branch', name: '分部', children: [] }
    ],
    // 我发出的成员邀请：等待凭邀请码加入 / 对方回复询问 / 连接失败（§4.2 邀请码流程）
    outgoing: [
      {
        id: 'out-org-1',
        rootId: mockRootId('oinv1'),
        nickname: '待加入成员',
        message: '',
        source: '邀请码',
        status: 'pending',
        createdAt: ago(35),
        updatedAt: ago(35),
        inviteCode: 'spark-invite:demo-hq-8f3k2m'
      },
      {
        id: 'out-org-2',
        rootId: mockRootId('oinv2'),
        nickname: '老刘',
        message: '',
        source: '名片',
        status: 'replied',
        createdAt: ago(85),
        updatedAt: ago(80),
        unread: true,
        thread: [{ from: 'peer', text: '请问是哪个部门的邀请？', ts: ago(80) }]
      },
      {
        id: 'out-org-3',
        rootId: mockRootId('oinv3'),
        nickname: '待加入成员',
        message: '',
        source: '名片',
        status: 'failed',
        createdAt: ago(140),
        updatedAt: ago(130),
        unread: true
      }
    ],
    memberExtras: {}
  };
}
