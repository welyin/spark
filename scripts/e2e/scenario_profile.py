#!/usr/bin/env python3
"""场景：资料变更同步。

A update-profile 改昵称 → B 收 FriendProfileUpdated → B 的朋友记录刷新为新昵称。
（profile-sync 是内容型 dm，与好友申请共享 1s 限流桶——配对后稍候再改资料。）
"""

import time

from node import Node, check, make_friends, poll_until, run_scenario


def main():
    a, b = Node("A").start(), Node("B").start()
    nodes = [a, b]
    a.init("Alice")
    b.init("Bob")

    def scenario():
        make_friends(a, b)
        # 避开与 friend-request 的 1s 限流桶冲突
        time.sleep(1.2)

        profile = a.send("update-profile", nickname="爱丽丝二世")
        check(profile["nickname"] == "爱丽丝二世", "本地资料已更新")

        updated = b.wait_event(
            "FriendProfileUpdated",
            lambda d: d.get("rootId") == a.root_id,
        )
        check(updated.get("nickname") == "爱丽丝二世", "事件携带新昵称")

        friend = poll_until(
            lambda: (lambda f: f if f and f["nickname"] == "爱丽丝二世" else None)(
                b.friend_entry(a.root_id)
            ),
            what="B 的朋友记录刷新昵称",
        )
        check(friend["nickname"] == "爱丽丝二世", "B 侧朋友昵称已刷新")

    elapsed = run_scenario(scenario, nodes)
    print(f"PASS scenario_profile  {elapsed:.1f}s")


if __name__ == "__main__":
    main()
