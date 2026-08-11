/**
 * ai-chat 流式响应解析器单测（评审 F4）。
 *
 * 覆盖 createSSEParser / createOllamaStreamParser 的纯逻辑：
 * - 跨块断行（最后一行不完整留待拼接）
 * - 残缺 JSON（跨块拼完整后再解析）
 * - SSE [DONE] 结束哨兵不产出 token
 * - 多字节字符（中文 UTF-8）跨块不产生乱码 / 不截断
 * - accumulated 累计语义正确
 */

import { describe, expect, it } from 'vitest';
import { createOllamaStreamParser, createSSEParser } from '../service';

describe('createSSEParser', () => {
  it('单块完整行：data 提取 delta.content 并累计', () => {
    const seen: Array<{ token: string; accumulated: string }> = [];
    const push = createSSEParser((token, accumulated) => seen.push({ token, accumulated }));
    push('data: {"choices":[{"delta":{"content":"你"}}]}\n');
    push('data: {"choices":[{"delta":{"content":"好"}}]}\n');
    push('data: [DONE]\n');
    expect(seen).toEqual([
      { token: '你', accumulated: '你' },
      { token: '好', accumulated: '你好' }
    ]);
  });

  it('跨块断行：最后一行不完整留待拼接', () => {
    const seen: Array<{ token: string; accumulated: string }> = [];
    const push = createSSEParser((token, accumulated) => seen.push({ token, accumulated }));
    // "data: {...}\n" 被切成两段跨块到达
    push('data: {"choi');
    push('ces":[{"delta":{"content":"hi"}}]}\n');
    push('data: {"choices":[{"delta":{"content":"!"}}]}\n');
    expect(seen).toEqual([
      { token: 'hi', accumulated: 'hi' },
      { token: '!', accumulated: 'hi!' }
    ]);
  });

  it('[DONE] 哨兵不产出 token，且不打断后续（若有）', () => {
    const seen: Array<{ token: string; accumulated: string }> = [];
    const push = createSSEParser((token, accumulated) => seen.push({ token, accumulated }));
    push('data: [DONE]\n');
    push('data: {"choices":[{"delta":{"content":"x"}}]}\n');
    expect(seen).toEqual([{ token: 'x', accumulated: 'x' }]);
  });

  it('非 data 行 / 空行 / 非 JSON data 一律跳过', () => {
    const seen: Array<{ token: string; accumulated: string }> = [];
    const push = createSSEParser((token, accumulated) => seen.push({ token, accumulated }));
    push('event: message\n');
    push('data: not-json\n');
    push('\n');
    push(': comment\n');
    expect(seen).toEqual([]);
  });

  it('残缺 JSON 跨块：拼完整后再解析，不产生 token 也不抛错', () => {
    const seen: Array<{ token: string; accumulated: string }> = [];
    const push = createSSEParser((token, accumulated) => seen.push({ token, accumulated }));
    // 第一段是不完整 JSON（无闭括号），第二段补全
    push('data: {"choices":[{"delta":{"content":"中"');
    push('}}]}\n');
    push('data: {"choices":[{"delta":{"content":"文"}}]}\n');
    expect(seen).toEqual([
      { token: '中', accumulated: '中' },
      { token: '文', accumulated: '中文' }
    ]);
  });
});

describe('createOllamaStreamParser', () => {
  it('单块完整行：每行一个 JSON 对象', () => {
    const seen: Array<{ token: string; accumulated: string }> = [];
    const push = createOllamaStreamParser((token, accumulated) => seen.push({ token, accumulated }));
    push('{"message":{"content":"a"}}\n');
    push('{"message":{"content":"b"}}\n');
    push('{"done":true}\n');
    expect(seen).toEqual([
      { token: 'a', accumulated: 'a' },
      { token: 'b', accumulated: 'ab' }
    ]);
  });

  it('跨块断行 + 残缺 JSON 拼接', () => {
    const seen: Array<{ token: string; accumulated: string }> = [];
    const push = createOllamaStreamParser((token, accumulated) => seen.push({ token, accumulated }));
    push('{"message":{');
    push('"content":"你好"}}\n');
    push('{"message":{"content":"!"}}\n');
    expect(seen).toEqual([
      { token: '你好', accumulated: '你好' },
      { token: '!', accumulated: '你好!' }
    ]);
  });

  it('多字节中文跨块不截断不产生乱码', () => {
    const seen: Array<{ token: string; accumulated: string }> = [];
    const push = createOllamaStreamParser((token, accumulated) => seen.push({ token, accumulated }));
    // 中文在 JSON 里以转义/或字面量形式；字面量 UTF-8 多字节跨块由外层
    // 字节缓冲保证（见 core/sys.rs），解析器侧只需保证 JSON 结构跨块拼接正确
    push('{"message":{"content":"');
    push('你好"}}\n');
    expect(seen).toEqual([{ token: '你好', accumulated: '你好' }]);
  });

  it('空行跳过，残缺非法行不抛错', () => {
    const seen: Array<{ token: string; accumulated: string }> = [];
    const push = createOllamaStreamParser((token, accumulated) => seen.push({ token, accumulated }));
    push('\n\n');
    push('garbage\n');
    push('{"message":{"content":"ok"}}\n');
    expect(seen).toEqual([{ token: 'ok', accumulated: 'ok' }]);
  });
});
