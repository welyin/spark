/**
 * 个人空间资料（stores/personal-space）回归：
 * 缺省名「个人空间」、自定义名字/logo 生效、空名恢复缺省、localStorage 持久化与重启恢复。
 */
import { beforeEach, describe, expect, it } from 'vitest';
import {
  DEFAULT_PERSONAL_SPACE_NAME,
  personalSpaceLogo,
  personalSpaceName,
  savePersonalSpaceProfile
} from '../../stores/personal-space';

describe('personal-space 个人空间资料', () => {
  beforeEach(() => {
    localStorage.clear();
    savePersonalSpaceProfile('', '');
  });

  it('缺省：名字为「个人空间」、logo 为空', () => {
    expect(personalSpaceName.value).toBe(DEFAULT_PERSONAL_SPACE_NAME);
    expect(personalSpaceLogo.value).toBe('');
  });

  it('设置名字与 logo 即时生效并持久化', () => {
    savePersonalSpaceProfile('小明的地盘', 'data:image/png;base64,xx');
    expect(personalSpaceName.value).toBe('小明的地盘');
    expect(personalSpaceLogo.value).toBe('data:image/png;base64,xx');
    const raw = JSON.parse(localStorage.getItem('spark:personal-space-profile')!);
    expect(raw.name).toBe('小明的地盘');
    expect(raw.logo).toBe('data:image/png;base64,xx');
  });

  it('名字留白（含纯空格）恢复缺省名', () => {
    savePersonalSpaceProfile('自定义', '');
    savePersonalSpaceProfile('   ', '');
    expect(personalSpaceName.value).toBe(DEFAULT_PERSONAL_SPACE_NAME);
  });
});
