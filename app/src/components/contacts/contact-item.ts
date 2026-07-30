/**
 * friend（个人空间朋友记录）→ ContactItem 的单点映射：
 * 通讯录联系人列表（use-contacts-data 个人分支）与新朋友面板「已接受」内嵌资料卡共用，
 * 不再各自维护一份（历史上两份已漂移：头像一个手拼一个走统一入口，
 * 「自己」判定一个自拉 rootIdentity.status() 一个用 currentUser.rootId）。
 */
import { currentUser } from '../../stores/current-user';
import { getProfileExtra } from '../../stores/profile-extra';
import { personAvatarSource, personDisplayName } from '../../stores/avatar-sources';
import type { MockFriend } from '../../mock/contacts';
import type { ContactItem } from './types';

/** 本地资料的性别文案（'男'/'女'/''）→ ContactItem 的 'male'/'female'/undefined（与组织成员同组件同规则） */
const toGender = (gender: string): ContactItem['gender'] =>
  gender === '男' ? 'male' : gender === '女' ? 'female' : undefined;

export function friendContactItem(friend: MockFriend): ContactItem {
  // 「自己」判定统一用 currentUser 单例（App 挂载时已刷新），不再各自拉 rootIdentity.status()
  const isSelf = friend.rootId === currentUser.rootId;
  // 自己：性别/签名取本地真实资料（内核朋友记录的 gender/signature 目前不落库，
  // 网络同步也还没有档案通道）；其他朋友用内核数据（gender 缺省则不显示图标，与组织同组件同规则）
  const extra = isSelf ? getProfileExtra(friend.rootId) : null;
  const signature = extra ? extra.signature : friend.signature;
  return {
    rootId: friend.rootId,
    displayName: personDisplayName('personal', friend.rootId),
    // 第二行展示签名（无签名则不显示），RootID 属隐私不上列表
    subtitle: signature,
    // 统一入口：自己→个人身份头像，朋友→朋友记录头像（含同步更新）
    avatarImage: personAvatarSource('personal', friend.rootId).image,
    signature,
    gender: extra ? toGender(extra.gender) : friend.gender,
    nickname: friend.nickname,
    blocked: friend.blocked,
    // 内核默认把自己作为联系人（发消息给自己=同步到所有个人节点）
    isSelf
  };
}
