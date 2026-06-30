# GameStore

> 一个面向高频游戏状态写入场景的、**持久化**、**高可用**、**兼容 Redis 协议** 的分布式键值存储。
>
> 设计目标对标 [TiKV](https://tikv.org/)、[Apache Kvrocks](https://kvrocks.apache.org/) 与字节跳动内部的 **Abase** 系统：
> 用磁盘（SSD）做主存储、用 Raft 做多副本一致性、用 Redis 协议做接入层，
> 在公有云上以低成本提供稳定、可靠、可水平扩展的在线 KV 服务。

---

## 这是什么

公司过去把 **Redis 当作持久化数据库** 来存玩家数据与财产信息。这条路线带来两个长期痛点：

1. **数据不稳定** —— Redis 以内存为主、持久化（RDB/AOF）为辅，宕机、OOM、主从切换都可能丢数据，
   对"玩家财产"这类强一致、不可丢失的数据是高风险的。
2. **成本居高不下** —— 全内存方案要求把全量数据常驻 RAM，随着玩家量与数据量增长，
   内存成本线性甚至超线性上升。

**GameStore** 用一句话概括其取舍：

> 把"主存储"从内存换成 SSD（RocksDB / LSM-Tree），把"可靠性"交给 Raft 多副本，
> 把"接入兼容性"交给 Redis 协议，从而在 **不改业务代码** 的前提下，
> 获得 **持久、稳定、低成本、可水平扩展** 的存储能力。

## 核心特征

| 维度 | 方案 |
| --- | --- |
| 接口协议 | 兼容 Redis RESP2/RESP3，支持 String / Hash / Set / ZSet / List 等常用类型 |
| 存储引擎 | RocksDB（LSM-Tree），SSD 为主存、内存做 Block Cache，写优化 |
| 一致性与高可用 | Multi-Raft，每个分片 3 副本跨可用区，自动选主与故障转移 |
| 水平扩展 | 哈希分片（兼容 Redis Cluster 槽位模型）+ Placement Driver 自动均衡 |
| 持久化 | 写入经 Raft 多数派落盘后才确认，配合对象存储做快照与 PITR |
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

## 关于本仓库的协作模式

这是一个 **由 AI 设计、人类干预、AI 实现** 的存储项目。为了让后续每一位（人类或 AI）参与者
都能快速获得完整的"项目记忆"，我们维护一份持续演进的上下文文档：

- **智能存储上下文 / 项目演进记录**：[`docs/EVOLUTION.md`](docs/EVOLUTION.md)

该文档从仓库的第一个 MR 开始，按时间顺序记录每一次变更的 **动机、关键决策、影响范围与后续方向**，
是阅读本项目、参与本项目之前 **强烈建议优先阅读** 的文件。

## 当前状态

项目处于 **Phase 0：设计阶段**。本批次提交为第一批设计文档，尚无可运行代码。
实现里程碑与阶段划分见 [`docs/design/09-roadmap.md`](docs/design/09-roadmap.md)。
