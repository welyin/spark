/**
 * 标签（设计 §8.2：新建/重命名/删除；删除时从所有联系人资料中摘除）。
 * 依赖 store（contactsOf/contactsApi）。
 */
import type { ContactProfile, ContactTag } from './types';
import { contactsApi, contactsOf } from './store';

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

/** 删除标签并把 tagId 从朋友与成员附加资料中全部摘除 */
export function deleteTag(spaceKey: string, tagId: string): void {
  const space = contactsOf(spaceKey);
  space.tags = space.tags.filter((item) => item.id !== tagId);
  const strip = (profile: ContactProfile) => {
    profile.tagIds = profile.tagIds.filter((id) => id !== tagId);
  };
  space.friends.forEach(strip);
  Object.values(space.memberExtras).forEach(strip);
  contactsApi()
    ?.tagDelete(spaceKey, tagId)
    .catch(() => {});
}
