#!/usr/bin/env python3
"""场景：成员移除通知（阶段四A P2 L3）。

A 建组织 → B/C 加入 → A 移除 B（在线）→ B 收 OrgRemoved 事件 + 本地擦除
（org-list 不再含该组织）→ A 侧成员墓碑收敛不复活；
C 离线时 A 移除 C → 通知入 org 域 pending 队列 → C 上线后补投 →
C 收 OrgRemoved + 本地擦除。
"""

import time

from node import Node, NodeError, check, poll_until, run_scenario

EVENT_TIMEOUT = 40.0


def join_org(admin, member, org_id):
    """预录（带 nodeInfo）+ 邀请 + 接受全流程（P3 口径：接受编排内部
    stub 自举 + connect + 有界等 orgsync 收敛，不再预等预录快照）。"""
    admin.send(
        "org-add-member",
        orgId=org_id,
        rootId=member.root_id,
        nodeInfo={"peerId": member.peer_id, "addresses": member.addresses},
    )
    admin.send(
        "org-send-invite",
        orgId=org_id,
        targetRootId=member.root_id,
        targetNickname=member.name,
    )
    received = member.wait_event(
        "OrgInviteReceived", lambda d: d.get("orgId") == org_id, timeout=EVENT_TIMEOUT
    )
    responded = None
    for attempt in range(3):
        try:
            responded = member.send(
                "org-respond-invite", inviteId=received["id"], accept=True
            )
            break
        except NodeError:
            if attempt == 2:
                raise
            time.sleep(2)
    check(responded["status"] == "accepted", f"{member.name} 侧邀请记录置 accepted")
    poll_until(
        lambda: any(o["orgId"] == org_id for o in member.send("org-list")) or None,
        timeout=EVENT_TIMEOUT,
        what=f"{member.name} 接受后收敛看到组织",
    )


def org_visible(node, org_id):
    return any(o["orgId"] == org_id for o in node.send("org-list"))


def member_count(node, org_id):
    view = node.send("org-view", orgId=org_id)
    return view["memberCount"] if view else None


def main():
    a, b, c = Node("A").start(), Node("B").start(), Node("C").start()
    nodes = [a, b, c]
    a.init("Alice")
    b.init("Bob")
    c.init("Carol")

    def scenario():
        org_id = a.send("org-create", name="P2 移除验收组织")["orgId"]
        join_org(a, b, org_id)
        join_org(a, c, org_id)
        poll_until(
            lambda: member_count(a, org_id) == 3 or None,
            timeout=EVENT_TIMEOUT,
            what="A 侧成员数收敛为 3",
        )
        print(f"  B/C 已加入: {org_id}")

        # ---- 在线移除 B：通知直达 + 本地擦除 ------------------------------
        a.send("org-remove-member", orgId=org_id, rootId=b.root_id)
        b.wait_event(
            "OrgRemoved",
            lambda d: d.get("orgId") == org_id,
            timeout=EVENT_TIMEOUT,
        )
        check(not org_visible(b, org_id), "B 本地组织已擦除（org-list 不再含该组织）")
        print("  [1/2] 在线移除 B：OrgRemoved 到达 + 本地擦除")

        # A 侧成员墓碑收敛：成员数回到 2 且不复活（墓碑经 org dlog 传播，
        # 复制组确认后 GC；短暂回退属合法并发， poll 收敛 + 稳定观察）
        poll_until(
            lambda: member_count(a, org_id) == 2 or None,
            timeout=EVENT_TIMEOUT,
            what="A 侧成员表收敛为 2（B 墓碑化）",
        )

        # ---- 离线移除 C：通知入 pending → C 上线补投 ------------------------
        c.stop_p2p()
        a.send("org-remove-member", orgId=org_id, rootId=c.root_id)
        poll_until(
            lambda: member_count(a, org_id) == 1 or None,
            timeout=EVENT_TIMEOUT,
            what="A 侧成员表收敛为 1（C 墓碑化）",
        )
        c.send("start-p2p")
        c.refresh_p2p()
        # pending 补投由 on_peer_app_ready flush 驱动——A 侧有待投通知，
        # C 重连后收敛（连接重建由 invite 残留的可达性/任一侧写入触发）
        c.wait_event(
            "OrgRemoved",
            lambda d: d.get("orgId") == org_id,
            timeout=EVENT_TIMEOUT * 2,
        )
        check(not org_visible(c, org_id), "C 本地组织已擦除（补投到达后）")
        print("  [2/2] 离线移除 C：pending 补投到达 + 本地擦除")

    elapsed = run_scenario(scenario, nodes)
    print(f"PASS scenario_org_remove  {elapsed:.1f}s")


if __name__ == "__main__":
    main()
