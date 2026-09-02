#!/usr/bin/env python3
"""F6 验收·故障注入：对端 org-pull 不应答（半连接长超时）下的 worker 韧性。

设计：wiki/architecture/sync/org-sync-stall-fix.md。原故障形态：join 编排期
A 的 orgsync-hello 出站稀疏后归零 → 对端「预录成员快照未达」→ join 失败
（e2e 间歇 1-2/15）。根因：1s 无界注入的 KeepaliveTick 在串行 worker 队列
积压，末段 orgsync-hello 被无限推迟。

复现手法（故障注入 + 超时缩短，替代「复跑 N 次等间歇」）：B 开启「收到
org-pull 不应答」黑洞（fault-org-pull-blackhole），A 的 tick S2 反熵对账
每轮对 B 吃满协议读超时（4s/次）。修复前该长链叠加 1s 注入使队列无限
积压；修复后注入合并（至多积压 1 份）+ S2 预算 20s 截断 + S3 恒执行。

断言（凭 A 的 stderr 心跳/发送日志 + join 流程）：
1. join 在窗口内完成（B 收讫预录快照并应答邀请）；
2. 黑洞期间 A 的 orgsync-hello 出站不中断（hello sent 间隔 ≤55s ≈ tick
   总预算 50s + 余量）；
3. worker 队列不积压（KeepaliveTick 完成心跳的 queue_depth 恒 ≤3）；
4. 关闭黑洞后 hello 持续（链路自愈）。
"""

import re
import time
from pathlib import Path

from node import Node, check, poll_until, run_scenario
from scenario_org_orgq import join_org

EVENT_TIMEOUT = 40.0
# tick 总预算上限（10+10+20+10s）+ 观测余量
MAX_HELLO_GAP = 55.0
# 合并语义下队列至多积压 1 份 tick，加上偶发 PushOrg 事件的余量
MAX_QUEUE_DEPTH = 3

HEARTBEAT_RE = re.compile(
    r"\[ORG_SYNC\] request done \| kind=KeepaliveTick elapsed=(\d+)ms queue_depth≈(\d+)"
)
HELLO_RE = re.compile(r"\[ORGSYNC\] hello sent \|")
BUDGET_RE = re.compile(r"tick stage budget exceeded, skipped \| stage=(S\d)")


class LogWatcher:
    """增量读取节点 stderr.log：按行吐出新增内容（日志行无时间戳，
    到达时间由本侧 poll 记录）。"""

    def __init__(self, node):
        self.path = Path(node.data_dir) / "stderr.log"
        self.offset = 0

    def new_lines(self):
        if not self.path.exists():
            return []
        text = self.path.read_text(encoding="utf-8", errors="replace")
        if len(text) <= self.offset:
            return []
        chunk = text[self.offset:]
        self.offset = len(text)
        return [line for line in chunk.splitlines() if line.strip()]


def main():
    a, b = Node("A").start(), Node("B").start()
    nodes = [a, b]
    a.init("Alice")
    b.init("Bob")

    def scenario():
        # ---- B 开启 org-pull 黑洞（对端半连接长超时注入）------------------
        b.set_org_pull_blackhole(True)
        print("  B 已开启 org-pull 黑洞（收到 pull 不应答）")

        # ---- 黑洞开启状态下完成 join（预录快照 + 邀请应答）----------------
        org_id = a.send("org-create", name="F6 故障注入组织")["orgId"]
        join_org(a, b, org_id)
        print(f"  [1/4] 黑洞期间 join 完成: {org_id}")

        # ---- 观察窗：黑洞持续，A 的 hello 出站与 worker 队列 --------------
        watcher = LogWatcher(a)
        hello_times = []
        heartbeats = []  # (elapsed_ms, queue_depth)
        budget_stages = set()
        window = 45.0
        deadline = time.monotonic() + window
        while time.monotonic() < deadline:
            now = time.monotonic()
            for line in watcher.new_lines():
                if HELLO_RE.search(line):
                    hello_times.append(now)
                m = HEARTBEAT_RE.search(line)
                if m:
                    heartbeats.append((int(m.group(1)), int(m.group(2))))
                m = BUDGET_RE.search(line)
                if m:
                    budget_stages.add(m.group(1))
            time.sleep(0.5)

        check(
            len(hello_times) >= 2,
            f"黑洞期间 A 的 orgsync-hello 出站中断（45s 窗内仅 {len(hello_times)} 次）",
        )
        gaps = [b_ - a_ for a_, b_ in zip(hello_times, hello_times[1:])]
        check(
            not gaps or max(gaps) <= MAX_HELLO_GAP,
            f"hello 出站间隔 {max(gaps):.1f}s 超总预算上限 {MAX_HELLO_GAP}s",
        )
        check(
            len(heartbeats) >= 2,
            f"45s 窗内 KeepaliveTick 完成心跳不足（{len(heartbeats)} 条），worker 疑似停滞",
        )
        deepest = max(depth for _, depth in heartbeats)
        check(
            deepest <= MAX_QUEUE_DEPTH,
            f"worker 队列积压：queue_depth 峰值 {deepest} > {MAX_QUEUE_DEPTH}",
        )
        print(
            f"  [2/4] 黑洞期间 hello {len(hello_times)} 次（最大间隔 "
            f"{max(gaps, default=0):.1f}s），tick 心跳 {len(heartbeats)} 条、"
            f"queue_depth 峰值 {deepest}"
        )
        if budget_stages:
            print(f"  [3/4] 阶段预算超时已触发（阶段 {sorted(budget_stages)} 被放弃）")
        else:
            print("  [3/4] 本窗内未触发阶段预算超时（对账链短于 S2 预算，正常）")

        # ---- 关闭黑洞：链路自愈，hello 持续 --------------------------------
        b.set_org_pull_blackhole(False)
        settle = LogWatcher(a)
        deadline = time.monotonic() + 30.0
        healed_hellos = 0
        while time.monotonic() < deadline and healed_hellos < 1:
            for line in settle.new_lines():
                if HELLO_RE.search(line):
                    healed_hellos += 1
            time.sleep(0.5)
        check(healed_hellos >= 1, "关闭黑洞后 30s 内 A 未再发出 orgsync-hello")
        print("  [4/4] 关闭黑洞后 hello 持续（链路自愈）")

    elapsed = run_scenario(scenario, nodes)
    print(f"PASS scenario_org_sync_stall  {elapsed:.1f}s")


if __name__ == "__main__":
    main()
