# GameStore

> 一个面向高频游戏状态写入场景的、**持久化**、**高可用**、**兼容 Redis 协议** 的分布式键值存储。
>
> 设计主要对标字节跳动内部的 **Abase**（Abase2）系统，并参考 [Apache Kvrocks](https://kvrocks.apache.org/) 的 Redis-on-RocksDB 编码：
> 用磁盘（SSD）做主存储、用 **无主多写 + 可调 Quorum + CRDT** 做高可用复制、用 Redis 协议做接入层，
> 在公有云上以低成本提供稳定、可靠、可水平扩展的在线 KV 服务。
>
> **关于复制机制：** 与 Abase 一致，本系统 **不使用 Raft**。Abase 刻意避开共识/主从协议以追求"极高可用"
> （消除选主停顿、规避慢节点），默认 **最终一致**，并向用户开放 `Quorum(W/R/N)` 与"多主/单主半同步"模式选择。
> 该路线的来龙去脉（含从 Raft 转向无主多写的决策与公开依据）见 [`docs/EVOLUTION.md`](docs/EVOLUTION.md) 的 **MR-0007**。

---

## 这是什么

公司过去把 **Redis 当作持久化数据库** 来存玩家数据与财产信息。这条路线带来两个长期痛点：

1. **数据不稳定** —— Redis 以内存为主、持久化（RDB/AOF）为辅，宕机、OOM、主从切换都可能丢数据，
   对"玩家财产"这类强一致、不可丢失的数据是高风险的。
2. **成本居高不下** —— 全内存方案要求把全量数据常驻 RAM，随着玩家量与数据量增长，
   内存成本线性甚至超线性上升。

**GameStore** 用一句话概括其取舍：

> 把"主存储"从内存换成 SSD（RocksDB / LSM-Tree），把"高可用"交给 **无主多写 + 可调 Quorum + CRDT**，
> 把"接入兼容性"交给 Redis 协议，从而在 **不改业务代码** 的前提下，
> 获得 **持久、稳定、低成本、可水平扩展、极高可用** 的存储能力。

## 核心特征

| 维度 | 方案 |
| --- | --- |
| 接口协议 | 兼容 Redis RESP2/RESP3，**标准 Redis 客户端零改造直连 Proxy（默认，无需自研 SDK）**；重型 SDK 仅为可选优化 |
| 存储引擎 | 双层引擎：数据暂存层（多版本冲突合并）+ 可插拔通用引擎（RocksDB/LSH），SSD 主存、写优化 |
| 一致性与高可用 | **无主多写**，每个分片 N 副本跨可用区，任一副本可读写；可调 Quorum（典型 W=3,R=3,N=5），可选单主半同步 |
| 冲突解决 | HLC 全局时间戳版本化 + LWW（幂等）+ Operation-based CRDT（`INCR`/`APPEND` 及复杂结构）+ Anti-Entropy 修复 |
| 水平扩展 | 哈希分片到 Partition + MetaServer 路由与多租户均衡调度 |
| 持久化 | 写入达到 Quorum（W 个副本 WAL 落盘）后才确认，配合对象存储做快照与 PITR |
| 公有云部署 | Kubernetes Operator + 云盘 + 对象存储，开箱即用、跨 AZ 部署 |
| 适用负载 | 高频小值更新（玩家状态：每人 ~50 个字段，每字段 1~2 次/秒） |

## 目标场景

- 玩家在线状态、财产、背包、计数器等 **频繁更新的小数据**。
- 典型规模：单玩家约 50 个字段，每个字段每秒被修改 1~2 次，写入压力随在线人数线性增长。
- 要求：**不丢数据**（强持久化）、**单 Key 线性一致**、**故障自动恢复**、**水平扩展**。

> 详细的负载建模、容量与吞吐估算见 [`docs/design/01-workload-data-model.md`](docs/design/01-workload-data-model.md)。

## 文档导航

完整设计方案位于 [`docs/`](docs/) 目录：

- 设计总览：[`docs/design/00-overview.md`](docs/design/00-overview.md)
- 工作负载与数据模型：[`docs/design/01-workload-data-model.md`](docs/design/01-workload-data-model.md)
- 系统架构与组件：[`docs/design/02-architecture.md`](docs/design/02-architecture.md)
- 存储引擎与 Redis 编码：[`docs/design/03-storage-engine.md`](docs/design/03-storage-engine.md)
- 复制、一致性与高可用：[`docs/design/04-replication-consistency.md`](docs/design/04-replication-consistency.md)
- 分片、路由与扩缩容：[`docs/design/05-sharding-routing.md`](docs/design/05-sharding-routing.md)
- 公有云与 Kubernetes 部署：[`docs/design/06-deployment-cloud.md`](docs/design/06-deployment-cloud.md)
- 备份、恢复与数据迁移：[`docs/design/07-backup-recovery.md`](docs/design/07-backup-recovery.md)
- 可观测性与运维：[`docs/design/08-observability-ops.md`](docs/design/08-observability-ops.md)
- 演进路线图：[`docs/design/09-roadmap.md`](docs/design/09-roadmap.md)
- Rust 实现总体框架与 MR 分解计划：[`docs/design/10-implementation-plan-rust.md`](docs/design/10-implementation-plan-rust.md)

## 关于本仓库的协作模式

这是一个 **由 AI 设计、人类干预、AI 实现** 的存储项目。为了让后续每一位（人类或 AI）参与者
都能快速获得完整的"项目记忆"，我们维护一份持续演进的上下文文档：

- **智能存储上下文 / 项目演进记录**：[`docs/EVOLUTION.md`](docs/EVOLUTION.md)

该文档从仓库的第一个 MR 开始，按时间顺序记录每一次变更的 **动机、关键决策、影响范围与后续方向**，
是阅读本项目、参与本项目之前 **强烈建议优先阅读** 的文件。

## 当前状态

项目处于 **Phase 0 收尾 / Phase 1 启动**：设计文档已完成，并通过 [`spike/`](spike/) 的双语言（Rust + C++）
探针为语言选型提供了事实依据。**语言选型已拍板为 Rust**（决策见 [`docs/EVOLUTION.md`](docs/EVOLUTION.md) 的 **MR-0013**）。

进入实现阶段的 **总体框架与逐个 MR 分解计划** 见
[`docs/design/10-implementation-plan-rust.md`](docs/design/10-implementation-plan-rust.md)：
我们按"**一个 item 一个 MR**"顺序推进。已落地：`I-01`（Cargo workspace 与工程基线，MR-0014）、
`I-02`（`gamestore-protocol`：RESP2/RESP3 sans-IO 编解码 + tokio `Framed` 适配，DataNode 接入并支持 `HELLO` 握手，MR-0015）。
下一个为 `I-03`（`gamestore-engine`：通用引擎 + 编码 + Compaction GC）。
能力里程碑与阶段划分见 [`docs/design/09-roadmap.md`](docs/design/09-roadmap.md)。
