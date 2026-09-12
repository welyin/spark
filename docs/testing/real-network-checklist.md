# 真实网络验证清单（试点前）

> 实测记录留存于 wiki testing/real-network-verification.md（2026-08 真机实测档案）。

以下能力无法在 localhost 验证，试点前用 2~3 台真机 + 手机热点/VPS 做一次：

- UPnP 端口映射（真实家用路由器）；
- DCUtR 打洞（两个 NAT 后节点，手机热点 + 家庭宽带即可）；
- AutoNAT 公网可达性确认（一台 VPS 常驻节点）；
- IPv6 GUA 可达（手机热点通常直接给真前缀）。
