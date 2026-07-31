/**
 * HTTP 代理设置纯逻辑单测（proxy-settings.ts）：
 * splitProxy 拆分（IPv4/域名/[IPv6]/异常输入）、joinProxy 拼接、端口即时约束。
 */
import { describe, expect, it } from 'vitest';
import { isPortPlausible, joinProxy, splitProxy } from './proxy-settings';

describe('splitProxy', () => {
  it('未设置返回 null', () => {
    expect(splitProxy(null)).toBeNull();
    expect(splitProxy('')).toBeNull();
    expect(splitProxy('   ')).toBeNull();
  });

  it('IPv4 与域名拆分', () => {
    expect(splitProxy('127.0.0.1:29290')).toEqual({ host: '127.0.0.1', port: '29290' });
    expect(splitProxy('proxy.example.com:443')).toEqual({ host: 'proxy.example.com', port: '443' });
  });

  it('[IPv6]:port 拆分保留方括号', () => {
    expect(splitProxy('[::1]:8080')).toEqual({ host: '[::1]', port: '8080' });
  });

  it('无法拆分返回 null', () => {
    expect(splitProxy('127.0.0.1')).toBeNull();
    expect(splitProxy(':8080')).toBeNull();
    expect(splitProxy('127.0.0.1:')).toBeNull();
  });
});

describe('joinProxy', () => {
  it('拼接并容忍首尾空白', () => {
    expect(joinProxy('127.0.0.1', '29290')).toBe('127.0.0.1:29290');
    expect(joinProxy(' 127.0.0.1 ', ' 29290 ')).toBe('127.0.0.1:29290');
  });

  it('任一为空视为关闭（空串）', () => {
    expect(joinProxy('', '29290')).toBe('');
    expect(joinProxy('127.0.0.1', '')).toBe('');
    expect(joinProxy('', '')).toBe('');
  });
});

describe('isPortPlausible', () => {
  it('接受 1-65535 纯数字', () => {
    expect(isPortPlausible('1')).toBe(true);
    expect(isPortPlausible('29290')).toBe(true);
    expect(isPortPlausible('65535')).toBe(true);
  });

  it('拒绝越界与非数字', () => {
    expect(isPortPlausible('0')).toBe(false);
    expect(isPortPlausible('65536')).toBe(false);
    expect(isPortPlausible('abc')).toBe(false);
    expect(isPortPlausible('')).toBe(false);
    expect(isPortPlausible('-1')).toBe(false);
  });
});
