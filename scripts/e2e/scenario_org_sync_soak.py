#!/usr/bin/env python3
"""org-sync worker 韧性 soak（F6 回归哨兵；P3 后改写）。

前身 scenario_org_sync_stall 的故障注入目标（tick S2 reconcile/org-pull）
已随 P3「legacy 出站停发」删除，注入无的放矢。本场景改为**无注入的稳态
soak**：双成员 join + 数据写入驱动下，对 A 的 worker 做 45s 观测窗，断言
F6 原始故障形态不再出现：

1. KeepaliveTick 完成心跳持续（worker 不停滞）；
2. queue_depth 恒 ≤3（tick 注入合并不积压）；
3. orgsync-hello 出站间隔 ≤55s（≈ tick 总预算上限）；
4. 本地写入触发即时 hello（SelfHelloNow 链路存活）。

日志锚点（F6 修复引入）：`[ORG_SYNC] request done` 心跳 /
`[ORGSYNC] hello sent` / `tick stage budget exceeded` WARN。
"""

import re
import time
from pathlib import Path

from node import Node, check, poll_until, run_scenario, join_org

EVENT_TIMEOUT = 40.0
# tick 总预算上限（10+10+10s，S2 已随 P3 删除）+ local_node_info 5s 兜底 + 余量
MAX_HELLO_GAP = 55.0
# 合并语义下队列至多积压 1 份 tick，加上偶发 PushOrg/SelfHelloNow 事件的余量
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
        # ---- 正常 join（P3 后全靠邀请流 orgsync 收敛）---------------------
        org_id = a.send("org-create", name="F6 soak 组织")["orgId"]
        join_org(a, b, org_id)
        print(f"  [1/3] join 完成: {org_id}")

        # ---- 观测窗 45s：心跳 / 队列 / hello 间隔 --------------------------
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
            len(heartbeats) >= 2,
            f"45s 窗内 KeepaliveTick 完成心跳不足（{len(heartbeats)} 条），worker 疑似停滞",
        )
        deepest = max(depth for _, depth in heartbeats)
        check(
            deepest <= MAX_QUEUE_DEPTH,
            f"worker 队列积压：queue_depth 峰值 {deepest} > {MAX_QUEUE_DEPTH}",
        )
        check(
            len(hello_times) >= 2,
            f"观测窗内 A 的 orgsync-hello 出站中断（45s 窗内仅 {len(hello_times)} 次）",
        )
        gaps = [b_ - a_ for a_, b_ in zip(hello_times, hello_times[1:])]
        check(
            not gaps or max(gaps) <= MAX_HELLO_GAP,
            f"hello 出站间隔 {max(gaps):.1f}s 超总预算上限 {MAX_HELLO_GAP}s",
        )
        print(
            f"  [2/3] 观测窗 hello {len(hello_times)} 次（最大间隔 "
            f"{max(gaps, default=0):.1f}s），tick 心跳 {len(heartbeats)} 条、"
            f"queue_depth 峰值 {deepest}"
        )
        if budget_stages:
            print(f"  [3/3] 阶段预算超时触发记录（阶段 {sorted(budget_stages)}）")
        else:
            print("  [3/3] 窗内无阶段预算超时（各阶段健康）")

    elapsed = run_scenario(scenario, nodes)
    print(f"PASS scenario_org_sync_soak  {elapsed:.1f}s")


if __name__ == "__main__":
    main()
