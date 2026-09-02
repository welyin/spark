#!/usr/bin/env python3
"""O2 验收·双端组织数据同步（靶心：删除后再写的折叠失明回归）。

A（管理员/数据账号）+ B（成员）两端，all-members 集合（复制组 = 全员）：

  A 建组织 → B 加入 → A 声明 org 集合（同步到 B）→
  A 写 k1 → B 经 orgsync 收讫 → A 删 k1（墓碑）→ B 收讫墓碑 →
  A 同集合再写 k2 → **B 必须收到 k2**（折叠失明回归断言）→
  双端折叠 vv 收敛一致（org-fold-vv 与 orgsync-hello 摘要同口径）。
"""

from node import Node, NodeError, check, poll_until, run_scenario

EVENT_TIMEOUT = 40.0


def poll_ok(fn, what, timeout=EVENT_TIMEOUT):
    """poll_until 的容错变体：NodeError（如声明尚未同步到对端）视为未就绪。"""

    def probe():
        try:
            return fn()
        except NodeError:
            return None

    return poll_until(probe, timeout=timeout, what=what)


def join_org(admin, member, org_name):
    """admin 建组织并邀 member 加入（全流程），返回 orgId。"""
    org = admin.send("org-create", name=org_name)
    org_id = org["orgId"]
    admin.send(
        "org-add-member",
        orgId=org_id,
        rootId=member.root_id,
        nodeInfo={"peerId": member.peer_id, "addresses": member.addresses},
    )
    invite = admin.send(
        "org-send-invite",
        orgId=org_id,
        targetRootId=member.root_id,
        targetNickname=member.name,
    )
    check(invite["status"] == "pending", f"{admin.name} 出站邀请初始 pending")
    received = member.wait_event(
        "OrgInviteReceived", lambda d: d.get("orgId") == org_id, timeout=EVENT_TIMEOUT
    )
    # 等管理员快照推送落地再应答（接受编排 pull-list 有 per-peer 限流，见 scenario_org）
    poll_until(
        lambda: any(o["orgId"] == org_id for o in member.send("org-list")),
        timeout=EVENT_TIMEOUT,
        what=f"{member.name} 收到预录组织快照",
    )
    responded = member.send(
        "org-respond-invite", inviteId=received["id"], accept=True
    )
    check(responded["status"] == "accepted", f"{member.name} 侧邀请记录置 accepted")
    admin.wait_event(
        "OrgInviteUpdated",
        lambda d: d.get("orgId") == org_id and d.get("status") == "accepted",
        timeout=EVENT_TIMEOUT,
    )
    return org_id


def get_value(node, domain, name, key, org_id):
    return node.send("data-get", domain=domain, name=name, key=key, orgId=org_id)["value"]


def fold_vv(node, org_id, name):
    return node.send("org-fold-vv", orgId=org_id, name=name)["vv"]


def main():
    a, b = Node("A").start(), Node("B").start()
    nodes = [a, b]
    a.init("Alice")
    b.init("Bob")

    def scenario():
        org_id = join_org(a, b, "O2 验收组织")
        print(f"  组织已建立，B 已加入: {org_id}")

        # ---- A 声明 all-members org 集合 → 声明同步到 B --------------------
        domain, name = "e2e", "e2e:ledger"
        a.send(
            "data-declare",
            domain=domain,
            name=name,
            orgId=org_id,
            accounts="all-members",
            confidentiality="filtered",
        )

        def b_has_decl():
            try:
                b.send("data-query", domain=domain, name=name, orgId=org_id)
                return True
            except NodeError:
                return None

        poll_until(b_has_decl, timeout=EVENT_TIMEOUT, what="B 收到集合声明（声明先行）")
        print("  集合声明已同步到 B")

        # ---- 靶心 1/3：A 写 k1 → B 收讫 ----------------------------------
        a.send(
            "data-save", domain=domain, name=name, key="k1", value={"n": 1}, orgId=org_id
        )
        got = poll_ok(
            lambda: get_value(b, domain, name, "k1", org_id) == {"n": 1} or None,
            "B 经 orgsync 收讫 k1",
        )
        check(got, "B 收讫 k1")
        print("  [1/3] A 写 k1 → B 已收讫")

        # ---- 靶心 2/3：A 删 k1（墓碑）→ B 收讫墓碑 ------------------------
        a.send("data-delete", domain=domain, name=name, key="k1", orgId=org_id)
        poll_ok(
            lambda: get_value(b, domain, name, "k1", org_id) is None or None,
            "B 经 orgsync 收讫 k1 墓碑",
        )
        print("  [2/3] A 删 k1（墓碑）→ B 已收讫墓碑")

        # ---- 靶心 3/3：A 同集合再写 k2 → B 必须收到（修复前折叠失明丢弃）----
        a.send(
            "data-save", domain=domain, name=name, key="k2", value={"n": 2}, orgId=org_id
        )
        got = poll_ok(
            lambda: get_value(b, domain, name, "k2", org_id) == {"n": 2} or None,
            "B 收到墓碑后的新写 k2（折叠失明回归）",
        )
        check(got, "B 必须收到 k2")
        print("  [3/3] A 删后再写 k2 → B 已收讫（折叠失明回归通过）")

        # ---- 双端折叠 vv 收敛一致 ------------------------------------------
        def vv_converged():
            va, vb = fold_vv(a, org_id, name), fold_vv(b, org_id, name)
            return va == vb and va or None

        vv = poll_until(
            vv_converged, timeout=EVENT_TIMEOUT, what="双端折叠 vv 收敛一致"
        )
        check(vv, "折叠 vv 非空")
        print(f"  双端折叠 vv 收敛一致: {vv}")

    elapsed = run_scenario(scenario, nodes)
    print(f"PASS scenario_org_data  {elapsed:.1f}s")


if __name__ == "__main__":
    main()
