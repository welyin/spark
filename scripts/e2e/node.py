"""e2e 节点进程封装：spawn e2e_node、命令收发、事件等待与断言助手。

协议（stdio JSON 行）：
- 请求：{"id": n, "cmd": "...", ...参数} → 响应 {"id": n, "ok": true, "data": ...}
  或 {"id": n, "ok": false, "error": "..."}
- 事件：{"event": "<P2pEvent 变体名>", "data": ...}（异步，随时到达）
- 启动后节点先打一行 {"ready": true}。

所有等待一律轮询（wait_event / poll_until），不做固定 sleep——真实 P2P 时序有抖动。
"""

import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

# 节点二进制：code/core/target/debug/examples/e2e_node（env 可覆盖）
CODE_CORE = Path(__file__).resolve().parents[2] / "core"
DEFAULT_BIN = CODE_CORE / "target" / "debug" / "examples" / "e2e_node"
NODE_BIN = Path(os.environ.get("E2E_NODE_BIN", DEFAULT_BIN))

# 事件/命令默认超时（秒）
EVENT_TIMEOUT = 25.0
CMD_TIMEOUT = 60.0


class NodeError(Exception):
    """命令被内核拒绝（响应 ok=false）。"""


class EventTimeout(AssertionError):
    """等待事件超时。"""


def check(cond, msg):
    if not cond:
        raise AssertionError(msg)


def _free_port():
    """向 OS 申请一个空闲 TCP 端口（随后释放，存在小竞态，测试可接受）。"""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def dialable(addresses):
    """通配监听地址换成 loopback（仅 ip4）。"""
    return [
        a.replace("/ip4/0.0.0.0/", "/ip4/127.0.0.1/")
        for a in addresses
        if "/ip4/" in a
    ]


def poll_until(fn, timeout=EVENT_TIMEOUT, interval=0.2, what="condition"):
    """轮询直到 fn() 返回真值（返回该值），超时抛 AssertionError。"""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = fn()
        if value:
            return value
        time.sleep(interval)
    raise AssertionError(f"timeout waiting for: {what}")


class Node:
    """一个 e2e_node 进程：stdin 写命令、stdout 读响应与事件。"""

    def __init__(self, name):
        self.name = name
        self.data_dir = tempfile.mkdtemp(prefix=f"e2e-{name}-")
        self.proc = None
        self._next_id = 0
        self._write_lock = threading.Lock()
        self._cond = threading.Condition()
        self._results = {}  # id -> response dict
        self._events = []  # 收到的全部事件行（dict）
        self._consumed = set()  # 已被 wait_event 消费的事件下标
        self._ready = False
        # 身份/网络缓存（init() 填充）
        self.root_id = None
        self.mnemonic = None
        self.peer_id = None
        self.addresses = []

    # ------------------------------------------------------------------
    # 进程生命周期
    # ------------------------------------------------------------------

    def start(self, timeout=30.0):
        check(NODE_BIN.exists(), f"e2e_node 二进制不存在: {NODE_BIN}（先 cargo build --example e2e_node）")
        # 钉死监听端口：p2p 重启（离线→上线场景）后端口不变，
        # 对端已存寻址不失效（e2e 配置 preferred_port 优先于持久化端口）。
        self.port = _free_port()
        stderr_log = open(Path(self.data_dir) / "stderr.log", "w")
        self._stderr_log = stderr_log
        self.proc = subprocess.Popen(
            [str(NODE_BIN), "--data-dir", self.data_dir, "--preferred-port", str(self.port)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=stderr_log,
            text=True,
            bufsize=1,
        )
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()
        poll_until(lambda: self._ready, timeout=timeout, what=f"{self.name} ready")
        return self

    def _read_loop(self):
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue
            with self._cond:
                if obj.get("ready"):
                    self._ready = True
                elif "event" in obj:
                    self._events.append(obj)
                elif "id" in obj and "ok" in obj:
                    self._results[obj["id"]] = obj
                self._cond.notify_all()

    def kill(self):
        """尽力优雅关停（shutdown 命令 → terminate → kill），并回收临时目录。"""
        if self.proc and self.proc.poll() is None:
            try:
                self.send("shutdown", timeout=5)
            except Exception:
                pass
            try:
                self.proc.wait(timeout=8)
            except subprocess.TimeoutExpired:
                self.proc.terminate()
                try:
                    self.proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.proc.kill()
        if self.proc:
            self.proc.stdout.close()
            if self.proc.stdin:
                self.proc.stdin.close()
        try:
            self._stderr_log.close()
        except Exception:
            pass

    def stop_p2p(self):
        self.send("stop-p2p")

    # ------------------------------------------------------------------
    # 命令收发
    # ------------------------------------------------------------------

    def send(self, cmd, timeout=CMD_TIMEOUT, **params):
        """发命令并等响应；ok=false 抛 NodeError。返回 data。"""
        with self._write_lock:
            self._next_id += 1
            req_id = self._next_id
            line = json.dumps({"id": req_id, "cmd": cmd, **params}, ensure_ascii=False)
            self.proc.stdin.write(line + "\n")
            self.proc.stdin.flush()
        deadline = time.monotonic() + timeout
        with self._cond:
            while req_id not in self._results:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise AssertionError(f"{self.name}: 命令 {cmd} 响应超时")
                self._cond.wait(timeout=min(remaining, 0.5))
            resp = self._results.pop(req_id)
        if not resp.get("ok"):
            raise NodeError(f"{self.name}: {cmd} 失败: {resp.get('error')}")
        return resp.get("data")

    # ------------------------------------------------------------------
    # 事件等待
    # ------------------------------------------------------------------

    def wait_event(self, name, pred=None, timeout=EVENT_TIMEOUT):
        """等待指定事件（可选谓词过滤 data）；命中即消费并返回 data。"""
        deadline = time.monotonic() + timeout
        while True:
            with self._cond:
                for i, obj in enumerate(self._events):
                    if i in self._consumed or obj.get("event") != name:
                        continue
                    data = obj.get("data")
                    if pred is None or pred(data):
                        self._consumed.add(i)
                        return data
            if time.monotonic() >= deadline:
                raise EventTimeout(f"{self.name}: 等待事件 {name} 超时（{timeout}s）")
            time.sleep(0.05)

    def expect_no_event(self, name, seconds=6.0, pred=None):
        """断言时间窗内不出现指定事件（拉黑/拒收路径）。"""
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            with self._cond:
                for i, obj in enumerate(self._events):
                    if i in self._consumed or obj.get("event") != name:
                        continue
                    data = obj.get("data")
                    if pred is None or pred(data):
                        raise AssertionError(f"{self.name}: 不应出现事件 {name}: {data}")
            time.sleep(0.05)

    # ------------------------------------------------------------------
    # 高层助手
    # ------------------------------------------------------------------

    def init(self, nickname):
        """创建身份并等 p2p 地址就绪；缓存 rootId/peerId/addresses。"""
        data = self.send("init-identity", nickname=nickname)
        self.root_id = data["rootId"]
        self.mnemonic = data["mnemonic"]
        self.refresh_p2p()
        return self

    def recover(self, mnemonic, nickname):
        """助记词恢复（设备配对：同身份第二节点）。"""
        data = self.send("recover-mnemonic", mnemonic=mnemonic, nickname=nickname)
        self.root_id = data["rootId"]
        self.mnemonic = mnemonic
        self.refresh_p2p()
        return self

    def refresh_p2p(self, timeout=EVENT_TIMEOUT):
        """轮询 p2p-status 直到拿到可拨地址（启动后地址绑定是异步的）。"""
        def probe():
            status = self.send("p2p-status")
            if status.get("started") and dialable(status.get("addresses", [])):
                return status
            return None

        status = poll_until(probe, timeout=timeout, what=f"{self.name} p2p 地址就绪")
        self.peer_id = status["peerId"]
        self.addresses = dialable(status["addresses"])
        return status

    def overview(self, space="personal"):
        return self.send("contact-overview", space=space)

    def friend_entry(self, root_id, space="personal"):
        """通讯录里的朋友条目（不存在 → None）。"""
        for f in self.overview(space)["friends"]:
            if f["rootId"] == root_id:
                return f
        return None

    def conv_id(self, peer_root_id):
        return f"dm:{peer_root_id}"

    def send_text(self, peer_root_id, text, link=None, message_id=None):
        params = {"peerRootId": peer_root_id, "text": text}
        if link is not None:
            params["link"] = link
        if message_id:
            params["messageId"] = message_id
        return self.send("send-text", **params)


def wait_message_status(node, conv_id, msg_id, status, timeout=EVENT_TIMEOUT):
    """轮询消息列表直到目标消息状态收敛（投递是异步的：先 sending 后终态）。"""
    def probe():
        for m in node.send("messages", convId=conv_id):
            if m["id"] == msg_id and m.get("status") == status:
                return m
        return None

    return poll_until(probe, timeout=timeout, what=f"{node.name} 消息 {msg_id} 状态={status}")


def _wait_terminal_status(node, conv_id, message_id, timeout):
    """等消息收敛到终态，返回 status 字符串（超时返回 None）。"""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        msgs = node.send("messages", convId=conv_id)
        msg = next((m for m in msgs if m["id"] == message_id), None)
        check(msg is not None, f"{node.name} 消息 {message_id} 未落库")
        if msg.get("status") in ("delivered", "read", "failed"):
            return msg
        time.sleep(0.2)
    return None


def send_chat_reliably(node, peer_root_id, text, message_id, link=None, timeout=EVENT_TIMEOUT):
    """发文本并等 delivered。

    dm 入站有 1s 限流桶（chat/friend-request 等内容型 kind 共享；见
    wiki/protocol/p2p-messages.md §19），自动化脚本消息间隔远低于真人，
    易被限流置 failed——失败后等 1.2s 用 resend 重试（最多 3 次）。
    """
    conv_id = node.conv_id(peer_root_id)
    node.send_text(peer_root_id, text, link=link, message_id=message_id)
    return _retry_until_delivered(node, conv_id, message_id, timeout)


def resend_reliably(node, conv_id, message_id, timeout=EVENT_TIMEOUT):
    """resend 并等 delivered（对端刚重启时监听未就绪，首次拨号可能秒失败）。"""
    node.send("resend", convId=conv_id, messageId=message_id)
    return _retry_until_delivered(node, conv_id, message_id, timeout)


def _retry_until_delivered(node, conv_id, message_id, timeout):
    for attempt in range(3):
        if attempt > 0:
            time.sleep(1.2)
            node.send("resend", convId=conv_id, messageId=message_id)
        msg = _wait_terminal_status(node, conv_id, message_id, timeout)
        if msg and msg.get("status") in ("delivered", "read"):
            return msg
    raise AssertionError(f"{node.name} 消息 {message_id} 多次投递失败（非抖动可解释）")


def make_friends(a, b, timeout=EVENT_TIMEOUT):
    """A→B 好友全流程：申请 → B 收事件 → B 接受 → A 收确认 → 双向记录断言。"""
    a.send(
        "send-request",
        rootId=b.root_id,
        peerId=b.peer_id,
        addresses=b.addresses,
        message="交个朋友",
    )
    received = b.wait_event(
        "FriendRequestReceived",
        lambda d: d["request"]["rootId"] == a.root_id,
        timeout=timeout,
    )
    b.send("accept-request", requestId=received["request"]["id"])
    a.wait_event(
        "FriendRequestAccepted",
        lambda d: d["friend"]["rootId"] == b.root_id,
        timeout=timeout,
    )
    check(a.friend_entry(b.root_id) is not None, f"{a.name} 的朋友列表应有 {b.name}")
    check(b.friend_entry(a.root_id) is not None, f"{b.name} 的朋友列表应有 {a.name}")


def kill_all(nodes):
    for node in nodes:
        try:
            node.kill()
        except Exception:
            pass


def run_scenario(fn, nodes):
    """场景骨架：计时 + 异常时打印节点 stderr 尾部便于诊断。"""
    started = time.monotonic()
    try:
        fn()
    except Exception:
        for node in nodes:
            log = Path(node.data_dir) / "stderr.log"
            if log.exists():
                tail = log.read_text(errors="replace")[-3000:]
                print(f"--- {node.name} stderr 尾部 ---\n{tail}", file=sys.stderr)
        raise
    finally:
        kill_all(nodes)
    return time.monotonic() - started
