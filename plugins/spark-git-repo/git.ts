/**
 * 代码仓库应用（spark-git-repo）· 本地 git CLI 集成层（桌面）。
 *
 * 能力口径（git-repo.md §4）：桌面经 sdk.sys.exec 调本地 git CLI（高危权限
 * system:exec，安装授权时明示）；git CLI 缺失时如实降级——浏览可用（纯 JS
 * 解析镜像对象，不经 CLI），PR 发起 / 合并 / 物化工作区给安装指引，不静默失败。
 *
 * 约束与对策（内核 sys.exec 无 stdin、插件无文件写面）：
 * - git 对象 / bundle 等二进制内容一律经 shell 管道 base64 过 stdout（先 `sh -c`，
 *   回退 PowerShell）——sys.exec stdout 是 UTF-8 文本，裸二进制会损坏；
 * - 临时文件落在目标仓库 .git 下（spark-mirror-tmp-*），用完即清；
 * - 本层全部函数注入 ExecFn，不直接碰 sdk——测试以 mock exec 钉住命令线形。
 */

import type { GitObjectType } from './model';

export type ExecResult = { stdout: string; stderr: string; exitCode: number };
export type ExecFn = (program: string, args: string[], workdir?: string) => Promise<ExecResult>;

// ------------------------------------------------------------------
// git CLI 检测与基础查询
// ------------------------------------------------------------------

export type GitCliStatus = { available: boolean; version: string | null };

/** 检测本地 git CLI（git --version）；不可用 → available:false（浏览不受影响） */
export async function detectGitCli(exec: ExecFn): Promise<GitCliStatus> {
  try {
    const result = await exec('git', ['--version']);
    if (result.exitCode !== 0) {
      return { available: false, version: null };
    }
    const match = result.stdout.match(/git version\s+(\S+)/);
    return { available: true, version: match ? match[1] : result.stdout.trim() || null };
  } catch {
    return { available: false, version: null };
  }
}

async function mustGit(exec: ExecFn, dir: string, args: string[], what: string): Promise<string> {
  const result = await exec('git', ['-C', dir, ...args]);
  if (result.exitCode !== 0) {
    throw new Error(`${what}失败：${result.stderr.trim() || result.stdout.trim() || `exit ${result.exitCode}`}`);
  }
  return result.stdout;
}

/** git sha 线形（SHA-1 / SHA-256 仓库；拦截 shell 插值与 option 注入面） */
export function isHexSha(value: string): boolean {
  return /^[0-9a-f]{40}$/.test(value) || /^[0-9a-f]{64}$/.test(value);
}

/**
 * 分支名 / ref 线形校验（option-injection 纵深防御，git check-ref-format 的
 * 实用子集）：拒 `-` 开头（会被 git 解释为选项）、空白/控制符、`..`、`@{`、
 * `~^:?*[\` 与 `.lock` 结尾。进入 git CLI 参数的用户可控名字一律先过此关。
 */
export function assertValidBranchName(name: string): void {
  const illegal =
    name.trim() === '' ||
    name.startsWith('-') ||
    name.startsWith('/') ||
    name.endsWith('/') ||
    name.endsWith('.') ||
    name.includes('..') ||
    name.includes('@{') ||
    name.endsWith('.lock') ||
    // eslint-disable-next-line no-control-regex
    /[\x00-\x20~^:?*[\]\\]/.test(name);
  if (illegal) {
    throw new Error(`分支名 / ref 线形非法（注入防御拒绝）：${JSON.stringify(name)}`);
  }
}

/** 解析 ref 到 commit sha */
export async function resolveRef(exec: ExecFn, dir: string, ref: string): Promise<string> {
  assertValidBranchName(ref);
  return (await mustGit(exec, dir, ['rev-parse', ref], `解析 ${ref} `)).trim();
}

/** 本地全部分支与 head（`for-each-ref`；sha 与名分行解析，避免分支名特殊字符注入） */
export async function listLocalBranches(exec: ExecFn, dir: string): Promise<Array<{ name: string; head: string }>> {
  const out = await mustGit(exec, dir, ['for-each-ref', '--format=%(objectname) %(refname:short)', 'refs/heads'], '读取分支');
  return out
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const space = line.indexOf(' ');
      return { head: line.slice(0, space), name: line.slice(space + 1) };
    })
    .filter((branch) => /^[0-9a-f]{40}$/.test(branch.head) || /^[0-9a-f]{64}$/.test(branch.head));
}

/** 当前分支的 upstream/默认 base 猜测（main → master → 第一分支） */
export function pickDefaultBranch(branches: Array<{ name: string }>): string | null {
  if (branches.length === 0) {
    return null;
  }
  return branches.find((b) => b.name === 'main')?.name
    ?? branches.find((b) => b.name === 'master')?.name
    ?? branches[0].name;
}

/**
 * 枚举发布分支 head 可达的 git 对象（`rev-list --objects <heads>`；发布镜像用）。
 * 口径收窄（评审 A28 不合理项）：不再用 `cat-file --batch-all-objects` 枚举本地
 * 全部对象——不可达对象、stash、未发布的私有 ref、PR 临时 ref 均不进入分发面。
 * 对象类型经临时文件 + `cat-file --batch-check` 批量判定（sys.exec 无 stdin，
 * sha 清单经 shell 重定向喂入）。输出 sha 一律过 hex 白名单。
 */
export async function listReachableObjects(
  exec: ExecFn,
  dir: string,
  headShas: string[]
): Promise<Array<{ sha: string; type: GitObjectType }>> {
  const heads = [...new Set(headShas.filter(isHexSha))];
  if (heads.length === 0) {
    return [];
  }
  const out = await mustGit(exec, dir, ['rev-list', '--objects', ...heads], '枚举可达对象');
  const shas: string[] = [];
  const seen = new Set<string>();
  for (const line of out.split('\n')) {
    const sha = line.trim().split(' ')[0] ?? '';
    if (!isHexSha(sha) || seen.has(sha)) {
      continue;
    }
    seen.add(sha);
    shas.push(sha);
  }
  if (shas.length === 0) {
    return [];
  }
  const tmpPath = `${dir}/.git/spark-objlist-${Date.now()}.txt`;
  // sha 清单全 ASCII（hex + 换行），btoa 直接可用
  await writeFileBase64(exec, tmpPath, btoa(`${shas.join('\n')}\n`));
  try {
    const shell = await detectShell(exec);
    const batchArg = '--batch-check=%(objectname) %(objecttype)';
    const result =
      shell === 'sh'
        ? await exec('sh', ['-c', `git -C ${shQuote(dir)} cat-file ${shQuote(batchArg)} < ${shQuote(tmpPath)}`])
        : await exec('powershell', [
            '-NoProfile',
            '-Command',
            `Get-Content -LiteralPath '${tmpPath.replace(/'/g, "''")}' | git -C '${dir.replace(/'/g, "''")}' cat-file '${batchArg}'`
          ]);
    if (result.exitCode !== 0) {
      throw new Error(`对象类型批量判定失败：${result.stderr.trim() || `exit ${result.exitCode}`}`);
    }
    const objects: Array<{ sha: string; type: GitObjectType }> = [];
    for (const line of result.stdout.split('\n')) {
      const [sha, type] = line.trim().split(' ');
      if (!isHexSha(sha ?? '') || !['commit', 'tree', 'blob', 'tag'].includes(type ?? '')) {
        continue;
      }
      objects.push({ sha, type: type as GitObjectType });
    }
    return objects;
  } finally {
    await removeFile(exec, tmpPath);
  }
}

// ------------------------------------------------------------------
// shell 管道 base64（sys.exec 无 stdin / stdout 仅文本的往返对策）
// ------------------------------------------------------------------

type ShellKind = 'sh' | 'powershell';

let detectedShell: ShellKind | null = null;

/** 测试专用：重置 shell 探测缓存（生产代码不应调用） */
export function resetShellCacheForTest(): void {
  detectedShell = null;
}

/** 探测可用 shell（先 sh，回退 PowerShell；结果缓存——同进程平台不变） */
export async function detectShell(exec: ExecFn): Promise<ShellKind> {
  if (detectedShell) {
    return detectedShell;
  }
  try {
    const probe = await exec('sh', ['-c', 'echo spark-probe']);
    if (probe.exitCode === 0 && probe.stdout.includes('spark-probe')) {
      detectedShell = 'sh';
      return detectedShell;
    }
  } catch {
    // 继续回退
  }
  const probe = await exec('powershell', ['-NoProfile', '-Command', 'Write-Output spark-probe']);
  if (probe.exitCode !== 0 || !probe.stdout.includes('spark-probe')) {
    throw new Error('本机既无 sh 也无可用 PowerShell——git 对象/附件的二进制往返无法进行（浏览不受影响，PR 发起/合并/物化不可用）');
  }
  detectedShell = 'powershell';
  return detectedShell;
}

/** 单引号包裹（sh 安全引号：' → '\''） */
export function shQuote(path: string): string {
  return `'${path.replace(/'/g, `'\\''`)}'`;
}

/** 经 shell 把 base64 内容落盘为文件（分块追加，规避命令行长度上限） */
export async function writeFileBase64(exec: ExecFn, path: string, base64: string): Promise<void> {
  const shell = await detectShell(exec);
  const CHUNK = 6000;
  const total = Math.max(1, Math.ceil(base64.length / CHUNK));
  for (let i = 0; i < total; i++) {
    const chunk = base64.slice(i * CHUNK, (i + 1) * CHUNK);
    if (shell === 'sh') {
      const op = i === 0 ? '>' : '>>';
      const result = await exec('sh', ['-c', `printf %s ${shQuote(chunk)} | base64 -d ${op} ${shQuote(path)}`]);
      if (result.exitCode !== 0) {
        throw new Error(`写入临时文件失败：${result.stderr.trim() || `exit ${result.exitCode}`}`);
      }
    } else {
      const quoted = path.replace(/'/g, "''");
      // .NET Framework 无 AppendAllBytes——追加块走 Open('Append')/Write/Close
      const ps =
        i === 0
          ? `[IO.File]::WriteAllBytes('${quoted}',[Convert]::FromBase64String('${chunk}'))`
          : `$fs=[IO.File]::Open('${quoted}','Append');$b=[Convert]::FromBase64String('${chunk}');$fs.Write($b,0,$b.Length);$fs.Close()`;
      const result = await exec('powershell', ['-NoProfile', '-Command', ps]);
      if (result.exitCode !== 0) {
        throw new Error(`写入临时文件失败：${result.stderr.trim() || `exit ${result.exitCode}`}`);
      }
    }
  }
}

/** 经 shell 读取文件为 base64（stdout 文本通道安全） */
export async function readFileBase64(exec: ExecFn, path: string): Promise<string> {
  const shell = await detectShell(exec);
  if (shell === 'sh') {
    const result = await exec('sh', ['-c', `base64 -w0 < ${shQuote(path)} 2>/dev/null || base64 < ${shQuote(path)}`]);
    if (result.exitCode !== 0) {
      throw new Error(`读取文件失败：${result.stderr.trim() || `exit ${result.exitCode}`}`);
    }
    return result.stdout.replace(/\s+/g, '');
  }
  const quoted = path.replace(/'/g, "''");
  const result = await exec('powershell', ['-NoProfile', '-Command', `[Convert]::ToBase64String([IO.File]::ReadAllBytes('${quoted}'))`]);
  if (result.exitCode !== 0) {
    throw new Error(`读取文件失败：${result.stderr.trim() || `exit ${result.exitCode}`}`);
  }
  return result.stdout.replace(/\s+/g, '');
}

/** 经 shell 删除文件（临时文件清理，尽力而为） */
export async function removeFile(exec: ExecFn, path: string): Promise<void> {
  try {
    const shell = await detectShell(exec);
    if (shell === 'sh') {
      await exec('sh', ['-c', `rm -f ${shQuote(path)}`]);
    } else {
      await exec('powershell', ['-NoProfile', '-Command', `Remove-Item -Force '${path.replace(/'/g, "''")}'`]);
    }
  } catch {
    // 清理失败不阻断主流程（.git 下临时文件，git gc 不受影响）
  }
}

/** 读取单个 git 对象的原始字节（base64 经 shell 管道过 stdout） */
export async function readObjectBase64(exec: ExecFn, dir: string, sha: string, type: GitObjectType): Promise<string> {
  // sha/type 会插值进 shell 命令与临时文件路径——入参先过白名单（来源虽是本机 git 输出，统一收口）
  if (!isHexSha(sha) || !['commit', 'tree', 'blob', 'tag'].includes(type)) {
    throw new Error(`对象坐标线形非法（拒绝读取）：sha=${JSON.stringify(sha)} type=${JSON.stringify(type)}`);
  }
  const shell = await detectShell(exec);
  if (shell === 'sh') {
    const result = await exec('sh', ['-c', `git -C ${shQuote(dir)} cat-file ${shQuote(type)} ${shQuote(sha)} | base64 -w0`]);
    if (result.exitCode !== 0) {
      throw new Error(`读取对象 ${sha.slice(0, 12)}… 失败：${result.stderr.trim() || `exit ${result.exitCode}`}`);
    }
    return result.stdout.replace(/\s+/g, '');
  }
  const quoted = dir.replace(/'/g, "''");
  // Start-Process 重定向保留原始字节（PS 的 > 运算符会按文本重编码，不可用）
  const tmp = `$env:TEMP\\spark-obj-${sha}.bin`;
  const ps =
    `Start-Process -FilePath git -ArgumentList '-C','${quoted}','cat-file','${type}','${sha}' ` +
    `-NoNewWindow -Wait -RedirectStandardOutput "${tmp}"; ` +
    `$b=[Convert]::ToBase64String([IO.File]::ReadAllBytes("${tmp}")); Remove-Item -Force "${tmp}"; $b`;
  const result = await exec('powershell', ['-NoProfile', '-Command', ps]);
  if (result.exitCode !== 0) {
    throw new Error(`读取对象 ${sha.slice(0, 12)}… 失败：${result.stderr.trim() || `exit ${result.exitCode}`}`);
  }
  return result.stdout.replace(/\s+/g, '');
}

// ------------------------------------------------------------------
// PR 流程的 git 步骤
// ------------------------------------------------------------------

/** 生成 git bundle（base..head 的提交序列；输出文件由 git 自写，读取走 shell base64） */
export async function createBundleBase64(
  exec: ExecFn,
  dir: string,
  baseSha: string,
  headRef: string,
  outPath: string
): Promise<string> {
  if (!isHexSha(baseSha)) {
    throw new Error(`base commit sha 线形非法：${JSON.stringify(baseSha)}`);
  }
  assertValidBranchName(headRef);
  await mustGit(exec, dir, ['bundle', 'create', outPath, `${baseSha}..${headRef}`], '生成 bundle');
  const base64 = await readFileBase64(exec, outPath);
  await removeFile(exec, outPath);
  return base64;
}

/** 校验 bundle 自含性（`git bundle verify` 需要 base 在本地；消费方 fetch 侧兜底） */
export async function fetchBundle(exec: ExecFn, dir: string, bundlePath: string, headSha: string, localRef: string): Promise<void> {
  if (!isHexSha(headSha)) {
    throw new Error(`head commit sha 线形非法：${JSON.stringify(headSha)}`);
  }
  assertValidBranchName(localRef);
  await mustGit(exec, dir, ['fetch', bundlePath, `${headSha}:${localRef}`], '从 bundle 取回提交');
}

/** 删除本地临时 ref（PR 合并后的 refs/spark-pr/* 清理；调用方尽力而为包裹） */
export async function deleteRef(exec: ExecFn, dir: string, ref: string): Promise<void> {
  assertValidBranchName(ref);
  await mustGit(exec, dir, ['update-ref', '-d', ref], `删除临时 ref ${ref}`);
}

/** 快进合并（--ff-only；维护者合并 PR 用，非快进由 git 直接拒绝） */
export async function mergeFfOnly(exec: ExecFn, dir: string, targetRef: string): Promise<string> {
  assertValidBranchName(targetRef);
  await mustGit(exec, dir, ['merge', '--ff-only', '--', targetRef], '快进合并');
  return resolveRef(exec, dir, 'HEAD');
}

/** 三方合并（维护者本地 git merge；冲突时 git 非零退出，插件如实上抛不代决） */
export async function mergeCommit(exec: ExecFn, dir: string, targetRef: string, message: string): Promise<string> {
  assertValidBranchName(targetRef);
  await mustGit(exec, dir, ['merge', '--no-edit', '-m', message, '--', targetRef], '合并');
  return resolveRef(exec, dir, 'HEAD');
}

/** ancestor 判定（CLI 版；`merge-base --is-ancestor`，exit 1 = 否） */
export async function isAncestorCli(exec: ExecFn, dir: string, ancestor: string, descendant: string): Promise<boolean> {
  const result = await exec('git', ['-C', dir, 'merge-base', '--is-ancestor', ancestor, descendant]);
  if (result.exitCode === 0) {
    return true;
  }
  if (result.exitCode === 1) {
    return false;
  }
  throw new Error(`祖先判定失败：${result.stderr.trim() || `exit ${result.exitCode}`}`);
}

/** 切出目标分支（维护者合并前定位 base） */
export async function checkoutBranch(exec: ExecFn, dir: string, branch: string): Promise<void> {
  // checkout 的 `--` 后位置是 pathspec 而非分支名——防注入靠线形校验而非 `--` 分隔
  assertValidBranchName(branch);
  await mustGit(exec, dir, ['checkout', branch], `检出分支 ${branch}`);
}

// ------------------------------------------------------------------
// 镜像物化（只读镜像 → 本地可用 git 工作区）
// ------------------------------------------------------------------

/**
 * 物化空目录护栏（评审 A28 不合理项）：目标目录须已存在且为空——
 * `git init` + `checkout -f` 会覆盖同名已有文件，非空目录直接拒绝。
 */
export async function assertEmptyDir(exec: ExecFn, dir: string): Promise<void> {
  const fail = `物化目标目录不存在或不为空：${dir}——请选择一个已存在的空目录（物化会执行 git init + checkout -f，非空目录有覆盖风险）`;
  const shell = await detectShell(exec);
  if (shell === 'sh') {
    const result = await exec('sh', ['-c', `[ -d ${shQuote(dir)} ] && [ -z "$(ls -A ${shQuote(dir)})" ]`]);
    if (result.exitCode !== 0) {
      throw new Error(fail);
    }
    return;
  }
  const quoted = dir.replace(/'/g, "''");
  const result = await exec('powershell', [
    '-NoProfile',
    '-Command',
    `if ((Test-Path -LiteralPath '${quoted}') -and -not (Get-ChildItem -Force -LiteralPath '${quoted}')) { exit 0 } else { exit 1 }`
  ]);
  if (result.exitCode !== 0) {
    throw new Error(fail);
  }
}

/**
 * 从镜像对象物化本地 git 仓库（桌面）：
 * git init → 逐对象写临时文件 + `hash-object -w` 入对象库 → update-ref 建分支
 * → HEAD 指向默认分支 → checkout 出工作区。临时文件经 shell base64 落盘
 * （sys.exec 无 stdin 的对策），全部在目标仓库 .git 下，完成后清理。
 */
export async function materializeRepo(
  exec: ExecFn,
  dir: string,
  objects: Array<{ sha: string; type: GitObjectType; base64: string }>,
  branches: Array<{ name: string; head: string }>,
  defaultBranch: string,
  onProgress?: (done: number, total: number) => void
): Promise<void> {
  await assertEmptyDir(exec, dir);
  for (const branch of branches) {
    assertValidBranchName(branch.name);
  }
  assertValidBranchName(defaultBranch);
  await mustGit(exec, dir, ['init'], '初始化仓库');
  let done = 0;
  try {
    for (const obj of objects) {
      const tmpPath = `${dir}/.git/spark-mirror-tmp-${obj.sha}`;
      await writeFileBase64(exec, tmpPath, obj.base64);
      await mustGit(exec, dir, ['hash-object', '-w', '-t', obj.type, '--', tmpPath], `写入对象 ${obj.sha.slice(0, 12)}…`);
      await removeFile(exec, tmpPath);
      done += 1;
      onProgress?.(done, objects.length);
    }
    for (const branch of branches) {
      await mustGit(exec, dir, ['update-ref', `refs/heads/${branch.name}`, branch.head], `建立分支 ${branch.name}`);
    }
    await mustGit(exec, dir, ['symbolic-ref', 'HEAD', `refs/heads/${defaultBranch}`], '设置默认分支');
    await mustGit(exec, dir, ['checkout', '-f', defaultBranch], '检出工作区');
  } finally {
    // 兜底清理残留临时文件（尽力而为；sh / PowerShell 两分支都要清）
    try {
      const shell = await detectShell(exec);
      if (shell === 'sh') {
        await exec('sh', ['-c', `rm -f ${shQuote(dir)}/.git/spark-mirror-tmp-*`]);
      } else {
        const quoted = `${dir}\\.git\\spark-mirror-tmp-*`.replace(/'/g, "''");
        // -Path（非 -LiteralPath）才能展开通配符
        await exec('powershell', ['-NoProfile', '-Command', `Remove-Item -Force -Path '${quoted}' -ErrorAction SilentlyContinue`]);
      }
    } catch {
      // 忽略
    }
  }
}
