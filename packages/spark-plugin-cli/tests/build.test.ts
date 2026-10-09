import { spawnSync } from 'child_process';
import { generateKeyPairSync } from 'crypto';
import { cp, mkdtemp, readFile, rm, writeFile } from 'fs/promises';
import fs from 'fs';
import { tmpdir } from 'os';
import path from 'path';
import { fileURLToPath } from 'url';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));
const cliBin = here('../bin/spark-plugin-cli.mjs');
const sampleAppSrc = here('../examples/sample-app');
const sampleLibSrc = here('../examples/sample-lib');

// 测试自签名（不依赖仓库 .secrets）：packager 优先读环境变量
const { privateKey } = generateKeyPairSync('ed25519');
const signingEnv = {
  ...process.env,
  SPARK_PLUGIN_SIGNING_PRIVATE_KEY: privateKey.export({ type: 'pkcs8', format: 'pem' }).toString()
};

let work: string;
let appDir: string;
let libDir: string;

function runCli(args: string[]) {
  const result = spawnSync(process.execPath, [cliBin, ...args], {
    env: signingEnv,
    encoding: 'utf8'
  });
  return result;
}

beforeEach(async () => {
  work = await mkdtemp(path.join(tmpdir(), 'spark-cli-build-'));
  appDir = path.join(work, 'sample-app');
  libDir = path.join(work, 'sample-lib');
  await cp(sampleAppSrc, appDir, { recursive: true });
  await cp(sampleLibSrc, libDir, { recursive: true });
});

afterEach(async () => {
  await rm(work, { recursive: true, force: true });
});

describe('spark-plugin-cli 双产物构建（样例实测）', () => {
  it('lock 回填 manifest hash + 写锁文件；build --mode app 产出 .spkg + sbom + 签名材料', () => {
    const lock = runCli(['lock', '--dir', appDir]);
    expect(lock.status, lock.stderr).toBe(0);
    const manifest = JSON.parse(fs.readFileSync(path.join(appDir, 'manifest.json'), 'utf8'));
    expect(manifest.libraries[0].hash).toMatch(/^[0-9a-f]{64}$/);
    const lockFile = JSON.parse(fs.readFileSync(path.join(appDir, 'spark-libraries.lock.json'), 'utf8'));
    expect(lockFile.libraries[0].hash).toBe(manifest.libraries[0].hash);

    const outDir = path.join(work, 'out-app');
    const build = runCli(['build', '--dir', appDir, '--mode', 'app', '--outputDir', outDir]);
    expect(build.status, build.stderr).toBe(0);

    const updateManifest = JSON.parse(fs.readFileSync(path.join(outDir, 'update-manifest.json'), 'utf8'));
    const kinds = updateManifest.assets.map((a: { kind: string }) => a.kind);
    expect(kinds).toContain('package');
    expect(kinds).toContain('sbom');
    expect(fs.existsSync(path.join(outDir, 'update-manifest.sig'))).toBe(true);
    expect(fs.existsSync(path.join(outDir, 'update-manifest.pub.pem'))).toBe(true);

    const packageAsset = updateManifest.assets.find((a: { kind: string }) => a.kind === 'package');
    const spkg = JSON.parse(fs.readFileSync(path.join(outDir, packageAsset.fileName), 'utf8'));
    // H1 回归：安装包 = dist 全量，无 vendor 条目；main.js 已内联依赖、不引用 vendor
    const spkgPaths = spkg.files.map((f: { path: string }) => f.path);
    expect([...spkgPaths].sort()).toEqual(['manifest.json', 'sbom.json', 'views/main.js']);
    const mainEntry = spkg.files.find((f: { path: string }) => f.path === 'views/main.js');
    const mainJs = Buffer.from(mainEntry.contentBase64, 'base64').toString('utf8');
    expect(mainJs).toContain('function formatBytes');
    expect(mainJs).not.toContain('vendor/');
    expect(mainJs).not.toMatch(/import\s+.*from\s*['"]\.\./);
    const sbomEntry = spkg.files.find((f: { path: string }) => f.path === 'sbom.json');
    expect(sbomEntry).toBeTruthy();
    const sbom = JSON.parse(Buffer.from(sbomEntry.contentBase64, 'base64').toString('utf8'));
    expect(sbom.sbomVersion).toBe(1);
    expect(sbom.plugin.id).toBe('sample-app');
    expect(sbom.libraries).toHaveLength(1);
    expect(sbom.libraries[0].repo).toBe('github.com/spark-samples/sample-lib');
    expect(sbom.libraries[0].hash).toBe(manifest.libraries[0].hash);
    // SBOM 文件本身也随包落盘到输出目录
    expect(fs.existsSync(path.join(outDir, 'sbom.json'))).toBe(true);
  });

  it('build --mode library 产出 npm/git 形态库包目录 + 校验清单', () => {
    const outDir = path.join(work, 'out-lib');
    const build = runCli(['build', '--dir', libDir, '--mode', 'library', '--outputDir', outDir]);
    expect(build.status, build.stderr).toBe(0);
    const packageDir = path.join(outDir, 'sample-lib-0.1.0');
    expect(fs.existsSync(path.join(packageDir, 'package.json'))).toBe(true);
    expect(fs.existsSync(path.join(packageDir, 'manifest.json'))).toBe(true);
    expect(fs.existsSync(path.join(packageDir, 'sbom.json'))).toBe(true);
    expect(fs.existsSync(path.join(packageDir, 'dist', 'index.js'))).toBe(true);
    expect(fs.existsSync(path.join(outDir, 'sample-lib-0.1.0-checksums.txt'))).toBe(true);
    const pkg = JSON.parse(fs.readFileSync(path.join(packageDir, 'package.json'), 'utf8'));
    expect(pkg.sparkPlugin.kind).toBe('library');
    const sbom = JSON.parse(fs.readFileSync(path.join(packageDir, 'sbom.json'), 'utf8'));
    expect(sbom.libraries).toEqual([]);
  });

  it('反例：未 lock 直接 build 必拒（E_LIB_HASH）', async () => {
    // 仓库内样例处于已锁定状态（实测留痕）；本用例在副本里还原未锁定现场
    const manifestPath = path.join(appDir, 'manifest.json');
    const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
    manifest.libraries[0].hash = '';
    await writeFile(manifestPath, JSON.stringify(manifest, null, 2) + '\n', 'utf8');
    await rm(path.join(appDir, 'spark-libraries.lock.json'), { force: true });
    const build = runCli(['build', '--dir', appDir, '--mode', 'app', '--outputDir', path.join(work, 'x1')]);
    expect(build.status).not.toBe(0);
    expect(build.stderr).toMatch(/E_LIB_HASH/);
  });

  it('反例：vendor 篡改后 verify/build 必拒（E_LIB_INTEGRITY）', async () => {
    expect(runCli(['lock', '--dir', appDir]).status).toBe(0);
    await writeFile(
      path.join(appDir, 'vendor', 'github.com', 'spark-samples', 'sample-lib', 'index.js'),
      'malicious\n',
      'utf8'
    );
    const verify = runCli(['verify', '--dir', appDir]);
    expect(verify.status).not.toBe(0);
    expect(verify.stderr).toMatch(/E_LIB_INTEGRITY/);
  });

  it('反例：dist 残留对 vendor/ 的引用必拒（E_DIST_NOT_SELFCONTAINED，H1 反模式回归）', async () => {
    expect(runCli(['lock', '--dir', appDir]).status).toBe(0);
    // 还原 H1 的坏形态：dist/views/main.js 相对引用 vendor 源码
    await writeFile(
      path.join(appDir, 'dist', 'views', 'main.js'),
      "import { formatBytes } from '../../vendor/github.com/spark-samples/sample-lib/index.js';\n"
        + 'export function render(el) { el.textContent = formatBytes(1572864); }\n',
      'utf8'
    );
    const build = runCli(['build', '--dir', appDir, '--mode', 'app', '--outputDir', path.join(work, 'x5')]);
    expect(build.status).not.toBe(0);
    expect(build.stderr).toMatch(/E_DIST_NOT_SELFCONTAINED/);
    expect(build.stderr).toMatch(/vendor/);
  });

  it('反例：dist 引用越过 dist 根目录 / 指向不存在目标必拒（E_DIST_NOT_SELFCONTAINED）', async () => {
    expect(runCli(['lock', '--dir', appDir]).status).toBe(0);
    await writeFile(
      path.join(appDir, 'dist', 'views', 'main.js'),
      "import { helper } from '../../src/helper.js';\nexport function render(el) { el.textContent = helper(); }\n",
      'utf8'
    );
    const build = runCli(['build', '--dir', appDir, '--mode', 'app', '--outputDir', path.join(work, 'x6')]);
    expect(build.status).not.toBe(0);
    expect(build.stderr).toMatch(/E_DIST_NOT_SELFCONTAINED/);
    // 库包路径同规则：sample-lib dist 指向不存在的 dist 内目标也必拒
    await writeFile(
      path.join(libDir, 'dist', 'index.js'),
      "export { formatBytes } from './missing.js';\n",
      'utf8'
    );
    const libBuild = runCli(['build', '--dir', libDir, '--mode', 'library', '--outputDir', path.join(work, 'x7')]);
    expect(libBuild.status).not.toBe(0);
    expect(libBuild.stderr).toMatch(/E_DIST_NOT_SELFCONTAINED/);
  });

  it('反例：app 用 --mode library / library 用 --mode app 必拒（E_KIND_MISMATCH）', () => {
    const wrong1 = runCli(['build', '--dir', appDir, '--mode', 'library', '--outputDir', path.join(work, 'x2')]);
    // app 未 lock 会先撞 E_LIB_HASH；先 lock 再验 kind
    expect(runCli(['lock', '--dir', appDir]).status).toBe(0);
    const wrongApp = runCli(['build', '--dir', appDir, '--mode', 'library', '--outputDir', path.join(work, 'x3')]);
    expect(wrongApp.status).not.toBe(0);
    expect(wrongApp.stderr).toMatch(/E_KIND_MISMATCH/);
    const wrongLib = runCli(['build', '--dir', libDir, '--mode', 'app', '--outputDir', path.join(work, 'x4')]);
    expect(wrongLib.status).not.toBe(0);
    expect(wrongLib.stderr).toMatch(/E_KIND_MISMATCH/);
    expect(wrong1.status).not.toBe(0);
  });
});
