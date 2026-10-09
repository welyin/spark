import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  assertValidBranchName,
  createBundleBase64,
  detectGitCli,
  detectShell,
  isAncestorCli,
  listLocalBranches,
  listReachableObjects,
  materializeRepo,
  pickDefaultBranch,
  readFileBase64,
  readObjectBase64,
  resetShellCacheForTest,
  shQuote,
  writeFileBase64,
  type ExecFn
} from '../git';

type Call = { program: string; args: string[]; workdir?: string };

/** 可编程 mock exec：按 (program, 首参) 路由，记录全部调用 */
function mockExec(routes: Array<{ match: (program: string, args: string[]) => boolean; stdout?: string; stderr?: string; exitCode?: number }>) {
  const calls: Call[] = [];
  const exec: ExecFn = async (program, args, workdir) => {
    calls.push({ program, args, workdir });
    const route = routes.find((r) => r.match(program, args));
    if (!route) {
      return { stdout: '', stderr: `unmocked: ${program} ${args.join(' ')}`, exitCode: 127 };
    }
    return { stdout: route.stdout ?? '', stderr: route.stderr ?? '', exitCode: route.exitCode ?? 0 };
  };
  return { exec, calls };
}

const SH_PROBE = { match: (p: string, a: string[]) => p === 'sh' && a[0] === '-c' && a[1] === 'echo spark-probe', stdout: 'spark-probe\n' };

beforeEach(() => {
  resetShellCacheForTest();
});

describe('detectGitCli', () => {
  it('解析版本号', async () => {
    const { exec } = mockExec([{ match: (p) => p === 'git', stdout: 'git version 2.43.0.windows.1\n' }]);
    expect(await detectGitCli(exec)).toEqual({ available: true, version: '2.43.0.windows.1' });
  });

  it('非零退出 / 抛错 → unavailable（浏览不受影响）', async () => {
    const { exec: e1 } = mockExec([{ match: () => true, exitCode: 1 }]);
    expect((await detectGitCli(e1)).available).toBe(false);
    const failing: ExecFn = async () => {
      throw new Error('spawn git ENOENT');
    };
    expect((await detectGitCli(failing)).available).toBe(false);
  });
});

describe('listLocalBranches / pickDefaultBranch', () => {
  it('for-each-ref 输出解析', async () => {
    const { exec } = mockExec([
      { match: (p, a) => p === 'git' && a[2] === 'for-each-ref', stdout: `${'a'.repeat(40)} main\n${'b'.repeat(40)} dev\nnotasha broken\n` }
    ]);
    const branches = await listLocalBranches(exec, '/repo');
    expect(branches).toEqual([
      { head: 'a'.repeat(40), name: 'main' },
      { head: 'b'.repeat(40), name: 'dev' }
    ]);
    expect(pickDefaultBranch(branches)).toBe('main');
    expect(pickDefaultBranch([{ name: 'dev' }])).toBe('dev');
    expect(pickDefaultBranch([])).toBeNull();
  });
});

describe('listReachableObjects（发布分支可达集，评审 A28 收窄口径）', () => {
  it('rev-list --objects + 临时文件 batch-check；非法行/非 hex 跳过，不走 --batch-all-objects', async () => {
    const { exec, calls } = mockExec([
      SH_PROBE,
      { match: (p, a) => p === 'git' && a[2] === 'rev-list', stdout: `${'a'.repeat(40)}\n${'b'.repeat(40)} src/x.ts\ngarbage-line\n` },
      { match: (p, a) => p === 'sh' && a[1]?.includes('base64 -d'), stdout: '' },
      { match: (p, a) => p === 'sh' && a[1]?.includes('--batch-check'), stdout: `${'a'.repeat(40)} commit\n${'b'.repeat(40)} blob\n` },
      { match: (p, a) => p === 'sh' && a[1]?.includes('rm -f'), stdout: '' }
    ]);
    const objects = await listReachableObjects(exec, '/repo', ['a'.repeat(40)]);
    expect(objects).toEqual([
      { sha: 'a'.repeat(40), type: 'commit' },
      { sha: 'b'.repeat(40), type: 'blob' }
    ]);
    expect(calls.some((c) => c.args[2] === 'rev-list' && c.args.includes('--objects'))).toBe(true);
    expect(calls.some((c) => c.args.includes('--batch-all-objects'))).toBe(false);
    // sha 清单临时文件用完即清
    expect(calls.some((c) => c.args[1]?.includes('spark-objlist-') && c.args[1]?.includes('rm -f'))).toBe(true);
  });

  it('head 清单为空 / 全非 hex → 不调 git，返回空', async () => {
    const { exec, calls } = mockExec([SH_PROBE]);
    expect(await listReachableObjects(exec, '/repo', [])).toEqual([]);
    expect(await listReachableObjects(exec, '/repo', ['not-a-sha'])).toEqual([]);
    expect(calls.filter((c) => c.program === 'git')).toHaveLength(0);
  });
});

describe('assertValidBranchName（option-injection 防御）', () => {
  it('拒 `-` 开头 / `..` / 空格 / `@{` / .lock 结尾；放过正常分支名与 HEAD', () => {
    for (const bad of ['-x', '--upload-pack=evil', 'a..b', 'a b', 'a@{0}', 'main.lock', '', '/main', 'main/']) {
      expect(() => assertValidBranchName(bad)).toThrow('线形非法');
    }
    for (const good of ['main', 'feature/x', 'dev-1', 'HEAD', 'refs/spark-pr/abc123']) {
      expect(() => assertValidBranchName(good)).not.toThrow();
    }
  });

  it('readObjectBase64：非 hex sha 在 exec 前拒绝', async () => {
    const { exec, calls } = mockExec([SH_PROBE]);
    await expect(readObjectBase64(exec, '/repo', 'x;rm -rf /', 'blob')).rejects.toThrow('线形非法');
    expect(calls.filter((c) => c.program !== 'sh')).toHaveLength(0);
  });
});

describe('shQuote', () => {
  it('单引号转义', () => {
    expect(shQuote(`/tmp/a'b`)).toBe(`'/tmp/a'\\''b'`);
    expect(shQuote('/plain/path')).toBe(`'/plain/path'`);
  });
});

describe('shell 探测与 base64 文件往返', () => {
  it('sh 优先；写文件分块追加', async () => {
    const { exec, calls } = mockExec([
      SH_PROBE,
      { match: (p, a) => p === 'sh' && a[1]?.includes('base64 -d'), stdout: '' }
    ]);
    expect(await detectShell(exec)).toBe('sh');
    await writeFileBase64(exec, '/tmp/x.bin', 'q'.repeat(13000));
    const writes = calls.filter((c) => c.args[1]?.includes('base64 -d'));
    // 13000 / 6000 → 3 块；首块覆盖写，后续追加
    expect(writes).toHaveLength(3);
    expect(writes[0].args[1]).toContain(' > ');
    expect(writes[1].args[1]).toContain(' >> ');
  });

  it('sh 缺席回退 PowerShell', async () => {
    const { exec, calls } = mockExec([
      { match: (p) => p === 'sh', exitCode: 127, stderr: 'not found' },
      { match: (p) => p === 'powershell', stdout: 'spark-probe\r\n' }
    ]);
    expect(await detectShell(exec)).toBe('powershell');
    const { exec: e2, calls: c2 } = mockExec([
      { match: (p) => p === 'sh', exitCode: 127 },
      { match: (p) => p === 'powershell', stdout: 'spark-probe' }
    ]);
    resetShellCacheForTest();
    await writeFileBase64(e2, 'C:\\tmp\\x.bin', 'AAAA');
    expect(c2.some((c) => c.program === 'powershell' && c.args[2]?.includes('WriteAllBytes'))).toBe(true);
    void calls; void vi;
  });

  it('两个 shell 都不可用 → 明确报错（不静默失败）', async () => {
    const { exec } = mockExec([{ match: () => true, exitCode: 127 }]);
    await expect(detectShell(exec)).rejects.toThrow('二进制往返无法进行');
  });

  it('readFileBase64 走 stdout 文本通道', async () => {
    const { exec } = mockExec([SH_PROBE, { match: (p, a) => p === 'sh' && a[1]?.includes('base64 -w0'), stdout: 'aGVsbG8=\n' }]);
    expect(await readFileBase64(exec, '/tmp/x')).toBe('aGVsbG8=');
  });
});

describe('createBundleBase64', () => {
  it('bundle create → base64 读取 → 临时文件清理', async () => {
    const { exec, calls } = mockExec([
      SH_PROBE,
      { match: (p, a) => p === 'git' && a[2] === 'bundle', stdout: '' },
      { match: (p, a) => p === 'sh' && a[1]?.includes('base64 -w0'), stdout: 'QlVORExF' },
      { match: (p, a) => p === 'sh' && a[1]?.includes('rm -f'), stdout: '' }
    ]);
    const base64 = await createBundleBase64(exec, '/repo', 'a'.repeat(40), 'feature', '/repo/.git/x.bundle');
    expect(base64).toBe('QlVORExF');
    const bundle = calls.find((c) => c.args[2] === 'bundle');
    expect(bundle?.args.slice(0, 5)).toEqual(['-C', '/repo', 'bundle', 'create', '/repo/.git/x.bundle']);
    expect(bundle?.args[5]).toBe(`${'a'.repeat(40)}..feature`);
    expect(calls.some((c) => c.args[1]?.includes('rm -f'))).toBe(true);
  });
});

describe('isAncestorCli', () => {
  it('exit 0 = 祖先；exit 1 = 否；其他 = 抛错', async () => {
    const { exec: yes } = mockExec([{ match: () => true, exitCode: 0 }]);
    expect(await isAncestorCli(yes, '/r', 'a'.repeat(40), 'b'.repeat(40))).toBe(true);
    const { exec: no } = mockExec([{ match: () => true, exitCode: 1 }]);
    expect(await isAncestorCli(no, '/r', 'a'.repeat(40), 'b'.repeat(40))).toBe(false);
    const { exec: bad } = mockExec([{ match: () => true, exitCode: 128, stderr: 'bad rev' }]);
    await expect(isAncestorCli(bad, '/r', 'x', 'y')).rejects.toThrow('祖先判定失败');
  });
});

describe('materializeRepo', () => {
  const EMPTY_DIR = { match: (p: string, a: string[]) => p === 'sh' && a[1]?.includes('ls -A'), stdout: '' };

  it('init → 逐对象 hash-object → update-ref → HEAD → checkout', async () => {
    const routes = [
      SH_PROBE,
      EMPTY_DIR,
      { match: (p, a) => p === 'sh' && a[1]?.includes('base64 -d'), stdout: '' },
      { match: (p, a) => p === 'sh' && a[1]?.includes('rm -f'), stdout: '' },
      { match: (p) => p === 'git', stdout: '' }
    ];
    const { exec, calls } = mockExec(routes);
    await materializeRepo(
      exec,
      '/target',
      [{ sha: 'a'.repeat(40), type: 'commit', base64: 'AAAA' }],
      [{ name: 'main', head: 'a'.repeat(40) }],
      'main'
    );
    const gitArgs = calls.filter((c) => c.program === 'git').map((c) => c.args.slice(2));
    expect(gitArgs[0]).toEqual(['init']);
    expect(gitArgs).toContainEqual(['hash-object', '-w', '-t', 'commit', '--', `/target/.git/spark-mirror-tmp-${'a'.repeat(40)}`]);
    expect(gitArgs).toContainEqual(['update-ref', 'refs/heads/main', 'a'.repeat(40)]);
    expect(gitArgs).toContainEqual(['symbolic-ref', 'HEAD', 'refs/heads/main']);
    expect(gitArgs).toContainEqual(['checkout', '-f', 'main']);
  });

  it('目标目录非空 / 不存在 → 拒绝且不会执行 git init（空目录护栏）', async () => {
    const { exec, calls } = mockExec([
      SH_PROBE,
      { match: (p, a) => p === 'sh' && a[1]?.includes('ls -A'), exitCode: 1 },
      { match: () => true, stdout: '' }
    ]);
    await expect(materializeRepo(exec, '/t', [], [{ name: 'main', head: 'a'.repeat(40) }], 'main')).rejects.toThrow('空目录');
    expect(calls.filter((c) => c.program === 'git')).toHaveLength(0);
  });

  it('hash-object 失败 → 上抛且残留临时文件兜底清理', async () => {
    const { exec, calls } = mockExec([
      SH_PROBE,
      EMPTY_DIR,
      { match: (p, a) => p === 'sh' && a[1]?.includes('base64 -d'), stdout: '' },
      { match: (p, a) => p === 'sh' && a[1]?.includes('rm -f'), stdout: '' },
      { match: (p, a) => p === 'git' && a[2] === 'hash-object', exitCode: 128, stderr: 'corrupt' },
      { match: (p) => p === 'git', stdout: '' }
    ]);
    await expect(
      materializeRepo(exec, '/t', [{ sha: 'b'.repeat(40), type: 'blob', base64: 'AA==' }], [], 'main')
    ).rejects.toThrow('写入对象');
    expect(calls.some((c) => c.args[1]?.includes('spark-mirror-tmp-*'))).toBe(true);
  });

  it('PowerShell 环境：兜底清理走 Remove-Item 分支（不再残留 spark-mirror-tmp-*）', async () => {
    const { exec, calls } = mockExec([
      { match: (p) => p === 'sh', exitCode: 127, stderr: 'not found' },
      { match: (p, a) => p === 'powershell' && a[2] === 'Write-Output spark-probe', stdout: 'spark-probe\r\n' },
      { match: (p) => p === 'powershell', stdout: '' },
      { match: (p, a) => p === 'git' && a[2] === 'hash-object', exitCode: 128, stderr: 'corrupt' },
      { match: (p) => p === 'git', stdout: '' }
    ]);
    await expect(
      materializeRepo(exec, 'C:\\t', [{ sha: 'b'.repeat(40), type: 'blob', base64: 'AA==' }], [], 'main')
    ).rejects.toThrow('写入对象');
    expect(calls.some((c) => c.program === 'powershell' && c.args[2]?.includes('Remove-Item') && c.args[2]?.includes('spark-mirror-tmp-*'))).toBe(true);
  });
});
