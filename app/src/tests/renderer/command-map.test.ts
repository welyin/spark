// command-map 完备性静态断言（R1 review 修复）：
// api/index.ts 里所有经 call('channel') 直连内核的 channel 必须在
// COMMAND_MAP（kebab→snake 命令名）中有条目；带位置参数的调用还必须在
// ARG_NAMES 中有等长参数名表。
// 缺映射时 call() 静默回退为「channel 原样当命令名 + 空 payload」，真实
// 壳下 Tauri 报 command not found 必 reject——contact-reply-request 曾因此
// 完全失效，而单测 stub 在适配层之上恰好察觉不到（R1 Blocker）。
// 提取用配平括号扫描（不用单行正则——多行调用/复杂实参会静默漏配，
// R3 review 实测 86 个 channel 漏 8 个），末条断言自检提取数，
// 漏配直接失败而不是假绿。
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { ARG_NAMES, COMMAND_MAP } from '../../api/command-map';

// vitest 以 app/ 为 cwd 运行；import.meta.url 经 transform 后不是可靠文件路径
const INDEX_SRC = resolve(process.cwd(), 'src/api/index.ts');

/**
 * 从 index.ts 源码提取全部 call('channel', ...) 的 channel 与顶层位置实参数。
 * 配平括号扫描：定位 `call('` 后逐字符推进，字符串/模板串跳过，括号配平
 * 找到调用终点，再按顶层逗号数实参个数（嵌套括号内的逗号不计）。
 */
function extractCalls(src: string): Array<{ channel: string; argc: number }> {
  const calls: Array<{ channel: string; argc: number }> = [];
  const marker = /call\(\s*'([^']+)'/g;
  for (const m of src.matchAll(marker)) {
    const channel = m[1];
    let i = m.index + m[0].length;
    // 扫描到配平的右括号；顶层逗号数即位置实参数（channel 后的第一个逗号
    // 正是 channel 与首个实参的分隔，n 个实参恰有 n 个顶层逗号）
    let depth = 1;
    let commas = 0;
    let sawArg = false;
    while (i < src.length && depth > 0) {
      const ch = src[i];
      if (ch === "'" || ch === '"' || ch === '`') {
        // 跳过字符串字面量（处理转义）
        i++;
        while (i < src.length && src[i] !== ch) {
          i += src[i] === '\\' ? 2 : 1;
        }
        i++;
        continue;
      }
      if (ch === '(') depth++;
      else if (ch === ')') depth--;
      else if (ch === ',' && depth === 1) commas++;
      else if (depth === 1 && !/\s/.test(ch)) sawArg = true;
      i++;
    }
    if (depth !== 0) {
      throw new Error(`call('${channel}' 括号未配平，提取失败（不允许静默漏配）`);
    }
    calls.push({ channel, argc: sawArg ? commas : 0 });
  }
  return calls;
}

describe('command-map 完备性', () => {
  it('index.ts 中所有 call() 直连 channel 均已注册命令名，带参调用参数名等长', () => {
    const src = readFileSync(INDEX_SRC, 'utf8');
    const calls = extractCalls(src);
    // 自检：提取数必须等于源码中 call(' 出现数（防提取器静默漏配）
    const occurrences = (src.match(/call\(\s*'/g) ?? []).length;
    expect(calls.length, '提取数 ≠ call( 出现数，提取器漏配').toBe(occurrences);
    expect(calls.length).toBeGreaterThan(0);
    for (const { channel, argc } of new Map(calls.map((c) => [c.channel, c])).values()) {
      expect(COMMAND_MAP[channel], `COMMAND_MAP 缺 ${channel}`).toBeTypeOf('string');
      if (argc === 0) {
        continue; // 零参调用空 payload 即正确，ARG_NAMES 可缺省
      }
      expect(
        ARG_NAMES[channel]?.length,
        `${channel}: ARG_NAMES ${ARG_NAMES[channel]?.length ?? '缺'} ≠ call() 实参 ${argc} 个`
      ).toBe(argc);
    }
  });
});
