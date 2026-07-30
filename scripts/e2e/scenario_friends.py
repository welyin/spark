#!/usr/bin/env python3
"""场景：好友申请全流程。

- A→B：互换名片（make/import-node-card）→ 申请 → B 收 FriendRequestReceived
  → B 接受 → A 收 FriendRequestAccepted → 双向 FriendRecord 断言；
- A→C：申请 → C reply-request 询问 → A 收 replied → A 答复 → C 接受；
- D：被 B 拉黑 → D 发申请/消息均被 blocked（B 无事件、D 侧 failed）。
"""

from node import Node, NodeError, check, make_friends, poll_until, run_scenario

EVENT_TIMEOUT = 25.0


def main():
    a, b, c, d = Node("A").start(), Node("B").start(), Node("C").start(), Node("D").start()
    nodes = [a, b, c, d]
    a.init("Alice")
    b.init("Bob")
    c.init("Carol")
    d.init("Dave")

    def scenario():
        # ---- A→B：名片互换 → 申请 → 接受 --------------------------------
        card_b = b.send("make-node-card")["card"]
        imported = a.send("import-node-card", card=card_b)
        check(imported["peerId"] == b.peer_id, "导入名片 peerId 应指向 B")
        check(imported["connectError"] is None, f"A 应能连上 B: {imported}")

        a.send(
            "send-request",
            rootId=b.root_id,
            peerId=b.peer_id,
            addresses=b.addresses,
            message="我是 Alice",
        )
        received = b.wait_event(
            "FriendRequestReceived", lambda d: d["request"]["rootId"] == a.root_id
        )
        check(
            received["request"]["id"].startswith(f"{a.root_id}:"),
            "入站申请 id 应为复合形式 {from}:{requestId}",
        )
        b.send("accept-request", requestId=received["request"]["id"])
        accepted = a.wait_event(
            "FriendRequestAccepted", lambda d: d["friend"]["rootId"] == b.root_id
        )
        check(accepted["request"]["status"] == "accepted", "A 的出站申请应置 accepted")

        friend_ab = a.friend_entry(b.root_id)
        friend_ba = b.friend_entry(a.root_id)
        check(friend_ab is not None, "A 的朋友列表应有 B")
        check(friend_ba is not None, "B 的朋友列表应有 A")
        check(friend_ba["nickname"] == "Alice", "B 侧朋友昵称取申请方昵称")
        check(friend_ab["peer"]["peerId"] == b.peer_id, "A 侧朋友记录带 B 的 peerId")

        # ---- A→C：申请 → 接受（reply 往返见下方 xfail 说明）--------------
        outgoing = a.send(
            "send-request",
            rootId=c.root_id,
            peerId=c.peer_id,
            addresses=c.addresses,
            message="加个好友",
        )
        received_c = c.wait_event(
            "FriendRequestReceived", lambda d: d["request"]["rootId"] == a.root_id
        )
        # xfail（协议层缺口，非绕过）：friend-reply 的「接收方主动发起询问」
        # 出站命令内核未实装（wiki/protocol/p2p-messages.md §19.2 已知边界③；
        # contact_reply_request 只操作出站申请、且要求 replied 状态）。
        # 因此「C 询问 → A 收 replied → A 答复」无法经内核 API 端到端驱动，
        # 这里只断言守卫：pending 状态下答复报「当前状态不可回复」。
        try:
            a.send("reply-request", requestId=outgoing["id"], text="提前答复")
            raise AssertionError("pending 状态下 reply-request 应被拒绝")
        except NodeError as e:
            check("当前状态不可回复" in str(e), f"守卫文案不符: {e}")

        c.send("accept-request", requestId=received_c["request"]["id"])
        a.wait_event(
            "FriendRequestAccepted", lambda d: d["friend"]["rootId"] == c.root_id
        )
        check(a.friend_entry(c.root_id) is not None, "A 的朋友列表应有 C")
        check(c.friend_entry(a.root_id) is not None, "C 的朋友列表应有 A")

        # ---- D 被 B 拉黑：申请与消息都被 blocked --------------------------
        b.send("block-root", rootId=d.root_id, blocked=True)
        d.send(
            "send-request",
            rootId=b.root_id,
            peerId=b.peer_id,
            addresses=b.addresses,
            message="是我",
        )
        # B 不生成入站申请；D 的出站申请投递被拒置 failed
        b.expect_no_event(
            "FriendRequestReceived",
            seconds=8.0,
            pred=lambda d_: d_["request"]["rootId"] == d.root_id,
        )
        poll_until(
            lambda: any(
                r["rootId"] == b.root_id and r["status"] == "failed"
                for r in d.overview()["outgoing"]
            ),
            timeout=EVENT_TIMEOUT,
            what="D 的出站申请置 failed",
        )
        # D 直接发消息也被拉黑拒收（status failed，B 无 ChatReceived）
        view = d.send_text(b.root_id, "在吗")
        check(view["status"] == "failed", f"拉黑后 D 的消息应 failed，得到 {view['status']}")
        b.expect_no_event(
            "ChatReceived",
            seconds=5.0,
            pred=lambda d_: d_["message"]["senderId"] == d.root_id,
        )

    elapsed = run_scenario(scenario, nodes)
    print(f"PASS scenario_friends  {elapsed:.1f}s")


if __name__ == "__main__":
    main()
