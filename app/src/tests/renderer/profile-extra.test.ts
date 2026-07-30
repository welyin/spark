// profile-extra store：个人空间键（rootId）走内核（status 水合读 / updateProfile 写），
// org 作用域键（rootId@orgId）F2b 起也走内核（listMine 水合读 / updateMyIdentity 写，
// 失败回滚）。组件侧 API 签名不变。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const ROOT_ID = 'root-personal';
const ORG_ID = 'org-1';
const ORG_KEY = 'root-personal@org-1';

type StatusValue = {
  rootId: string | null;
  gender: string | null;
  region: string | null;
  signature: string | null;
};

/** 最小 OrganizationView 线形（仅 org 键水合关心的字段） */
function makeOrgView(member: Record<string, unknown>) {
  return {
    orgId: ORG_ID,
    name: '测试组织',
    description: '',
    createdAt: 0,
    createdBy: ROOT_ID,
    updatedAt: 0,
    members: [{ rootId: ROOT_ID, role: 'member', joinedAt: 1, addedBy: ROOT_ID, ...member }],
    currentUserRole: 'member',
    isCurrentUserAdmin: false,
    memberCount: 1,
    adminCount: 0
  };
}

let statusValue: StatusValue;
let updateProfile: ReturnType<typeof vi.fn>;
let listMine: ReturnType<typeof vi.fn>;
let updateMyIdentity: ReturnType<typeof vi.fn>;

async function importStore() {
  vi.resetModules();
  return await import('../../stores/profile-extra');
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

beforeEach(() => {
  localStorage.clear();
  statusValue = { rootId: ROOT_ID, gender: '女', region: '杭州', signature: '保持热爱' };
  updateProfile = vi.fn().mockResolvedValue(null);
  listMine = vi.fn().mockResolvedValue([
    makeOrgView({ gender: '男', region: '广州', signature: '组织签名' })
  ]);
  updateMyIdentity = vi.fn().mockResolvedValue(makeOrgView({}));
  (window as any).electronAPI = {
    rootIdentity: {
      status: async () => statusValue,
      updateProfile
    },
    organization: { listMine, updateMyIdentity }
  };
  vi.spyOn(console, 'warn').mockImplementation(() => {});
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('个人空间键（rootId）走内核', () => {
  it('首次读触发 status 水合，缓存异步回填', async () => {
    const store = await importStore();
    // 同步读返回默认值（签名不变），随后异步水合
    expect(store.getProfileExtra(ROOT_ID)).toEqual({ gender: '', region: '', signature: '' });
    await flush();
    expect(store.getProfileExtra(ROOT_ID)).toEqual({ gender: '女', region: '杭州', signature: '保持热爱' });
  });

  it('status 的 rootId 与键不匹配时不写入缓存', async () => {
    statusValue.rootId = 'other-root';
    const store = await importStore();
    store.getProfileExtra(ROOT_ID);
    await flush();
    expect(store.getProfileExtra(ROOT_ID)).toEqual({ gender: '', region: '', signature: '' });
    expect(store.profileExtras.value[ROOT_ID]).toBeUndefined();
  });

  it('写：更新缓存并调 updateProfile（空串透传=清除，缺省字段不传），不落 localStorage', async () => {
    const store = await importStore();
    store.setProfileExtra(ROOT_ID, { region: '上海', signature: '' });
    // 缓存立即可读
    expect(store.getProfileExtra(ROOT_ID)).toEqual({ gender: '', region: '上海', signature: '' });
    expect(updateProfile).toHaveBeenCalledWith({ region: '上海', signature: '' });
    expect(localStorage.getItem('spark:profile-extra')).toBeNull();
  });

  it('本地写入后水合结果不得覆盖更新鲜的值', async () => {
    let resolveStatus: (v: StatusValue) => void = () => {};
    (window as any).electronAPI.rootIdentity.status = () =>
      new Promise<StatusValue>((resolve) => {
        resolveStatus = resolve;
      });
    const store = await importStore();
    store.getProfileExtra(ROOT_ID); // 触发水合（未决）
    store.setProfileExtra(ROOT_ID, { region: '北京' }); // 本地写先行
    resolveStatus({ rootId: ROOT_ID, gender: '男', region: '杭州', signature: '旧值' });
    await flush();
    expect(store.getProfileExtra(ROOT_ID).region).toBe('北京');
  });
});

describe('org 作用域键（rootId@orgId）走内核（F2b）', () => {
  it('首次读触发 listMine 水合，成员身份字段异步回填', async () => {
    const store = await importStore();
    expect(store.getProfileExtra(ORG_KEY)).toEqual({ gender: '', region: '', signature: '' });
    await flush();
    expect(store.getProfileExtra(ORG_KEY)).toEqual({ gender: '男', region: '广州', signature: '组织签名' });
  });

  it('写：更新缓存并调 updateMyIdentity（空串透传=清除，缺省字段不传），不落 localStorage', async () => {
    const store = await importStore();
    store.setProfileExtra(ORG_KEY, { gender: '男', region: '北京' });
    expect(store.getProfileExtra(ORG_KEY)).toEqual({ gender: '男', region: '北京', signature: '' });
    expect(updateMyIdentity).toHaveBeenCalledWith(ORG_ID, { gender: '男', region: '北京' });
    expect(updateProfile).not.toHaveBeenCalled();
    expect(localStorage.getItem('spark:profile-extra')).toBeNull();
  });

  it('写失败回滚展示值并 console.warn', async () => {
    updateMyIdentity.mockRejectedValue(new Error('MemberNotFound'));
    const store = await importStore();
    store.setProfileExtra(ORG_KEY, { signature: '新签名' });
    expect(store.getProfileExtra(ORG_KEY).signature).toBe('新签名');
    await flush();
    expect(store.getProfileExtra(ORG_KEY).signature).toBe('');
    expect(console.warn).toHaveBeenCalled();
  });

  it('旧 localStorage org 键数据被丢弃（开发期以内核水合为准）', async () => {
    localStorage.setItem(
      'spark:profile-extra',
      JSON.stringify({
        [ORG_KEY]: { gender: '女', region: '旧地区', signature: '旧签名' }
      })
    );
    const store = await importStore();
    // 不读 localStorage：水合前为默认值，水合后以内核为准
    expect(store.getProfileExtra(ORG_KEY)).toEqual({ gender: '', region: '', signature: '' });
    await flush();
    expect(store.getProfileExtra(ORG_KEY)).toEqual({ gender: '男', region: '广州', signature: '组织签名' });
  });

  it('非 Tauri 环境：无 organization API 时读返回默认值、写保留本地值', async () => {
    delete (window as any).electronAPI.organization;
    const store = await importStore();
    expect(store.getProfileExtra(ORG_KEY)).toEqual({ gender: '', region: '', signature: '' });
    store.setProfileExtra(ORG_KEY, { region: '本地地区' });
    expect(store.getProfileExtra(ORG_KEY).region).toBe('本地地区');
    expect(updateMyIdentity).not.toHaveBeenCalled();
  });
});
