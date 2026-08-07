/**
 * 顶栏「+」菜单添加请求 store（移动端第二批打磨）：
 * request/consume 成对语义——请求可被消费一次，消费后置空。
 */
import { describe, expect, it } from 'vitest';
import { consumePendingAddContact, requestAddContact } from '../../stores/pending-add-contact';

describe('pending-add-contact 添加请求', () => {
  it('request 后可被 consume 消费一次', () => {
    requestAddContact('friend');
    expect(consumePendingAddContact()).toBe('friend');
    // 消费后置空
    expect(consumePendingAddContact()).toBeNull();
  });

  it('无请求时 consume 返回 null', () => {
    expect(consumePendingAddContact()).toBeNull();
  });

  it('后写请求覆盖先写请求（取最近一次意图）', () => {
    requestAddContact('friend');
    requestAddContact('member');
    expect(consumePendingAddContact()).toBe('member');
  });
});
