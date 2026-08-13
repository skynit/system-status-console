import { spawn } from "node:child_process";
import { access } from "node:fs/promises";
import net from "node:net";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const workspaceRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const viteHost = "127.0.0.1";
const vitePort = 1420;
const startupTimeoutMs = 120_000;
const shutdownTimeoutMs = 5_000;
const children = new Map();
let shuttingDown = false;

function log(message) {
  process.stdout.write(`[dev] ${message}\n`);
}

function fail(message) {
  process.stderr.write(`[dev] ${message}\n`);
  process.exitCode = 1;
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function connect(options, timeoutMs = 250) {
  return new Promise((resolve) => {
    const socket = net.createConnection(options);
    const finish = (connected) => {
      socket.removeAllListeners();
      socket.destroy();
      resolve(connected);
    };

    socket.setTimeout(timeoutMs);
    socket.once("connect", () => finish(true));
    socket.once("timeout", () => finish(false));
    socket.once("error", () => finish(false));
  });
}

async function waitUntil(label, probe) {
  const deadline = Date.now() + startupTimeoutMs;
  while (Date.now() < deadline) {
    if (await probe()) {
      log(`${label} 已就绪`);
      return;
    }
    await delay(150);
  }
  throw new Error(`${label} 在 ${startupTimeoutMs / 1000} 秒内未就绪`);
}

function start(name, command, args) {
  log(`启动 ${name}: ${command} ${args.join(" ")}`);
  const child = spawn(command, args, {
    cwd: workspaceRoot,
    detached: true,
    env: process.env,
    stdio: "inherit",
  });

  children.set(name, child);
  child.once("error", (error) => {
    if (!shuttingDown) {
      void shutdown(1, `${name} 启动失败: ${error.message}`);
    }
  });
  child.once("exit", (code, signal) => {
    if (shuttingDown) {
      return;
    }
    const result = signal ? `信号 ${signal}` : `退出码 ${code ?? 1}`;
    const exitCode = name === "desktop" && code === 0 ? 0 : code || 1;
    void shutdown(exitCode, `${name} 已退出（${result}）`);
  });

  return child;
}

function runPreparation(name, command, args) {
  log(`执行 ${name}: ${command} ${args.join(" ")}`);
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: workspaceRoot,
      detached: true,
      env: process.env,
      stdio: "inherit",
    });
    children.set(name, child);
    let settled = false;

    child.once("error", (error) => {
      if (settled) return;
      settled = true;
      children.delete(name);
      reject(new Error(`${name} 启动失败: ${error.message}`));
    });
    child.once("exit", (code, signal) => {
      if (settled) return;
      settled = true;
      children.delete(name);
      if (code === 0) {
        resolve();
        return;
      }
      const result = signal ? `信号 ${signal}` : `退出码 ${code ?? 1}`;
      reject(new Error(`${name} 失败（${result}）`));
    });
  });
}

function signalProcessGroup(child, signal) {
  if (!child.pid) {
    return;
  }
  try {
    process.kill(-child.pid, signal);
  } catch (error) {
    if (error.code !== "ESRCH") {
      fail(`无法向进程组 ${child.pid} 发送 ${signal}: ${error.message}`);
    }
  }
}

function processGroupExists(child) {
  if (!child.pid) return false
  try {
    process.kill(-child.pid, 0)
    return true
  } catch (error) {
    if (error.code === "ESRCH") return false
    throw error
  }
}

async function shutdown(exitCode, reason) {
  if (shuttingDown) {
    return;
  }
  shuttingDown = true;
  log(reason);
  log("正在关闭开发进程...");

  for (const child of children.values()) {
    signalProcessGroup(child, "SIGTERM");
  }

  const deadline = Date.now() + shutdownTimeoutMs
  while (Date.now() < deadline && [...children.values()].some(processGroupExists)) {
    await delay(100)
  }
  for (const child of children.values()) {
    if (processGroupExists(child)) signalProcessGroup(child, "SIGKILL");
  }
  process.exit(exitCode);
}

async function main() {
  const runtimeDirectory = process.env.XDG_RUNTIME_DIR;
  if (!runtimeDirectory || !path.isAbsolute(runtimeDirectory)) {
    throw new Error("XDG_RUNTIME_DIR 未设置或不是绝对路径，appd 无法创建安全 socket");
  }

  const appdSocket = path.join(runtimeDirectory, "localdesk", "appd.sock");
  if (await connect({ path: appdSocket })) {
    throw new Error(`已有 appd 正在监听 ${appdSocket}，请先关闭旧开发进程`);
  }
  if (await connect({ host: viteHost, port: vitePort })) {
    throw new Error(`端口 ${viteHost}:${vitePort} 已被占用，请先关闭旧 Vite 进程`);
  }

  await runPreparation("Rust 构建", "cargo", [
    "build",
    "-p",
    "localdesk-appd",
    "-p",
    "localdesk-desktop",
    "-p",
    "localdesk-remote-ssh",
    "-p",
    "localdesk-telemetry-helper",
    "-p",
    "localdesk-network-helper",
    "--locked",
  ]);

  start("appd", path.join(workspaceRoot, "target/debug/localdesk-appd"), []);
  await waitUntil("appd", async () => {
    try {
      await access(appdSocket);
      return connect({ path: appdSocket });
    } catch {
      return false;
    }
  });

  start("vite", "pnpm", [
    "--filter",
    "desktop-ui",
    "dev",
    "--host",
    viteHost,
    "--port",
    String(vitePort),
    "--strictPort",
  ]);
  await waitUntil("Vite", () => connect({ host: viteHost, port: vitePort }));

  start("desktop", path.join(workspaceRoot, "target/debug/localdesk-desktop"), []);
  log("开发环境已启动；关闭桌面窗口或按 Ctrl+C 可停止全部进程");
}

process.on("SIGINT", () => void shutdown(130, "收到 Ctrl+C"));
process.on("SIGTERM", () => void shutdown(143, "收到 SIGTERM"));
process.on("SIGHUP", () => void shutdown(129, "开发终端已关闭"));

main().catch(async (error) => {
  fail(error instanceof Error ? error.message : String(error));
  if (children.size > 0) {
    await shutdown(1, "开发环境启动失败");
  }
});
