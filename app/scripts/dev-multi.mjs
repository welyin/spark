#!/usr/bin/env node
// 多实例开发启动器：npm run tauri dev [N|android]
//   dev          → 单实例桌面端（现状不变）
//   dev 2|3      → 单机多开桌面端：共享 vite，实例 1 启动，其余复用
//   dev android  → 桌面端 + Android 真机联调：共享 vite，手机连接局域网 dev server
// 其余子命令（build 等）原样透传给 tauri CLI。
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const appDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DEV_URL = 'http://localhost:1420';
const KILL_GRACE_MS = 10_000;

const args = process.argv.slice(2).filter((a) => a !== '--');
const isAndroidMode = args[0] === 'dev' && args.includes('android');
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

async function waitForDevServer(timeoutMs = 180_000, url = DEV_URL) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(url, { signal: AbortSignal.timeout(3_000) });
      if (res.ok) return;
    } catch {
      // dev server 尚未就绪
    }
    await new Promise((r) => setTimeout(r, 1_000));
  }
  throw new Error(`等待 ${url} 超时（vite dev server 未就绪）`);
}

function getLocalIP() {
  const nets = os.networkInterfaces();
  for (const name of Object.keys(nets)) {
    for (const net of nets[name] ?? []) {
      if (net.family === 'IPv4' && !net.internal) return net.address;
    }
  }
  return '127.0.0.1';
}

async function main() {
  // Android 联调模式：桌面 + 手机共用一个 vite
  if (isAndroidMode) {
    process.on('SIGINT', () => shutdown('SIGINT'));
    process.on('SIGTERM', () => shutdown('SIGTERM'));

    const lanIP = getLocalIP();
    console.log(`[dev-multi] Android 联调模式，局域网 IP: ${lanIP}`);

    // 1. 先启动独立 vite（监听 0.0.0.0，两端共用）
    run('npx', ['vite', '--port', '1420', '--strictPort'], {
      TAURI_DEV_HOST: '0.0.0.0',
    });
    console.log('[dev-multi] vite dev server 启动中…');
    await waitForDevServer(30_000, `http://${lanIP}:1420`);
    console.log(`[dev-multi] vite 就绪 → http://${lanIP}:1420`);

    // 2. 清空 beforeDevCommand 的配置文件（两端复用，不重复起 vite）
    const noServerConfig = path.join(appDir, '.dev-data', 'tauri-no-dev-server.json');
    fs.mkdirSync(path.dirname(noServerConfig), { recursive: true });
    fs.writeFileSync(noServerConfig, JSON.stringify({ build: { beforeDevCommand: '' } }));

    // 3. 启动 Android dev
    const androidDataDir = path.join(appDir, '.dev-data', 'instance-android');
    fs.mkdirSync(androidDataDir, { recursive: true });
    run('npx', [
      'tauri', 'android', 'dev',
      '-c', '.dev-data/tauri-no-dev-server.json',
      '--host', lanIP,
    ], {
      SPARK_DATA_DIR: androidDataDir,
    });
    console.log(`[dev-multi] Android 实例启动（SPARK_DATA_DIR=${androidDataDir}）`);

    // 4. 启动桌面端
    const desktopDataDir = path.join(appDir, '.dev-data', 'instance-desktop');
    fs.mkdirSync(desktopDataDir, { recursive: true });
    run('npx', [
      'tauri', 'dev',
      '-c', '.dev-data/tauri-no-dev-server.json',
      '--no-watch',
    ], {
      SPARK_DATA_DIR: desktopDataDir,
    });
    console.log(`[dev-multi] 桌面实例启动（SPARK_DATA_DIR=${desktopDataDir}）`);

    // 全部退出时整组退出
    const watcher = setInterval(() => {
      if (children.size === 0) {
        clearInterval(watcher);
        process.exit(1);
      }
    }, 500);
    return;
  }

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
      // 多实例必须隔离 cargo target 目录：Windows 上运行中的 exe 被独占锁定，
      // 共享 target 时第二个实例无法覆盖 spark-app.exe（os error 5），
      // 且会阻塞在 build 目录文件锁上。各实例独立 target-instance-<i>
      // （代价是第二个实例首次全量编译较慢）。
      env.CARGO_TARGET_DIR = path.join(appDir, 'src-tauri', `target-instance-${i}`);
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
