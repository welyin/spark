#!/usr/bin/env python3
"""场景：同身份多设备（设备配对）。

A 与 A2（助记词恢复出的同身份第二节点）互发申请自动接受（设备配对）
→ A 给自己发消息 → A2 同步可见（自消息跨设备投递）。
"""

import time

from node import Node, check, poll_until, run_scenario


def main():
    a, a2 = Node("A").start(), Node("A2").start()
    nodes = [a, a2]
    a.init("Alice")
    # 第二节点用助记词恢复同一身份（不同数据目录 → 不同 peerId）
    a2.recover(a.mnemonic, "Alice 的设备2")
    check(a2.root_id == a.root_id, "恢复后 rootId 与 A 一致")
    check(a2.peer_id != a.peer_id, "第二节点 peerId 不同")

    def scenario():
        # ---- 设备配对：向自己发申请 → 对端自动接受 --------------------------
        a.send(
            "send-request",
            rootId=a.root_id,
            peerId=a2.peer_id,
            addresses=a2.addresses,
            message="配对我的设备",
        )
        # 自动接受回执：A 的出站申请置 accepted
        a.wait_event(
            "FriendRequestAccepted",
            lambda d: d["request"]["rootId"] == a.root_id
            and d["request"]["status"] == "accepted",
        )
        # 双方都有同 rootId 的设备朋友记录（带对端 peer 寻址）
        device_a = poll_until(
            lambda: a.friend_entry(a.root_id),
            what="A 的设备朋友记录",
        )
        check(
            device_a["peer"]["peerId"] == a2.peer_id,
            "A 侧设备记录指向 A2 的 peerId",
        )
        poll_until(
            lambda: (lambda f: f if f and f.get("peer") else None)(a2.friend_entry(a2.root_id)),
            what="A2 的设备朋友记录",
        )

        # ---- A 给自己发消息 → A2 同步可见 -----------------------------------
        # 与配对 friend-request 错开 1s 限流桶（chat 为内容型 kind；
        # 设备同步投递尽力而为，被限流即丢）
        time.sleep(1.2)
        view = a.send_text(a.root_id, "同步到我的第二台设备", message_id="dev-m1")
        check(view["status"] == "delivered", "自消息本机副本天然 delivered")

        received = a2.wait_event(
            "ChatReceived",
            lambda d: d["message"]["id"] == "dev-m1",
        )
        check(
            received["message"]["senderId"] == "me",
            "A2 侧自消息 senderId 映射为 me",
        )
        check(
            received["conversation"]["unreadCount"] == 0,
            "自消息不产生未读",
        )
        msgs = a2.send("messages", convId=a2.conv_id(a2.root_id))
        check(
            any(m["id"] == "dev-m1" and m["content"] == "同步到我的第二台设备" for m in msgs),
            "A2 消息列表含同步过来的自消息",
        )

    elapsed = run_scenario(scenario, nodes)
    print(f"PASS scenario_devices  {elapsed:.1f}s")


if __name__ == "__main__":
    main()
