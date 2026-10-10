import { describe, expect, it } from 'vitest';
import { parseArgs } from '../src/cli.js';

describe('parseArgs', () => {
  it('键值旗取下一个参数为值', () => {
    expect(parseArgs(['build', '--pluginId', 'spark-moments', '--mode', 'app'])).toEqual({
      _: ['build'],
      pluginId: 'spark-moments',
      mode: 'app'
    });
  });

  it('布尔旗 --help 不吞掉下一个参数（A33 评审 S3）', () => {
    expect(parseArgs(['--help', 'build'])).toEqual({ _: ['build'], help: true });
  });

  it('键值旗后随另一旗时不误吞为值', () => {
    expect(parseArgs(['build', '--mode', '--pluginId', 'x'])).toEqual({
      _: ['build'],
      mode: undefined,
      pluginId: 'x'
    });
  });

  it('末尾无值的键值旗置 undefined', () => {
    expect(parseArgs(['verify', '--dir'])).toEqual({ _: ['verify'], dir: undefined });
  });
});
