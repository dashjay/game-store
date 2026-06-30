# 09 · 演进路线图与里程碑

> 本文件给出从设计到完整分布式形态的 **分阶段落地计划**。原则：**渐进式交付，每阶段都可独立验证、可上线试用**。
> 不以日历时间排期（对自治的 AI 实现不适用），而以 **能力里程碑** 与 **依赖关系** 描述。

## 阶段总览

```
Phase 0  设计           ──▶  Phase 1  单机 MVP        ──▶  Phase 2  单分片高可用
(本批文档)                  (Redis-on-RocksDB)          (Raft 3 副本)
                                                            │
Phase 5  生产化   ◀──  Phase 4  云原生部署   ◀──  Phase 3  分布式
(类型/运维/性能)        (Operator/备份)            (Multi-Raft + PD + Proxy)
```

## Phase 0 · 设计（当前批次）
- **产出：** 本 `docs/` 设计文档体系 + [`EVOLUTION.md`](../EVOLUTION.md) 项目记忆机制。
- **退出标准：** 目标、架构、关键机制与取舍达成共识（含人类干预确认）。

## Phase 1 · 单机 MVP（Redis-on-RocksDB）
- **目标：** 验证"Redis 协议 + RocksDB 编码"的核心可行性。
- **范围：**
  - RESP2 服务端，支持 String 与 **Hash**（玩家数据主载体）核心命令。
  - 元数据键 + 子键 + 版本号编码、TTL、Compaction Filter GC（见 [`03-storage-engine.md`](03-storage-engine.md)）。
  - 基础指标与慢日志。
- **退出标准：** 现有 Redis 客户端可直连读写 Hash；重启不丢已落盘数据；通过兼容性用例。
- **依赖：** Phase 0。

## Phase 2 · 单分片高可用（Raft 3 副本）
- **目标：** 把单机引擎变成"不丢数据 + 可故障转移"的单分片。
- **范围：**
  - 接入 Raft，3 副本，多数派落盘才确认写（见 [`04-replication-consistency.md`](04-replication-consistency.md)）。
  - Leader Lease / ReadIndex 线性一致读；写 batch/group commit。
  - Raft 日志引擎与数据引擎分离；快照收发；成员变更（Joint Consensus）。
- **退出标准：** 杀掉 Leader 自动选主且不丢已确认写；副本可重建追平。
- **依赖：** Phase 1。

## Phase 3 · 分布式（Multi-Raft + PD + Proxy）
- **目标：** 水平扩展与透明路由。
- **范围：**
  - 16384 哈希槽分片，单节点多 Raft 组（见 [`05-sharding-routing.md`](05-sharding-routing.md)）。
  - PD：元数据、槽位映射（带 epoch）、心跳、Leader/副本均衡、故障转移、副本补齐。
  - Proxy：RESP 路由、连接复用、屏蔽 MOVED/ASK；可选智能 SDK 直连。
  - 扩缩容：副本搬迁与分片分裂，迁移期不阻塞前台。
- **退出标准：** 加/减节点对业务透明、负载自动均衡；单 AZ 故障整体仍可读写。
- **依赖：** Phase 2。

## Phase 4 · 云原生部署（Operator + 备份）
- **目标：** 在公有云一键部署、跨 AZ、可备份恢复。
- **范围：**
  - Kubernetes Operator + `GameStoreCluster` CRD（见 [`06-deployment-cloud.md`](06-deployment-cloud.md)）。
  - StatefulSet + 云盘 + 跨 AZ 拓扑约束；Proxy HPA。
  - 备份/恢复/PITR 到对象存储（见 [`07-backup-recovery.md`](07-backup-recovery.md)）；滚动升级。
- **退出标准：** 声明式拉起跨 3 AZ 集群；备份恢复演练达 RTO/RPO 目标。
- **依赖：** Phase 3。

## Phase 5 · 生产化（类型扩展 / 运维 / 性能）
- **目标：** 走向生产可用与规模化。
- **范围：**
  - 数据类型补全：Set / ZSet / List，及更多命令；按需 Pub/Sub、Lua、Stream。
  - Follower Read 扩展读吞吐；热点治理增强。
  - 完整可观测性与告警（见 [`08-observability-ops.md`](08-observability-ops.md)）；从 Redis 在线迁移工具链。
  - 性能压测与 RocksDB/Raft 调参，校准 SLO。
- **退出标准：** 承接真实业务流量，达成既定 SLO，完成 Redis 平滑下线。
- **依赖：** Phase 4。

## 后续（Backlog，待评估）
> 对应 [`EVOLUTION.md`](../EVOLUTION.md) 的"待决问题清单"。

- 跨分片分布式事务（2PC / Percolator）。
- 多地域 Active-Active 与冲突解决。
- 大 Value 的 KV 分离（BlobDB）。
- 冷热分层与更精细的成本优化。

## 实施约定

- 每完成一个阶段或一个重要里程碑，**在 [`EVOLUTION.md`](../EVOLUTION.md) 追加记录**（动机/决策/范围/后续）。
- 每个阶段交付都要 **可独立验证**（测试 + 关键指标），并保留灰度与回退能力。
- 重大取舍变更需 **人类干预确认**，并在演进记录中标注决策主体。
