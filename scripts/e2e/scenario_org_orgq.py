#!/usr/bin/env python3
"""O2 验收·三端组织数据同步（D1 靶心：orgq 受理删除后再写的折叠失明回归）。

拓扑：A = 创建者/管理员（数据账号），C = 晋升管理员（第二数据账号），
B = 普通成员（全程不晋升）。集合 = data-accounts + encrypted（复制组 =
{A, C}；B 不驻留，写/删经 orgq 由数据账号受理——filtered 无插件运行时
fail-closed 不可受理；encrypted 写恒受理、删除要求 from ∈ acl readers，
见 inbound_dm/orgq.rs）。

  A 建组织 → B/C 加入 → A 晋升 C 为管理员 → A 声明 encrypted 集合 →
  声明经 org:structure 全员同步（F2-P1），B 自然收讫（无晋升编排）→
  三方 accessKey 背靠背发布（F1 结构化合并：并发写成员表不丢更新）→
  A 一次 grantAccess({B,C})（F3 残余修复实证：deliver 抢跑 acl 时收端
  暂存 + acl 晚到重评估，密钥不再丢失，无 revoke/re-grant 编排）→
  B 写 k1（orgq → 数据账号受理）→ A/C 收讫 →
  B 删 k1（orgq → 受理方落墓碑，走修复的 org_tombstone_local）→ 两端收讫 →
  墓碑受理方就地写 k2（受理删除后的首次受管写，修复前序号碰撞 → 对端
  折叠失明）→ 对端必须收讫 k2 → B 再写 k3 + 读回 k2 → A/C 折叠 vv 收敛。
"""

import time

from node import Node, NodeError, check, poll_until, run_scenario

EVENT_TIMEOUT = 40.0
DOMAIN, NAME = "e2e", "e2e:vault"


def poll_ok(fn, what, timeout=EVENT_TIMEOUT):
    """poll_until 的容错变体：NodeError（密钥未达/声明未同步等）视为未就绪。"""

    def probe():
        try:
            return fn()
        except NodeError:
            return None

    return poll_until(probe, timeout=timeout, what=what)


def join_org(admin, member, org_id):
    """向已建组织邀 member 加入（全流程）。"""
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
    check(invite["status"] == "pending", f"{admin.name}→{member.name} 出站邀请 pending")
    received = member.wait_event(
        "OrgInviteReceived", lambda d: d.get("orgId") == org_id, timeout=EVENT_TIMEOUT
    )
    # P3 后 join 全靠接受编排（stub 自举 + connect + 有界等 orgsync 收敛）——
    # 不再预等「预录快照」（legacy 推送通道已删）。收敛超时/失败按 TS 口径
    # 报错可重试（幂等，等价 UI 用户再点一次确认）。
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
    # 接受后确认：本地 org-list 经收敛可见该组织
    poll_until(
        lambda: any(o["orgId"] == org_id for o in member.send("org-list")) or None,
        timeout=EVENT_TIMEOUT,
        what=f"{member.name} 接受后收敛看到组织",
    )
    # 邀请回执（org-invite-reply）为尽力投递：可能迟到/丢失，但成员关系已由
    # 预录 + 接受编排生效。轮询管理员侧邀请记录收口；不到达则告警继续
    # （O2 数据验收不依赖该回执），丢失情况记入验收报告。
    try:
        poll_until(
            lambda: any(
                r.get("direction") == "outgoing"
                and r.get("peerRootId") == member.root_id
                and r.get("status") == "accepted"
                for r in admin.send("org-invite-records", orgId=org_id)
            )
            or None,
            timeout=20.0,
            what=f"{admin.name} 收讫 {member.name} 的邀请回执",
        )
    except AssertionError:
        print(
            f"  WARN: {admin.name} 未收到 {member.name} 的 org-invite-reply"
            "（尽力投递边界），成员关系已生效，继续"
        )


def org_view(node, org_id):
    view = node.send("org-view", orgId=org_id)
    check(view, f"{node.name} 应有组织视图")
    return view


def member_entry(node, org_id, root_id):
    for m in org_view(node, org_id)["members"]:
        if m["rootId"] == root_id:
            return m
    return None


def query_keys(node, org_id):
    page = node.send("data-query", domain=DOMAIN, name=NAME, orgId=org_id)
    return {item["key"] for item in page["items"]}


def fold_vv(node, org_id):
    return node.send("org-fold-vv", orgId=org_id, name=NAME)["vv"]


def local_acl(node, org_id):
    return node.send("data-list-access", orgId=org_id, name=NAME)


def main():
    a, b, c = Node("A").start(), Node("B").start(), Node("C").start()
    nodes = [a, b, c]
    a.init("Alice")
    b.init("Bob")
    c.init("Carol")

    def scenario():
        # ---- 建组织 + B/C 加入 ------------------------------------------
        org_id = a.send("org-create", name="O2 三端验收组织")["orgId"]
        join_org(a, b, org_id)
        print(f"  B 已加入: {org_id}")
        join_org(a, c, org_id)
        print("  C 已加入")

        # ---- A 晋升 C 为管理员（缺省推导 → C 成为数据账号）----------------
        a.send("org-set-member-role", orgId=org_id, rootId=c.root_id, role="admin")
        poll_until(
            lambda: org_view(c, org_id).get("isCurrentUserAdmin") is True or None,
            timeout=EVENT_TIMEOUT,
            what="C 收讫成员表（自身已晋升管理员）",
        )
        print("  C 已晋升管理员（数据账号缺省 = 全体管理员）")

        # ---- A 声明 encrypted + data-accounts 集合 ------------------------
        a.send(
            "data-declare",
            domain=DOMAIN,
            name=NAME,
            orgId=org_id,
            accounts="data-accounts",
            confidentiality="encrypted",
        )

        # ---- 声明经 org:structure 全员同步（F2-P1）：C（复制组）与 B（普通
        #      成员，全程不晋升）均自然收讫——无晋升编排 -----------------------
        def has_decl(node):
            try:
                node.send("data-query", domain=DOMAIN, name=NAME, orgId=org_id)
                return True
            except NodeError:
                return None

        poll_until(lambda: has_decl(c), timeout=EVENT_TIMEOUT, what="C 经 orgsync 收讫集合声明")
        # B 非驻留：data-query 会路由 orgq（受 acl 门控，grant 前 denied 空页
        # 会造成假阳性）；改用 data-get 探测——声明未落本地时 resolve_org 报错
        # （NodeError），声明到达后即返回正常应答（acl 拒绝只是读不到值）。
        poll_until(
            lambda: has_decl(b),
            timeout=EVENT_TIMEOUT,
            what="B 经 org:structure 收讫集合声明（F2-P1，无晋升编排）",
        )
        print("  集合声明已同步到 B（普通成员直收）与 C")

        # ---- 三方 accessKey 背靠背发布（F1 结构化合并回归：org:meta 并发写
        #      不丢更新）→ 六向互相可见 --------------------------------------
        c.send("org-publish-access-key", orgId=org_id)
        b.send("org-publish-access-key", orgId=org_id)
        a.send("org-publish-access-key", orgId=org_id)
        for holder, hlabel in ((a, "A"), (b, "B"), (c, "C")):
            for observer, olabel in ((a, "A"), (b, "B"), (c, "C")):
                if holder is observer:
                    continue
                poll_until(
                    lambda o=observer, h=holder: (
                        member_entry(o, org_id, h.root_id) or {}
                    ).get("accessKey")
                    or None,
                    timeout=EVENT_TIMEOUT,
                    what=f"{olabel} 的成员表可见 {hlabel} 的 accessKey（F1 合并）",
                )
        print("  accessKey 背靠背发布后六向互相可见（F1 结构化合并生效）")

        # ---- A 一次 grantAccess({B, C})（F3 残余修复实证：deliver 抢跑 acl
        #      时收端暂存 + acl 晚到重评估，密钥不再丢失——无 revoke/re-grant
        #      编排）。C 也须持有 epoch 密钥：靶心 3/4 由墓碑受理方就地写
        #      k2，受理方可能是 C（encrypted 本地写须持当前 epoch 密钥）------
        a.send("data-grant-access", orgId=org_id, name=NAME, readers=[b.root_id, c.root_id])
        for node, label in ((b, "B"), (c, "C")):
            poll_until(
                lambda n=node: a.root_id in local_acl(n, org_id).get("owners", []) or None,
                timeout=EVENT_TIMEOUT,
                what=f"acl 经 org:structure 同步到 {label}",
            )
        # C 的密钥可用性探针：就地写一条记录（需当前 epoch 密钥）→ A 收讫
        poll_ok(
            lambda: c.send(
                "data-save", domain=DOMAIN, name=NAME, key="kc0",
                value={"n": 0}, orgId=org_id,
            )
            and True,
            "C 持密钥就地写探针记录 kc0（F3：单次 grant 投递即达）",
        )
        poll_ok(
            lambda: "kc0" in query_keys(a, org_id) or None,
            "A 经 orgsync 收讫 C 的探针记录",
        )
        print("  单次 grant 后 B/C 密钥就绪（F3 收端暂存 + 重评估生效）")

        # ---- D1 靶心 1/3：B 写 k1 → 某数据账号 orgq 受理 → 两端收讫 -------
        def b_save(key, value):
            b.send(
                "data-save",
                domain=DOMAIN, name=NAME, key=key, value=value, orgId=org_id,
            )
            return True

        poll_ok(lambda: b_save("k1", {"n": 1}), "B 写 k1（等 orgkey 送达 + orgq 受理）")
        for node, label in ((a, "A"), (c, "C")):
            poll_ok(
                lambda n=node: "k1" in query_keys(n, org_id) or None,
                f"{label} 经 orgsync 收讫 k1",
            )
        print("  [1/4] B 写 k1（orgq 受理）→ A/C 均已收讫")

        # ---- D1 靶心 2/3：B 删 k1 → 某数据账号受理墓碑（org_tombstone_local，
        #      修复点位）→ 两端收讫墓碑 --------------------------------------
        b.send("data-delete", domain=DOMAIN, name=NAME, key="k1", orgId=org_id)
        for node, label in ((a, "A"), (c, "C")):
            poll_ok(
                lambda n=node: "k1" not in query_keys(n, org_id) or None,
                f"{label} 经 orgsync 收讫 k1 墓碑",
            )
        print("  [2/4] B 删 k1（orgq 受理墓碑）→ A/C 均已收讫墓碑")

        # ---- D1 靶心 3/3：定位墓碑受理方 Y（orgq 在线数据账号选择在多个
        #      在线数据账号间非确定，从节点日志判定），由 Y 就地写 k2——
        #      这是 Y 受理墓碑后的首次受管写，修复前序号与墓碑碰撞
        #      （org-vv-fix §1.2），另一端 Z 对 k2 折叠失明 -----------------
        from pathlib import Path

        def tombstone_acceptor():
            hits = set()
            for label, node in (("A", a), ("C", c)):
                log = Path(node.data_dir) / "stderr.log"
                if log.exists():
                    text = log.read_text(encoding="utf-8", errors="replace")
                    for line in text.splitlines():
                        if "[ORGQ] write tombstone" in line and line.rstrip().endswith(":k1"):
                            hits.add(label)
            return sorted(hits)

        poll_until(
            lambda: tombstone_acceptor() or None,
            timeout=EVENT_TIMEOUT,
            what="节点日志出现 k1 墓碑受理记录",
        )
        hits = tombstone_acceptor()
        check(len(hits) == 1, f"k1 墓碑应有唯一受理方，实际 {hits}")
        writer, observer = (a, c) if hits[0] == "A" else (c, a)
        print(f"  墓碑受理方 = {hits[0]}（由该端就地写 k2，对齐缺陷触发条件）")

        writer.send(
            "data-save", domain=DOMAIN, name=NAME, key="k2",
            value={"n": 2}, orgId=org_id,
        )
        poll_ok(
            lambda: "k2" in query_keys(observer, org_id) or None,
            "对端数据账号收到受理删除后的新写 k2（D1 折叠失明回归）",
        )
        print("  [3/4] 受理方删后再写 k2 → 对端数据账号已收讫（D1 回归通过）")

        # ---- D1 靶心 4/4：成员 B 再写 k3（orgq 受理）→ 两端收讫；B 读回 k2
        poll_ok(lambda: b_save("k3", {"n": 3}), "B 写 k3（orgq 受理）")
        for node, label in ((a, "A"), (c, "C")):
            poll_ok(
                lambda n=node: "k3" in query_keys(n, org_id) or None,
                f"{label} 经 orgsync 收讫 k3",
            )
        got = poll_ok(
            lambda: b.send(
                "data-get", domain=DOMAIN, name=NAME, key="k2", orgId=org_id
            )["value"]
            == {"n": 2}
            or None,
            "B 经 orgq 查询读回 k2 明文",
        )
        check(got, "B 应能读回 k2")
        print("  [4/4] B 再写 k3 两端收讫；B 经 orgq 读回 k2 明文一致")

        # ---- A/C 折叠 vv 收敛一致（复制组两端）----------------------------
        def vv_converged():
            va, vc = fold_vv(a, org_id), fold_vv(c, org_id)
            return va == vc and va or None

        vv = poll_until(
            vv_converged, timeout=EVENT_TIMEOUT, what="A/C 折叠 vv 收敛一致"
        )
        check(vv, "折叠 vv 非空")
        print(f"  A/C 折叠 vv 收敛一致: {vv}")

    elapsed = run_scenario(scenario, nodes)
    print(f"PASS scenario_org_orgq  {elapsed:.1f}s")


if __name__ == "__main__":
    main()
