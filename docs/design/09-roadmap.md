# 09 · 演进路线图与里程碑

> 本文件给出从设计到完整分布式形态的 **分阶段落地计划**。原则：**渐进式交付，每阶段都可独立验证、可上线试用**。
> 不以日历时间排期（对自治的 AI 实现不适用），而以 **能力里程碑** 与 **依赖关系** 描述。
>
> **路线说明：** 里程碑已对齐无主多写路线（MR-0007），不再有"Raft/选主"阶段；详见 [`../EVOLUTION.md`](../EVOLUTION.md)。

## 阶段总览

```
Phase 0  设计       ──▶ Phase 1 单机 MVP      ──▶ Phase 2 多副本无主多写
(本批文档)              (Redis-on-RocksDB)        (Quorum + WAL)
                                                      │
                                                      ▼
Phase 3  冲突解决与最终一致 (HLC + LWW + CRDT + Anti-Entropy)
                                                      │
Phase 6 生产化 ◀── Phase 5 云原生(Operator/备份) ◀── Phase 4 分布式(Partition + MetaServer + Proxy)
(类型/多租户/性能)
```

## Phase 0 · 设计（当前批次）
- **产出：** 本 `docs/` 设计文档体系 + [`EVOLUTION.md`](../EVOLUTION.md) 项目记忆机制。
- **退出标准：** 目标、架构、关键机制与取舍达成共识（含 MR-0007 的无主多写路线确认）。

## Phase 1 · 单机 MVP（Redis-on-RocksDB）
- **目标：** 验证"Redis 协议 + 通用引擎编码"的核心可行性。
- **范围：**
  - RESP2 服务端，支持 String 与 **Hash**（玩家数据主载体）核心命令。
  - 元数据键 + 子键 + 结构 version 编码、TTL、Compaction Filter GC（见 [`03-storage-engine.md`](03-storage-engine.md)）。
  - 基础指标与慢日志。
- **退出标准：** 现有 Redis 客户端可直连读写 Hash；重启不丢已落盘数据；通过兼容性用例。
- **依赖：** Phase 0。

## Phase 2 · 多副本无主多写（Quorum + WAL）
- **目标：** 把单机引擎变成"不丢数据 + 极高可用"的多副本 Partition。
- **范围：**
  - N 副本、**任一副本可写**；Replica Coordinator 并发 forward，达到 **Quorum W** 即确认（见 [`04-replication-consistency.md`](04-replication-consistency.md)）。
  - 每 Core 共享 WAL；可配置 `N/W/R`，支持 `W+R>N` 写后读一致。
  - 引入 HLC 时间戳作为版本号（为 Phase 3 冲突解决铺垫）。
- **退出标准：** 杀掉任一副本写入不中断、不丢已确认写；`W=1` 可极致可用、`W+R>N` 可写后读一致。
- **依赖：** Phase 1。

## Phase 3 · 冲突解决与最终一致（HLC + LWW + CRDT + Anti-Entropy）
- **目标：** 多写下保证最终一致且 **完全兼容 Redis 语义**。
- **范围：**
  - 数据暂存层（冲突合并）+ 通用引擎层（单版本）双层引擎；Operation 日志 + Checkpoint。
  - LWW（幂等）+ **Operation-based CRDT**（`INCR`/`APPEND` 与 String/Hash/ZSet/List）。
  - **Anti-Entropy**（ReplicaLog/Seqno 进度向量）一致性检测与修复。
- **退出标准：** 网络分区注入后恢复，数据秒级收敛且 `INCR` 等非幂等命令不丢更新、语义与 Redis 一致。
- **依赖：** Phase 2。

## Phase 4 · 分布式（Partition + MetaServer + Proxy）
- **目标：** 水平扩展与透明路由。
- **范围：**
  - Namespace/Table/Partition/Replica 模型，DataNode 多 Core/多 Replica（见 [`05-sharding-routing.md`](05-sharding-routing.md)）。
  - MetaServer：元信息、路由映射（带 epoch）、心跳、多租户负载均衡、故障检测、副本重建。
  - **Proxy 为默认接入**：RESP2/Thrift 路由、连接复用、Backup Request，标准 Redis 客户端零改造直连；
    重型 SDK 直连为 **可选项**，可推迟到 Phase 6。
  - 扩缩容：Replica 重建 + Anti-Entropy 追平，迁移期不阻塞前台。
- **退出标准：** 加/减节点对业务透明、负载自动均衡；单 AZ 故障整体仍可读写。
- **依赖：** Phase 3。

## Phase 5 · 云原生部署（Operator + 备份）
- **目标：** 在公有云一键部署、跨 AZ、可备份恢复。
- **范围：**
  - Kubernetes Operator + `GameStoreCluster` CRD（见 [`06-deployment-cloud.md`](06-deployment-cloud.md)）。
  - StatefulSet + 云盘 + 跨 AZ/POD 拓扑约束；Proxy HPA。
  - 备份/恢复/PITR 到对象存储（引擎 Checkpoint + Operation 日志，见 [`07-backup-recovery.md`](07-backup-recovery.md)）；滚动升级（无需转主）。
- **退出标准：** 声明式拉起跨 3 AZ 集群；备份恢复演练达 RTO/RPO 目标。
- **依赖：** Phase 4。

## Phase 6 · 生产化（类型扩展 / 多租户 / 性能）
- **目标：** 走向生产可用与规模化。
- **范围：**
  - 数据类型与命令补全；按需 Pub/Sub、Lua、Stream；单主半同步模式完善。
  - **多租户 QoS**（NRC/Quota/WFQ）与多维负载均衡；多地域就近访问与 Main Replicator。
  - 完整可观测性与告警（见 [`08-observability-ops.md`](08-observability-ops.md)）；DTS 从 Redis 在线迁移工具链。
  - 性能压测与引擎调参（Run-to-Complete、KV 分离等），校准 SLO。
- **退出标准：** 承接真实业务流量，达成既定 SLO，完成 Redis 平滑下线。
- **依赖：** Phase 5。

## 后续（Backlog，待评估）
> 对应 [`EVOLUTION.md`](../EVOLUTION.md) 的"待决问题清单"。

- 强一致字段的默认配置定档（单主半同步 vs `W+R>N`）。
- 跨分片事务（无主多写下语义更复杂）。
- 多地域 Active-Active 的更强冲突解决与同步优化。
- 大 Value 的 KV 分离、带 TTL 大 Value 只写 Log。
- 新硬件：RDMA / io_uring / ZNS SSD / PMEM。

## 实施约定

- 每完成一个阶段或一个重要里程碑，**在 [`EVOLUTION.md`](../EVOLUTION.md) 追加记录**（动机/决策/范围/后续）。
- 每个阶段交付都要 **可独立验证**（测试 + 关键指标），并保留灰度与回退能力。
- 重大取舍变更需 **人类干预确认**，并在演进记录中标注决策主体。
