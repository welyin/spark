#!/usr/bin/env node
// 多实例开发启动器：npm run tauri dev [N]
//   dev      → 单实例（现状不变，用默认 app_data_dir）
//   dev 2|3  → 单机多开：共享一个 vite dev server（实例 1 启动，其余 --no-dev-server
//              复用），每实例独立 SPARK_DATA_DIR=.dev-data/instance-<i>（sled 单目录
//              独占，必须隔离）；Ctrl+C 一次全退（同进程组收到信号，包装器兜底强杀）
// 其余子命令（build 等）原样透传给 tauri CLI。
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const appDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DEV_URL = 'http://localhost:1420';
const KILL_GRACE_MS = 10_000;

const args = process.argv.slice(2).filter((a) => a !== '--');
const countIndex = args.findIndex((a) => /^\d+$/.test(a));
const count = countIndex >= 0 ? Number(args[countIndex]) : 1;
const passthrough = countIndex >= 0 ? args.filter((_, i) => i !== countIndex) : args;

const children = new Set();
let shuttingDown = false;

function run(cmd, cmdArgs, env = {}) {
  // 不 detached：子进程与终端同进程组，Ctrl+C 时全员收到 SIGINT
  const child = spawn(cmd, cmdArgs, {
    cwd: appDir,
    env: { ...process.env, ...env },
    stdio: 'inherit',
    shell: process.platform === 'win32',
  });
  children.add(child);
  child.on('exit', () => children.delete(child));
  return child;
}

function forceKillAll() {
  for (const child of children) {
    try {
      child.kill('SIGKILL');
    } catch {
      // 已退出，忽略
    }
  }
}

function shutdown(signal) {
  if (shuttingDown) return;
  shuttingDown = true;
  console.log(`\n[dev-multi] 收到 ${signal}，等待全部实例退出…`);
  // 子进程同组已收到信号；兜底：宽限期后强杀残留
  setTimeout(() => {
    if (children.size > 0) {
      console.log(`[dev-multi] 宽限期满，强杀 ${children.size} 个残留进程`);
      forceKillAll();
    }
    process.exit(0);
  }, KILL_GRACE_MS).unref();
  const timer = setInterval(() => {
    if (children.size === 0) {
      clearInterval(timer);
      process.exit(0);
    }
  }, 200);
}

async function waitForDevServer(timeoutMs = 180_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(DEV_URL, { signal: AbortSignal.timeout(3_000) });
      if (res.ok) return;
    } catch {
      // dev server 尚未就绪
    }
    await new Promise((r) => setTimeout(r, 1_000));
  }
  throw new Error(`等待 ${DEV_URL} 超时（实例 1 的 vite dev server 未就绪）`);
}

async function main() {
  // 非 dev 或未给数量：原样透传（含 npm run tauri build 等）
  if (passthrough[0] !== 'dev' || count <= 1) {
    const child = run('npx', ['tauri', ...args]);
    child.on('exit', (code) => process.exit(code ?? 0));
    return;
  }

  process.on('SIGINT', () => shutdown('SIGINT'));
  process.on('SIGTERM', () => shutdown('SIGTERM'));

  console.log(`[dev-multi] 启动 ${count} 个实例，数据目录 .dev-data/instance-<i>`);
  for (let i = 1; i <= count; i++) {
    const dataDir = path.join(appDir, '.dev-data', `instance-${i}`);
    fs.mkdirSync(dataDir, { recursive: true });
    const env = { SPARK_DATA_DIR: dataDir };
    if (i === 1) {
      // 实例 1 正常 dev：经 beforeDevCommand 拉起 vite dev server
      run('npx', ['tauri', 'dev'], env);
    } else {
      // 其余实例复用实例 1 的 dev server（--no-dev-server 在本 CLI 版本不生效，
      // 用 -c 覆盖清空 beforeDevCommand 跳过重复起 vite，--no-watch 关掉文件监听）。
      // 注意：Windows 下 spawn shell:true 经 cmd.exe 转发会剥掉 JSON 内的双引号，
      // 内联 -c '{"build":...}' 会被 CLI 当成非法 JSON——改为写配置文件传路径，
      // 相对路径 + 正斜杠，彻底绕开 cmd 引号语义。
      const noServerConfig = path.join(appDir, '.dev-data', 'tauri-no-dev-server.json');
      fs.writeFileSync(noServerConfig, JSON.stringify({ build: { beforeDevCommand: '' } }));
      await waitForDevServer();
      run(
        'npx',
        ['tauri', 'dev', '-c', '.dev-data/tauri-no-dev-server.json', '--no-watch'],
        env,
      );
    }
    console.log(`[dev-multi] 实例 ${i} 已启动（SPARK_DATA_DIR=${dataDir}）`);
  }
  // 实例 1 挂掉（如编译失败）时整组退出，不留孤儿
  const watcher = setInterval(() => {
    if (children.size === 0) {
      clearInterval(watcher);
      process.exit(1);
    }
  }, 500);
}

main().catch((e) => {
  console.error(`[dev-multi] ${e.message}`);
  forceKillAll();
  process.exit(1);
});
