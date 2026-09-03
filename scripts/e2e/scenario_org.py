#!/usr/bin/env python3
"""场景：组织邀请与资料同步 + P2 join 新通道。

A 建组织 → 预录 B（nodeInfo）→ org-send-invite → B 收 OrgInviteReceived +
sys:notice 系统会话链接卡片 → B org-respond-invite(accept) → B org-list 有该组织
→ A 收 OrgInviteUpdated(accepted) → B 改组织昵称 → A 经快照同步可见 →
A 改组织 logo → B 可见 →
**P2 L1/L2**：成员 E 不预录寻址（邀请走显式 peerId/addresses 直达）→ E 经
orgsync 收敛看到组织（join 不再走 legacy pull 编排）→ E 接受后自写成员条目
（claim 退役）→ A 装配视图看到 E 的端点回填。
"""

import time

from node import Node, NodeError, check, poll_until, run_scenario

# 1x1 PNG data URL（内核 avatar 校验要求 data:image/ 前缀）
TINY_PNG = (
    "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAD"
    "UlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
)


def main():
    a, b = Node("A").start(), Node("B").start()
    e = Node("E").start()
    nodes = [a, b, e]
    a.init("Alice")
    b.init("Bob")
    e.init("Eve")

    def scenario():
        # ---- 建组织 + 预录 B（带真实 nodeInfo，推送可直连送达）------------
        org = a.send("org-create", name="E2E 组织")
        org_id = org["orgId"]
        a.send(
            "org-add-member",
            orgId=org_id,
            rootId=b.root_id,
            nodeInfo={"peerId": b.peer_id, "addresses": b.addresses},
        )
        # 立即连发邀请：首投若与 org-share 推送拨号竞争失败，内核
        # spawn_deliveries_with_retry 退避重试兜底（+2s/+5s）

        # ---- 经 DM 发邀请 → B 收事件 + 系统会话卡片 ------------------------
        invite = a.send(
            "org-send-invite",
            orgId=org_id,
            targetRootId=b.root_id,
            targetNickname="Bob",
        )
        check(invite["direction"] == "outgoing", "A 侧为出站邀请")
        check(invite["status"] == "pending", "出站邀请初始 pending")

        received = b.wait_event(
            "OrgInviteReceived", lambda d: d.get("orgId") == org_id
        )
        invite_id = received["id"]
        check(received["status"] == "pending", "入站邀请 pending")
        check(received.get("inviteCode"), "入站记录携带邀请码")

        card = b.wait_event(
            "ChatReceived",
            lambda d: d["conversation"]["id"] == "sys:notice"
            and d["message"]["id"] == f"org-invite-{invite_id}",
        )
        check(
            card["message"]["link"]["url"] == f"spark-org-invite://{invite_id}",
            "系统会话卡片为组织邀请链接",
        )
        check(card["message"]["link"]["title"] == "E2E 组织", "卡片标题为组织名")

        # ---- B 接受 → 双方状态收敛 ----------------------------------------
        # 接受编排是 pull-list + pull-org 连发（org-pull 有 per-peer 限流），
        # B 无本地记录时全靠拉取会确定性被限；真实流程里管理员的快照推送
        # 先于用户点确认到达，这里等推送落地再应答（邀请 DM 仍是预录后
        # 立即连发，重试兜底已在上面卡片断言中验证）
        poll_until(
            lambda: any(o["orgId"] == org_id for o in b.send("org-list")),
            what="B 收到预录组织快照",
        )
        # 接受触发凭码加入编排（connectAndPull）：失败保持 pending 可安全
        # 重试（等价 UI 用户再点一次确认）
        responded = None
        for attempt in range(3):
            try:
                responded = b.send("org-respond-invite", inviteId=invite_id, accept=True)
                break
            except NodeError:
                if attempt == 2:
                    raise
                time.sleep(2)
        check(responded["status"] == "accepted", "B 侧邀请记录置 accepted")
        mine_b = b.send("org-list")
        org_b = next((o for o in mine_b if o["orgId"] == org_id), None)
        check(org_b is not None, "B 的组织列表应有该组织")
        check(org_b["memberCount"] == 2, "B 侧成员数为 2")
        check(org_b["isCurrentUserAdmin"] is False, "B 为 member 角色")

        a.wait_event(
            "OrgInviteUpdated",
            lambda d: d.get("orgId") == org_id and d.get("status") == "accepted",
        )

        # ---- B 改组织内昵称 → A 经快照同步可见 ------------------------------
        b.send("org-update-my-identity", orgId=org_id, nickname="组织里的 Bob")

        def a_sees_nickname():
            view = a.send("org-view", orgId=org_id)
            if not view:
                return None
            member = next((m for m in view["members"] if m["rootId"] == b.root_id), None)
            if member and member.get("nickname") == "组织里的 Bob":
                return member
            return None

        poll_until(a_sees_nickname, what="A 经快照同步看到 B 的组织昵称")

        # ---- A 改组织 logo → B 可见 ----------------------------------------
        updated = a.send("org-update-info", orgId=org_id, avatar=TINY_PNG)
        check(updated["avatar"] == TINY_PNG, "A 侧 logo 已更新")

        def b_sees_avatar():
            view = b.send("org-view", orgId=org_id)
            return view if view and view.get("avatar") == TINY_PNG else None

        poll_until(b_sees_avatar, what="B 经快照同步看到组织新 logo")

        # ---- P2 L1/L2：无预录寻址的成员 E——邀请经显式寻址直达，接受走
        #      orgsync 收敛（L1），E 自写成员条目回填 nodeInfo（L2，claim
        #      通道已退役）→ A 经装配视图看到 E 的端点 ----------------------
        a.send("org-add-member", orgId=org_id, rootId=e.root_id)  # 不带 nodeInfo
        invite_e = a.send(
            "org-send-invite",
            orgId=org_id,
            targetRootId=e.root_id,
            targetPeerId=e.peer_id,
            targetAddresses=e.addresses,
            targetNickname="Eve",
        )
        check(invite_e["status"] == "pending", "E 的出站邀请初始 pending")
        received_e = e.wait_event(
            "OrgInviteReceived", lambda d: d.get("orgId") == org_id
        )
        # L1：E 无预录寻址，接受编排内部完成 stub 自举 + connect + 有界等待
        # orgsync 收敛（替代 legacy pull 编排）——直接应答，不预等快照
        responded_e = None
        for attempt in range(3):
            try:
                responded_e = e.send("org-respond-invite", inviteId=received_e["id"], accept=True)
                break
            except NodeError:
                if attempt == 2:
                    raise
                time.sleep(2)
        check(responded_e["status"] == "accepted", "E 侧邀请记录置 accepted")
        poll_until(
            lambda: any(o["orgId"] == org_id for o in e.send("org-list")) or None,
            what="E 接受后经 orgsync 收敛看到组织（L1）",
        )

        # L2：E 接受后自写成员条目（含自身端点）→ A 装配视图可见
        def a_sees_e_endpoint():
            view = a.send("org-view", orgId=org_id)
            if not view:
                return None
            member = next((m for m in view["members"] if m["rootId"] == e.root_id), None)
            if member and member.get("nodeInfo"):
                return member
            return None

        poll_until(a_sees_e_endpoint, what="A 看到 E 自写条目的端点回填（L2）")

    elapsed = run_scenario(scenario, nodes)
    print(f"PASS scenario_org  {elapsed:.1f}s")


if __name__ == "__main__":
    main()
