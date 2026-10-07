/**
 * 后端地址传输加固与机密字段剥离单测（评审 H1 · R1.5 / R1.1）。
 *
 * 覆盖 validateBackendUrl / stripSecretFields 的纯逻辑：
 * - https 一律放行
 * - http 仅回环地址（localhost/127.0.0.1/::1）放行，其余拒绝
 * - 空串 / 非法 URL 拒绝
 * - stripSecretFields 剥离 apiKey 且不改动原对象
 */

import { describe, expect, it } from 'vitest';
import { stripSecretFields, validateBackendUrl } from '../model';

describe('validateBackendUrl', () => {
  it('https 放行', () => {
    expect(validateBackendUrl('https://api.openai.com/v1')).toEqual({ ok: true });
    expect(validateBackendUrl(' https://api.example.com:8443/v1 ')).toEqual({ ok: true });
  });

  it('http 回环地址放行（本机 Ollama 等场景）', () => {
    expect(validateBackendUrl('http://localhost:11434')).toEqual({ ok: true });
    expect(validateBackendUrl('http://127.0.0.1:8080/v1')).toEqual({ ok: true });
    expect(validateBackendUrl('http://[::1]:9000/v1')).toEqual({ ok: true });
  });

  it('http 非回环地址拒绝（Bearer key 不走明文链路）', () => {
    for (const url of [
      'http://api.openai.com/v1',
      'http://192.168.1.10:8080/v1',
      'http://10.0.0.2/v1',
      'http://example.com',
    ]) {
      const r = validateBackendUrl(url);
      expect(r.ok).toBe(false);
      if (!r.ok) expect(r.reason).toContain('https');
    }
  });

  it('空串 / 非法 URL / 其他协议拒绝', () => {
    expect(validateBackendUrl('').ok).toBe(false);
    expect(validateBackendUrl('   ').ok).toBe(false);
    expect(validateBackendUrl('not-a-url').ok).toBe(false);
    expect(validateBackendUrl('ftp://example.com').ok).toBe(false);
  });
});

describe('stripSecretFields', () => {
  it('剥离 apiKey，保留其余字段', () => {
    expect(
      stripSecretFields({ baseUrl: 'https://a/v1', apiKey: 'sk-x', model: 'gpt-4o' }),
    ).toEqual({ baseUrl: 'https://a/v1', model: 'gpt-4o' });
  });

  it('无 apiKey 时原样返回字段集，且不改动入参对象', () => {
    const cfg = { baseUrl: 'https://a/v1' };
    const out = stripSecretFields(cfg);
    expect(out).toEqual({ baseUrl: 'https://a/v1' });
    expect(cfg).toEqual({ baseUrl: 'https://a/v1' });
    expect(out).not.toBe(cfg);
  });

  it('含 apiKey 的入参对象本身不被修改', () => {
    const cfg = { apiKey: 'sk-x', baseUrl: 'https://a/v1' };
    stripSecretFields(cfg);
    expect(cfg.apiKey).toBe('sk-x');
  });
});
