/**
 * 标签（设计 §8.2：新建/重命名/删除；删除时从所有联系人资料中摘除）。
 * 依赖 store（contactsOf/contactsApi）与 queries（profileOf/updateProfile）。
 */
import type { ContactProfile, ContactTag } from './types';
import { contactsApi, contactsOf } from './store';
import { profileOf, updateProfile } from './queries';

export function createTag(spaceKey: string, name: string): ContactTag {
  const space = contactsOf(spaceKey);
  const tag: ContactTag = { id: `tag-${Date.now()}-${space.tags.length}`, name };
  space.tags.push(tag);
  contactsApi()
    ?.tagCreate(spaceKey, tag.id, name)
    .catch(() => {});
  return tag;
}

export function renameTag(spaceKey: string, tagId: string, name: string): void {
  const tag = contactsOf(spaceKey).tags.find((item) => item.id === tagId);
  if (tag) {
    tag.name = name;
    contactsApi()
      ?.tagRename(spaceKey, tagId, name)
      .catch(() => {});
  }
}

/**
 * 确保插件联系人携带"以插件显示名命名"的标签（标签不存在则创建）。
 * 由桥 dispatcher 在 registerAsContact 成功且通讯录刷新完成后调用，
 * 用于标识联系人来源于哪个插件；profile.tagIds 的变更经 updateProfile
 * 持久化（函数级直写内核，只写脏项），标签本体经 tagCreate 落内核。
 */
export function ensurePluginContactTag(spaceKey: string, pluginName: string, contactId: string): void {
  const space = contactsOf(spaceKey);
  let tag = space.tags.find((item) => item.name === pluginName);
  if (!tag) {
    tag = createTag(spaceKey, pluginName);
  }
  const profile = profileOf(spaceKey, contactId);
  if (!profile.tagIds.includes(tag.id)) {
    updateProfile(spaceKey, contactId, { tagIds: [...profile.tagIds, tag.id] });
  }
}

/** 删除标签并把 tagId 从朋友与成员附加资料中全部摘除（逐脏项经 updateProfile 持久化） */
export function deleteTag(spaceKey: string, tagId: string): void {
  const space = contactsOf(spaceKey);
  space.tags = space.tags.filter((item) => item.id !== tagId);
  const strip = (rootId: string, profile: ContactProfile) => {
    if (profile.tagIds.includes(tagId)) {
      updateProfile(spaceKey, rootId, { tagIds: profile.tagIds.filter((id) => id !== tagId) });
    }
  };
  space.friends.forEach((friend) => strip(friend.rootId, friend));
  Object.entries(space.memberExtras).forEach(([rootId, profile]) => strip(rootId, profile));
  contactsApi()
    ?.tagDelete(spaceKey, tagId)
    .catch(() => {});
}
