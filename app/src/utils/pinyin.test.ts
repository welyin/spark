// utils/pinyin 单测：首字母分桶的空串/英文/汉字/非汉字/'#' 兜底与分组键排序
import { describe, it, expect } from 'vitest';
import { compareLetters, compareNames, firstLetter } from './pinyin';

describe('firstLetter', () => {
  it('空串与纯空白归 #', () => {
    expect(firstLetter('')).toBe('#');
    expect(firstLetter('   ')).toBe('#');
  });

  it('英文字母取大写（大小写不敏感）', () => {
    expect(firstLetter('alice')).toBe('A');
    expect(firstLetter('Bob')).toBe('B');
    expect(firstLetter('zoe')).toBe('Z');
  });

  it('常见汉字按映射表归组', () => {
    expect(firstLetter('陈静')).toBe('C');
    expect(firstLetter('王')).toBe('W');
    expect(firstLetter('张三')).toBe('Z');
    expect(firstLetter('阿斯蒂')).toBe('A');
  });

  it('数字/符号/表外生僻字归 #', () => {
    expect(firstLetter('13812345678')).toBe('#');
    expect(firstLetter('!demo')).toBe('#');
    expect(firstLetter('龘')).toBe('#');
  });

  it('忽略前导空白后取首个字符', () => {
    expect(firstLetter('  陈静')).toBe('C');
  });
});

describe('compareLetters', () => {
  it('字母按 A..Z 排序', () => {
    expect(compareLetters('A', 'B')).toBeLessThan(0);
    expect(compareLetters('B', 'A')).toBeGreaterThan(0);
    expect(compareLetters('M', 'M')).toBe(0);
  });

  it('# 恒排最后', () => {
    expect(compareLetters('#', 'A')).toBeGreaterThan(0);
    expect(compareLetters('Z', '#')).toBeLessThan(0);
    expect(compareLetters('#', '#')).toBe(0);
  });
});

describe('compareNames', () => {
  it('按拼音序比较汉字（阿 < 包 < 陈）', () => {
    expect(compareNames('阿', '包')).toBeLessThan(0);
    expect(compareNames('包', '陈')).toBeLessThan(0);
    expect(compareNames('陈', '阿')).toBeGreaterThan(0);
  });

  it('同名返回 0，且可用于稳定排序', () => {
    expect(compareNames('陈静', '陈静')).toBe(0);
    const sorted = ['王五', '阿斯蒂', '陈静'].sort(compareNames);
    expect(sorted).toEqual(['阿斯蒂', '陈静', '王五']);
  });
});
