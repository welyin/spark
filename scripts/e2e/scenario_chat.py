#!/usr/bin/env python3
"""场景：单聊消息。

A↔B 文本消息（ChatReceived + 未读数）→ B mark-read → A 收 ChatStatus(peerRead)
→ A 撤回 → B 收撤回事件；B 离线时 A 发消息 → failed → B 上线 → A resend →
delivered；超长 link 字段截断与非法 url 丢弃断言。

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

        # ---- 离线 failed → 上线 resend → delivered ------------------------
        b.stop_p2p()
        a.send_text(b.root_id, "离线消息", message_id="m4")
        wait_message_status(a, conv_a, "m4", "failed")

        b.send("start-p2p")
        b.refresh_p2p()
        resend_reliably(a, conv_a, "m4")
        received = b.wait_event(
            "ChatReceived", lambda d: d["message"]["id"] == "m4"
        )
        check(received["message"]["content"] == "离线消息", "B 收到重发的消息")

    elapsed = run_scenario(scenario, nodes)
    print(f"PASS scenario_chat  {elapsed:.1f}s")


if __name__ == "__main__":
    main()
