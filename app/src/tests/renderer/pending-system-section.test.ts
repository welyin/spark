/**
 * 系统设置深链请求 store（Android 前端改造）：
 * request/consume 成对语义——请求可被消费一次，消费后置空。
 */
import { describe, expect, it } from 'vitest';
import {
  consumePendingSystemSection,
  requestOpenSystemSection
} from '../../stores/pending-system-section';

describe('pending-system-section 深链请求', () => {
  it('request 后可被 consume 消费一次', () => {
    requestOpenSystemSection('netStatus');
    expect(consumePendingSystemSection()).toBe('netStatus');
    // 消费后置空
    expect(consumePendingSystemSection()).toBeNull();
  });

  it('无请求时 consume 返回 null', () => {
    expect(consumePendingSystemSection()).toBeNull();
  });

  it('后写请求覆盖先写请求（取最近一次意图）', () => {
    requestOpenSystemSection('general');
    requestOpenSystemSection('netStatus');
    expect(consumePendingSystemSection()).toBe('netStatus');
  });
});
