// 免责声明同意状态存储：默认未同意；同意后按版本号记录；版本升级后需重新同意
import { describe, expect, it, beforeEach } from 'vitest';
import {
  DISCLAIMER_VERSION,
  isDisclaimerAccepted,
  markDisclaimerAccepted
} from '../../utils/disclaimer';

describe('disclaimer', () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it('默认未同意', () => {
    expect(isDisclaimerAccepted()).toBe(false);
  });

  it('同意后为已同意', () => {
    markDisclaimerAccepted();
    expect(isDisclaimerAccepted()).toBe(true);
  });

  it('存储的是旧版本号时视为未同意（文案升级需重新确认）', () => {
    window.localStorage.setItem('spark.disclaimer.acceptedVersion', String(DISCLAIMER_VERSION - 1));
    expect(isDisclaimerAccepted()).toBe(false);
  });
});
