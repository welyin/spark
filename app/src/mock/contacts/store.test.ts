// mock/contacts/store P2P 事件单测：FriendRequestReceived 按 id upsert、
// FriendRequestSent 投递终态回写 outbox、FriendProfileUpdated 资料同步、
// OrgInviteUpdated 组织邀请回执合入组织空间「我发出的邀请」
import { describe, it, expect } from 'vitest';
import type { FriendRequestDto, OrgInviteRecordDto } from '../../api';
import { handleContactsP2pEvent, contactsOf } from './store';
import { emptyProfile } from './types';

let dtoSeq = 0;

function makeRequestDto(overrides: Partial<FriendRequestDto> = {}): FriendRequestDto {
  dtoSeq += 1;
  return {
    id: `test-req-${dtoSeq}`,
    rootId: `root-${dtoSeq}`,
    nickname: `申请人${dtoSeq}`,
    message: '你好，加一下',
    source: 'search',
    status: 'pending',
    createdAt: 1_000,
    updatedAt: 1_000,
    ...overrides
  };
}

describe('FriendRequestReceived（按 id upsert）', () => {
  it('新申请：入列并置未读，updatedAt 取 dto 值', () => {
    const dto = makeRequestDto({ updatedAt: 5_000 });
    handleContactsP2pEvent({ kind: 'FriendRequestReceived', data: { request: dto } });
    const request = contactsOf('personal').requests.find((item) => item.id === dto.id);
    expect(request).toBeDefined();
    expect(request?.unread).toBe(true);
    expect(request?.updatedAt).toBe(5_000);
  });

  it('同 id 重复到达=对端重试的内容更新：替换字段不新增，置未读', () => {
    const space = contactsOf('personal');
    const before = space.requests.length;
    const dto = makeRequestDto({ message: '第一条', updatedAt: 5_000 });
    handleContactsP2pEvent({ kind: 'FriendRequestReceived', data: { request: dto } });
    handleContactsP2pEvent({
      kind: 'FriendRequestReceived',
      data: { request: { ...dto, message: '重试更新后的附言', nickname: '新昵称', updatedAt: 6_000 } }
    });
    const matches = space.requests.filter((item) => item.id === dto.id);
    expect(matches).toHaveLength(1);
    expect(space.requests.length).toBe(before + 1);
    expect(matches[0].message).toBe('重试更新后的附言');
    expect(matches[0].nickname).toBe('新昵称');
    expect(matches[0].updatedAt).toBe(6_000);
    expect(matches[0].unread).toBe(true);
  });
});

describe('FriendRequestSent（我发出申请的投递终态）', () => {
  it("pending=已送达：按 id 更新既有 outbox 记录（回填 nickname），不新增", () => {
    const space = contactsOf('personal');
    const dto = makeRequestDto({ status: 'pending', nickname: '待回填' });
    space.outgoing.push({
      id: dto.id,
      rootId: dto.rootId,
      nickname: dto.nickname,
      message: dto.message,
      source: dto.source,
      status: 'pending',
      createdAt: 1_000,
      updatedAt: 1_000
    });
    const before = space.outgoing.length;
    handleContactsP2pEvent({
      kind: 'FriendRequestSent',
      data: { request: { ...dto, nickname: '真实昵称', updatedAt: 7_000 } }
    });
    const matches = space.outgoing.filter((item) => item.id === dto.id);
    expect(matches).toHaveLength(1);
    expect(space.outgoing.length).toBe(before);
    expect(matches[0].nickname).toBe('真实昵称');
    expect(matches[0].status).toBe('pending');
    expect(matches[0].updatedAt).toBe(7_000);
  });

  it('failed=投递失败：置未读提醒（可重试）', () => {
    const space = contactsOf('personal');
    const dto = makeRequestDto();
    space.outgoing.push({
      id: dto.id,
      rootId: dto.rootId,
      nickname: dto.nickname,
      message: dto.message,
      source: dto.source,
      status: 'pending',
      createdAt: 1_000,
      updatedAt: 1_000
    });
    handleContactsP2pEvent({
      kind: 'FriendRequestSent',
      data: { request: { ...dto, status: 'failed', updatedAt: 8_000 } }
    });
    const record = space.outgoing.find((item) => item.id === dto.id);
    expect(record?.status).toBe('failed');
    expect(record?.unread).toBe(true);
    expect(record?.updatedAt).toBe(8_000);
  });

  it('本地无记录（如另一台设备发出）：按 id 落一条新记录', () => {
    const dto = makeRequestDto({ status: 'pending', updatedAt: 9_000 });
    handleContactsP2pEvent({ kind: 'FriendRequestSent', data: { request: dto } });
    const record = contactsOf('personal').outgoing.find((item) => item.id === dto.id);
    expect(record).toBeDefined();
    expect(record?.status).toBe('pending');
    expect(record?.updatedAt).toBe(9_000);
  });
});

describe('FriendProfileUpdated（对端资料同步）', () => {
  it('按 rootId 就地更新好友昵称与头像', () => {
    const space = contactsOf('personal');
    space.friends.push({
      ...emptyProfile(),
      rootId: 'root-profile-1',
      nickname: '旧昵称',
      signature: '',
      addedAt: 1_000
    });
    handleContactsP2pEvent({
      kind: 'FriendProfileUpdated',
      data: { rootId: 'root-profile-1', nickname: '新昵称', avatar: 'data:image/png;base64,xxx' }
    });
    const friend = space.friends.find((item) => item.rootId === 'root-profile-1');
    expect(friend?.nickname).toBe('新昵称');
    expect(friend?.avatar).toBe('data:image/png;base64,xxx');
  });

  it('avatar 缺省时保留既有头像；未知 rootId 不产生副作用', () => {
    const space = contactsOf('personal');
    space.friends.push({
      ...emptyProfile(),
      rootId: 'root-profile-2',
      nickname: '甲',
      signature: '',
      avatar: 'data:image/png;base64,old',
      addedAt: 1_000
    });
    const before = space.friends.length;
    handleContactsP2pEvent({ kind: 'FriendProfileUpdated', data: { rootId: 'root-profile-2', nickname: '乙' } });
    handleContactsP2pEvent({
      kind: 'FriendProfileUpdated',
      data: { rootId: 'root-unknown', nickname: '陌生人', avatar: 'data:x' }
    });
    const friend = space.friends.find((item) => item.rootId === 'root-profile-2');
    expect(friend?.nickname).toBe('乙');
    expect(friend?.avatar).toBe('data:image/png;base64,old');
    expect(space.friends.length).toBe(before);
  });
});

// ------------------------------------------------------------------
// OrgInviteUpdated（组织邀请回执 → 组织空间「我发出的邀请」upsert）
// ------------------------------------------------------------------

let inviteSeq = 0;

function makeOrgInviteDto(overrides: Partial<OrgInviteRecordDto> = {}): OrgInviteRecordDto {
  inviteSeq += 1;
  return {
    id: `test-inv-${inviteSeq}`,
    orgId: 'org-invite-test',
    orgName: '测试组织',
    peerRootId: `root-peer-${inviteSeq}`,
    peerNickname: `被邀请人${inviteSeq}`,
    direction: 'outgoing',
    status: 'pending',
    createdAt: 1_000,
    updatedAt: 1_000,
    ...overrides
  };
}

describe('OrgInviteUpdated（组织邀请回执）', () => {
  it('本地无记录：按 org:{orgId} 空间落一条我发出的邀请并置未读', () => {
    const dto = makeOrgInviteDto({ status: 'accepted', updatedAt: 5_000 });
    handleContactsP2pEvent({ kind: 'OrgInviteUpdated', data: dto });
    const record = contactsOf('org:org-invite-test').outgoing.find((item) => item.id === dto.id);
    expect(record).toBeDefined();
    expect(record?.rootId).toBe(dto.peerRootId);
    expect(record?.nickname).toBe(dto.peerNickname);
    expect(record?.message).toBe('邀请加入「测试组织」');
    expect(record?.source).toBe('org-invite');
    expect(record?.status).toBe('accepted');
    expect(record?.updatedAt).toBe(5_000);
    expect(record?.unread).toBe(true);
  });

  it('既有记录：按 id 更新 status/updatedAt，不新增', () => {
    const space = contactsOf('org:org-invite-test');
    const dto = makeOrgInviteDto({ status: 'pending' });
    space.outgoing.push({
      id: dto.id,
      rootId: dto.peerRootId,
      nickname: dto.peerNickname,
      message: `邀请加入「${dto.orgName}」`,
      source: 'org-invite',
      status: 'pending',
      createdAt: 1_000,
      updatedAt: 1_000
    });
    const before = space.outgoing.length;
    handleContactsP2pEvent({
      kind: 'OrgInviteUpdated',
      data: { ...dto, status: 'declined', updatedAt: 6_000 }
    });
    const matches = space.outgoing.filter((item) => item.id === dto.id);
    expect(matches).toHaveLength(1);
    expect(space.outgoing.length).toBe(before);
    expect(matches[0].status).toBe('declined');
    expect(matches[0].updatedAt).toBe(6_000);
    expect(matches[0].unread).toBe(true);
  });

  it('incoming 方向的记录不落入我发出的列表', () => {
    const dto = makeOrgInviteDto({ direction: 'incoming', status: 'accepted' });
    handleContactsP2pEvent({ kind: 'OrgInviteUpdated', data: dto });
    expect(contactsOf('org:org-invite-test').outgoing.find((item) => item.id === dto.id)).toBeUndefined();
  });
});
