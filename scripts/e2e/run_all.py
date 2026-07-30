#!/usr/bin/env python3
"""串行跑全部 e2e 场景并输出汇总。

用法：python3 scripts/e2e/run_all.py [场景名过滤...]
前置：cd code/core && cargo build --example e2e_node
"""

import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
# 单场景硬超时（秒）：超出即杀，记 FAIL
SCENARIO_TIMEOUT = 180


def main():
    filters = sys.argv[1:]
    scenarios = sorted(HERE.glob("scenario_*.py"))
    if filters:
        scenarios = [s for s in scenarios if any(f in s.name for f in filters)]
    if not scenarios:
        print("没有匹配的场景")
        return 1

    results = []
    for script in scenarios:
        name = script.stem
        started = time.monotonic()
        proc = subprocess.Popen(
            [sys.executable, str(script)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        try:
            output, _ = proc.communicate(timeout=SCENARIO_TIMEOUT)
            status = "PASS" if proc.returncode == 0 else "FAIL"
        except subprocess.TimeoutExpired:
            proc.kill()
            output, _ = proc.communicate()
            status = "FAIL(timeout)"
        elapsed = time.monotonic() - started
        results.append((name, status, elapsed))
        print(f"[{status}] {name}  {elapsed:.1f}s")
        if proc.returncode != 0 or status.startswith("FAIL"):
            print(output[-4000:])

    print("\n===== e2e 汇总 =====")
    failed = 0
    for name, status, elapsed in results:
        print(f"{status:>13}  {name:<28} {elapsed:6.1f}s")
        if status != "PASS":
            failed += 1
    print(f"共 {len(results)} 个场景，{len(results) - failed} 通过，{failed} 失败")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
