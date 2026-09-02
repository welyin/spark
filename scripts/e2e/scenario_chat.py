#!/usr/bin/env python3
"""场景：单聊消息。

A↔B 文本消息（ChatReceived + 未读数）→ B mark-read → A 收 ChatStatus(peerRead)
→ A 撤回 → B 收撤回事件；B 离线时 A 发消息 → 保持 sending 入 dm:pending 离线
队列（social-feed §6.4 新语义：不再置 failed）→ B 上线 → A resend 手动兜底 →
delivered；再由 B 主动发消息触发连接重建，验证 on_peer_app_ready 事件驱动
flush 自动补投第二条离线消息；超长 link 字段截断与非法 url 丢弃断言。

注意：dm 入站 1s 限流桶（内容型 kind 共享），脚本消息间隔远低于真人，
发送一律走 send_chat_reliably（failed 后等 1.2s 重发）包裹。
"""

from node import (
    Node,
    check,
    make_friends,
    resend_reliably,
    run_scenario,
    send_chat_reliably,
    wait_message_status,
)

import time

LINK_TITLE_MAX = 256


def main():
    a, b = Node("A").start(), Node("B").start()
    nodes = [a, b]
    a.init("Alice")
    b.init("Bob")

    def scenario():
        make_friends(a, b)
        conv_a = a.conv_id(b.root_id)
        conv_b = b.conv_id(a.root_id)

        # ---- 文本消息 + 未读数 ------------------------------------------
        send_chat_reliably(a, b.root_id, "你好 Bob", "m1")
        received = b.wait_event(
            "ChatReceived", lambda d: d["message"]["id"] == "m1"
        )
        check(received["message"]["content"] == "你好 Bob", "B 收到消息正文")
        check(received["conversation"]["unreadCount"] == 1, "事件快照未读 +1")
        convs = b.send("conversations")
        conv = next(c for c in convs if c["id"] == conv_b)
        check(conv["unreadCount"] == 1, "会话列表未读为 1")

        # ---- mark-read → A 收 peerRead ----------------------------------
        b.send("mark-read", convId=conv_b)
        status = a.wait_event(
            "ChatStatus", lambda d: d.get("peerRead") is True and d.get("convId") == conv_a
        )
        check(status["peerRead"] is True, "A 应收 peerRead")
        mine = a.send("messages", convId=conv_a)
        check(
            next(m for m in mine if m["id"] == "m1")["status"] == "read",
            "A 侧消息置已读",
        )

        # ---- 撤回 --------------------------------------------------------
        recalled = a.send("recall", convId=conv_a, messageId="m1")
        check(recalled["recalled"] is True, "窗口内撤回成功")
        b.wait_event(
            "ChatStatus",
            lambda d: d.get("recalled") is True and d.get("messageId") == "m1",
        )
        theirs = b.send("messages", convId=conv_b)
        check(next(m for m in theirs if m["id"] == "m1")["recalled"] is True, "B 侧消息已撤回")

        # ---- link：超长字段截断 / 非法 url 丢弃 ---------------------------
        long_title = "题" * 500
        sent = send_chat_reliably(
            a,
            b.root_id,
            "看这个链接",
            "m2",
            link={
                "url": "https://example.com/page",
                "title": long_title,
                "description": "desc",
                "siteName": "Example",
                "domain": "example.com",
            },
        )
        check(sent["link"] is not None, "合法 link 保留")
        check(
            len(sent["link"]["title"]) == LINK_TITLE_MAX,
            f"title 截断到 {LINK_TITLE_MAX} 字符，实际 {len(sent['link']['title'])}",
        )
        received = b.wait_event("ChatReceived", lambda d: d["message"]["id"] == "m2")
        check(
            len(received["message"]["link"]["title"]) == LINK_TITLE_MAX,
            "对端收到的 link title 同为截断值",
        )

        sent = send_chat_reliably(
            a,
            b.root_id,
            "非法链接",
            "m3",
            link={
                "url": "javascript:alert(1)",
                "title": "x",
                "description": "",
                "siteName": "",
                "domain": "",
            },
        )
        check(sent.get("link") is None, "非 http(s) url 的 link 应被丢弃")

        # ---- 离线入队（sending，不置 failed）→ 上线 resend 兜底 ------------
        # social-feed §6.4 新语义：投递失败入 dm:pending 离线队列，状态保持
        # sending 不置 failed；补投事件驱动（on_peer_app_ready flush），
        # resend 为手动兜底。
        b.stop_p2p()
        a.send_text(b.root_id, "离线消息", message_id="m4")
        b.expect_no_event(
            "ChatReceived", seconds=4.0, pred=lambda d: d["message"]["id"] == "m4"
        )
        m4 = next(m for m in a.send("messages", convId=conv_a) if m["id"] == "m4")
        check(
            m4["status"] == "sending",
            f"离线消息应保持 sending 入队（不置 failed），实际 {m4['status']}",
        )

        b.send("start-p2p")
        b.refresh_p2p()
        resend_reliably(a, conv_a, "m4")
        received = b.wait_event(
            "ChatReceived", lambda d: d["message"]["id"] == "m4"
        )
        check(received["message"]["content"] == "离线消息", "B 收到重发的消息")

        # ---- 事件驱动自动补投：B 离线期间 A 再发 m5（sending 入队）；
        #      B 上线后主动向 A 发消息（触发 B→A 连接重建 + 应用层就绪探测），
        #      A 侧 on_peer_app_ready flush 自动补投 m5 —— 无 resend --------
        b.stop_p2p()
        a.send_text(b.root_id, "第二条离线消息", message_id="m5")
        m5 = next(m for m in a.send("messages", convId=conv_a) if m["id"] == "m5")
        check(m5["status"] == "sending", f"m5 应保持 sending，实际 {m5['status']}")
        b.send("start-p2p")
        b.refresh_p2p()
        time.sleep(1.2)  # 与 m4 重发错开 1s 限流桶
        send_chat_reliably(b, a.root_id, "我回来了", "m5-probe")
        # 无 resend：flush 自动补投 → A 侧 m5 置 delivered，B 收到 m5
        wait_message_status(a, conv_a, "m5", "delivered")
        received = b.wait_event(
            "ChatReceived", lambda d: d["message"]["id"] == "m5"
        )
        check(received["message"]["content"] == "第二条离线消息", "B 收到自动补投的消息")

    elapsed = run_scenario(scenario, nodes)
    print(f"PASS scenario_chat  {elapsed:.1f}s")


if __name__ == "__main__":
    main()
