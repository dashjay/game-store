# 智能存储上下文 · 项目演进记录（EVOLUTION）

> **这是什么：** 一份从仓库 **第一个 MR** 开始、持续追加、不回溯删除的"项目记忆"文档。
> 它记录 GameStore 从无到有的 **整体演进方向**，以及每一次变更背后的 **动机与关键决策**。
>
> **为什么需要它：** 本项目采用 **AI 设计 → 人类干预 → AI 实现** 的协作模式。
> 在这种模式下，参与者（尤其是无状态的 AI Agent）每次介入时缺乏连续的上下文。
> 一份结构化、可被人类与机器同时读取的演进记录，能让任意一位参与者在动手前
> 快速理解"我们从哪里来、现在在哪、要往哪去、以及为什么"。

---

## 1. 使用约定

1. **只追加，不重写历史。** 已记录的条目不删除；如需修正，新增一条 `修订` 类型的记录指向它。
2. **一次逻辑变更（一个 MR / 一组相关提交）= 一条记录。** 编号单调递增：`MR-0001`、`MR-0002`……
3. **每条记录必须包含以下字段：**
   - **编号 / 日期 / 类型**：类型取值 `AI` / `Human` / `AI+Human`，标明决策与实现的主导方。
   - **动机（Why）**：为什么要做这次变更，解决什么问题。
   - **关键决策（Decisions）**：做了哪些重要选择，放弃了哪些备选项及原因。
   - **影响范围（Scope）**：新增/修改了哪些文档或模块。
   - **后续方向（Next）**：这次变更打开了哪些下一步，遗留了哪些待办。
   - **关联（Links）**：相关文档路径、commit、外部参考。
4. **决策应可追溯。** 重大架构取舍尽量在对应设计文档里展开，本文件只记摘要与指针。
5. **AI 友好。** 任何 Agent 在开始工作前应先读本文件与 [`README.md`](../README.md)，
   完成工作后 **必须** 在文末追加一条新记录。

---

## 2. 项目北极星（North Star）

> 这一节描述**长期不变**的目标，作为所有决策的锚点。变更需谨慎并记录原因。

- **不丢数据。** 写入须经 **Quorum 落盘**（达到用户配置的 W 个副本 WAL 持久化）后才返回成功；
  对玩家财产类数据可进一步配置 `W+R>N` 的写后读一致或单主半同步模式。
  （注：MR-0007 起，持久化保证从"Raft 多数派"调整为"Quorum + WAL"，理由见该记录。）
- **兼容 Redis。** 业务以最小改动（理想为零改动）从现有 Redis 迁移过来；
  对非幂等命令（`INCR`/`APPEND` 等）与复杂结构用 **CRDT** 保持语义完全兼容。
- **高可用（极高可用）。** 追求 Abase 式"极高可用"：**消除选主/主从切换造成的秒级不可用，并从架构上规避慢节点**。
  单节点/单可用区故障不影响整体可用性，故障自动恢复。
- **低成本。** 以 SSD 为主存替代全内存，存储成本随数据量增长而可控。
- **可调一致性。**（MR-0007 新增）默认 **最终一致**（无主多写），并向用户开放
  `Quorum(W/R/N)` 与"多主/单主半同步"模式选择，让业务在一致性、可用性、可靠性、性能之间自行取舍。
- **可水平扩展。** 吞吐与容量随节点数近似线性增长，扩缩容对业务透明。
- **云原生。** 在主流公有云上可一键部署、跨可用区、自动运维。

---

## 3. 演进记录（按时间顺序）

### MR-0001 · 初始化项目与 README
- **日期：** 2026-06-30
- **类型：** AI（设计与撰写）
- **动机：** 公司过去用 Redis 充当持久化数据库来存玩家数据与财产，存在 **数据不稳定**
  与 **成本居高不下** 两大痛点。需要一个全新的、定位清晰的持久化存储项目作为后续基础设施的底座。
- **关键决策：**
  - 确立产品定位：**持久化 + 高可用 + 兼容 Redis 协议** 的分布式 KV 存储，命名为 **GameStore**。
  - 选定对标系统：TiKV（Multi-Raft + RocksDB + PD）、Kvrocks（Redis-on-RocksDB）、Abase（高可用 Redis 兼容存储）。
  - 核心技术取舍：**SSD 主存（RocksDB/LSM）+ Raft 多副本 + Redis 协议接入**。
- **影响范围：** 新增/重写 [`README.md`](../README.md)。
- **后续方向：** 建立项目记忆机制；产出完整设计文档体系。
- **关联：** commit「初始化项目与 README」。

### MR-0002 · 引入智能存储上下文（项目演进记录机制）
- **日期：** 2026-06-30
- **类型：** AI（机制设计）
- **动机：** 项目采用"AI 设计 / 人类干预 / AI 实现"的协作模式，后续将有大量人类与 AI 参与者
  断续介入。需要一份从第一个 MR 开始、持续记录整体演进方向的上下文文档，避免决策上下文丢失、
  避免重复讨论已定论的问题。
- **关键决策：**
  - 以本文件 [`docs/EVOLUTION.md`](EVOLUTION.md) 作为唯一的"项目记忆"入口，**只追加不回溯**。
  - 定义统一的记录字段（动机/决策/范围/后续/关联）与编号规范（`MR-####`）。
  - 设立"项目北极星"小节，沉淀长期不变的目标作为决策锚点。
  - 约定任何 Agent 介入前先读、完成后追加记录的工作流。
- **影响范围：** 新增 [`docs/EVOLUTION.md`](EVOLUTION.md)；在 [`README.md`](../README.md) 中加入指引。
- **后续方向：** 在此机制下逐批补齐设计文档（总览、负载模型、架构、存储引擎、复制一致性、
  分片路由、部署、备份、可观测性、路线图）。后续每批文档落地时在本节追加对应记录。
- **关联：** 本文件。

### MR-0003 · 设计总览与工作负载/数据模型
- **日期：** 2026-06-30
- **类型：** AI
- **动机：** 在动手设计具体组件前，先统一"我们要解决的问题"与"数据长什么样、压力有多大"，
  作为后续所有设计文档的共同前提。
- **关键决策：**
  - [`design/00-overview.md`](design/00-overview.md)：明确目标/非目标/整体取舍，确立 CAP 立场（分片内 CP），沉淀术语表。
  - [`design/01-workload-data-model.md`](design/01-workload-data-model.md)：
    对"每玩家 ~50 字段、每字段 1~2 次/秒"做量化建模（百万在线 → 50~100M 写 QPS），
    得出 **水平分片是硬需求**；推荐用 **Hash（`player:{id}`）聚合玩家字段** 以获得同分片局部性、减少 Key 数、便于批量写合并。
- **影响范围：** 新增 `design/00-overview.md`、`design/01-workload-data-model.md`。
- **后续方向：** 据此设计系统架构与存储引擎。
- **关联：** 上述两份文档。

### MR-0004 · 系统架构、存储引擎与 Redis 数据编码
- **日期：** 2026-06-30
- **类型：** AI
- **动机：** 把总体取舍落到具体组件分层与磁盘上的数据布局。
- **关键决策：**
  - 架构（[`design/02-architecture.md`](design/02-architecture.md)）：四层 —— Client/SDK → 无状态 Proxy（RESP/路由/热点缓解）
    → Storage Node（Multi-Raft，多分片共享一个 RocksDB，按分片前缀隔离）→ PD（3 节点 Raft 的元数据与调度中心）。
    明确 **控制面（PD）与数据面分离**，PD 不在读写关键路径。
  - 存储引擎（[`design/03-storage-engine.md`](design/03-storage-engine.md)）：选 RocksDB/LSM（写优化、覆盖友好、低成本）；
    采用"**元数据键 + 子键 + 版本号**"编码 Redis 类型，version 递增实现 O(1) 逻辑删除 + Compaction Filter 后台 GC；
    **Raft 日志引擎与 KV 数据引擎分离**以稳定写延迟；给出 MemTable/组提交/Compaction/限速等写优化方向。
- **影响范围：** 新增 `design/02-architecture.md`、`design/03-storage-engine.md`。
- **后续方向：** 设计复制一致性（Multi-Raft）与分片路由（PD + Proxy）。
- **关联：** 上述两份文档。

### MR-0005 · 复制一致性（Multi-Raft）与分片路由（PD + Proxy）
- **日期：** 2026-06-30
- **类型：** AI
- **动机：** 落实"不丢数据 + 高可用 + 水平扩展"三大目标的核心机制。
- **关键决策：**
  - 复制与一致性（[`design/04-replication-consistency.md`](design/04-replication-consistency.md)）：
    每分片一个 Raft 组、默认 3 副本跨 AZ；多数派落盘才确认写入（不丢数据）；
    Leader Lease/ReadIndex 提供线性一致读，可选 Follower Read 扩展读；写入 batch/group commit 应对高频写；
    成员变更用 Joint Consensus；定义 Leader 故障转移与副本补齐流程；明确 CP 可用性边界；对比 Redis 主从。
  - 分片与路由（[`design/05-sharding-routing.md`](design/05-sharding-routing.md)）：
    兼容 Redis Cluster 的 **16384 哈希槽**，槽→分片→副本映射由 PD 维护并带 epoch；
    Proxy 路由（屏蔽 MOVED/ASK）或智能 SDK 直连；扩缩容用副本搬迁/分片分裂，迁移期 ASK 重定向、不阻塞前台；
    负载均衡、跨 AZ 反亲和与热点治理。
- **影响范围：** 新增 `design/04-replication-consistency.md`、`design/05-sharding-routing.md`。
- **后续方向：** 设计公有云部署、备份恢复、可观测性与演进路线图。
- **关联：** 上述两份文档。

### MR-0006 · 公有云部署、备份恢复、可观测性与演进路线图
- **日期：** 2026-06-30
- **类型：** AI
- **动机：** 让设计具备"能在公有云落地、能运维、能演进"的完整闭环，收束第一批设计文档。
- **关键决策：**
  - 部署（[`design/06-deployment-cloud.md`](design/06-deployment-cloud.md)）：Kubernetes Operator + `GameStoreCluster` CRD（含 YAML 草案）；
    Proxy 用 Deployment+HPA、Storage 用 StatefulSet+云盘、PD 3 副本；跨 AZ 拓扑约束；仅依赖块存储/对象存储/LB 以保多云可移植。
  - 备份恢复（[`design/07-backup-recovery.md`](design/07-backup-recovery.md)）：强调"多副本≠备份"；RocksDB Checkpoint 全量 + Raft 日志归档 → 对象存储；
    支持按分片并行恢复与 PITR；给出从现有 Redis 的离线/在线迁移与灰度切换方案。
  - 可观测性（[`design/08-observability-ops.md`](design/08-observability-ops.md)）：定义 Proxy/Store/PD 指标、慢日志/审计、追踪、告警规则与初始 SLO。
  - 路线图（[`design/09-roadmap.md`](design/09-roadmap.md)）：Phase 0 设计 → 1 单机 MVP → 2 单分片高可用 → 3 分布式(Multi-Raft+PD+Proxy)
    → 4 云原生(Operator+备份) → 5 生产化；以能力里程碑+依赖描述，不排日历时间。
- **影响范围：** 新增 `design/06-deployment-cloud.md`、`07-backup-recovery.md`、`08-observability-ops.md`、`09-roadmap.md`。
- **后续方向：** 第一批设计文档完成，进入 Phase 1 实现单机 Redis 兼容引擎 MVP。
- **关联：** 上述四份文档。

### MR-0007 · 架构路线修正：从 Multi-Raft 转向 Abase 式「无主多写」
- **日期：** 2026-06-30
- **类型：** AI+Human（**人类干预**：质疑 Raft 选型并明确"要做和 Abase 很接近的产品"；AI 查证并改写设计）
- **动机：** MR-0001~0006 的设计借鉴 TiKV，以 **Multi-Raft（CP）** 作为复制与一致性机制。
  人类参与者对"是否需要 Raft"提出质疑，并要求对齐 **字节跳动 Abase**（完全兼容 Redis 接口）。
  经查证，**Abase 并不使用 Raft**，反而是 **刻意避开** 共识/主从协议以追求"极高可用"。需据此修正路线。
- **关键证据（公开资料）：**
  - VLDB/SIGMOD 2025 论文《ABase: the Multi-Tenant NoSQL Serverless Database…》（arXiv:2505.07692）：
    "ABase supports the Redis protocol… and **enables eventual consistency**"。
  - 火山引擎《字节跳动极高可用 KV 存储系统详解》（作者：Abase2 负责人刘健）：明确对比 Raft/2PC/Quorum，
    选择 **无主 + Quorum** 以获得更高可用性；"Abase 2.0 是一套**多写架构**…没有了主从架构的切换主节点的时间…
    从架构层面屏蔽了慢节点"。
  - 《Abase2：字节跳动新一代高可用 NoSQL 数据库》（Abase NoSQL team）：
    Multi-Leader（默认，最终一致）+ 单主半同步（可选，强一致）；冲突解决用 **HLC + LWW + Operation-based CRDT**；
    **Anti-Entropy（ReplicaLog/Seqno）** 修复一致性；**双层引擎**（数据暂存层 + 可插拔通用引擎）。
- **关键决策：**
  1. **否决 Raft 作为主复制机制。** 改为 **无主多写（Leaderless Multi-Write）+ 可调 Quorum（W+R>N，典型 W=3,R=3,N=5）**，
     默认 **最终一致**；保留 **单主半同步（Leader&Followers，类 MySQL semi-sync）** 作为强一致可选模式。
     —— Raft 仍作为"已评估并否决的备选项"保留在文档中，其取舍理由（选主停顿、慢节点）正是否决依据。
  2. **冲突解决：** 写入用 **HLC 时间戳** 版本化；幂等命令 **LWW**；非幂等（`INCR`/`APPEND`）与复杂结构（String/Hash/ZSet/List）
     用 **Operation-based CRDT**，保证 **完全兼容 Redis 语义**。
  3. **一致性修复：** 用 **Anti-Entropy + ReplicaLog（内存进度向量）** 替代 Dynamo/Cassandra 的 Merkle-tree 全量 Diff。
  4. **引擎分层：** **数据暂存层（Conflict Resolver，多版本合并）+ 通用引擎层（可插拔 RocksDB/LSH，存单版本最终值）**；
     配合 Operation 日志的定期 **Checkpoint**。
  5. **组件对齐 Abase：** PD → **MetaServer**（元信息 + 多租户 QoS + 故障检测/修复）+ **RootServer**（多集群协调）；
     Storage Node → **DataNode**（Core/Run-to-Complete 协程、每盘一 DataNode、Core 内多 Replica 共享一个 WAL）；
     Raft Group → **Partition + N Replica + Replica Coordinator（任一副本可写）**；新增 **重型 SDK 直连** 与 **DTS 迁移**。
  6. **针对游戏"财产"数据的建议：** 高频计数/状态用多主 + CRDT 计数器（天然契合"每字段 1~2 次/秒"）；
     金币/财产等强一致字段按表配置 **单主半同步** 或 **`W+R>N` 写后读一致**。
- **影响范围：** 重写 [`design/04-replication-consistency.md`](design/04-replication-consistency.md)（核心机制）；
  更新 [`README.md`](../README.md)、[`design/00-overview.md`](design/00-overview.md)（CAP 立场/目标/对标）、
  [`design/02-architecture.md`](design/02-architecture.md)（组件）、[`design/03-storage-engine.md`](design/03-storage-engine.md)（双层引擎/CRDT）、
  [`design/05-sharding-routing.md`](design/05-sharding-routing.md)（Partition/Replica 模型与多地域）、
  以及 `06/07/08/09`（CRD/备份/指标/路线图）的术语与机制。
- **后续方向：** 据新路线推进实现；为强一致字段定档默认配置（见待决问题）。
- **关联：** 上述文档；arXiv:2505.07692；火山引擎/掘金 Abase2 系列文章。

### MR-0008 · 落实无主多写：重写架构与复制一致性文档
- **日期：** 2026-06-30
- **类型：** AI（按 MR-0007 决策实现文档改写）
- **动机：** 把 MR-0007 的路线修正落到核心设计文档。
- **关键决策：**
  - 重写 [`design/04-replication-consistency.md`](design/04-replication-consistency.md)：选型论证（为什么不用 Raft）、
    Partition+N Replica+Replica Coordinator 模型、Quorum 写读路径、可调一致性旋钮（含给"财产"字段的建议）、
    HLC+LWW、Operation-based CRDT、Anti-Entropy(ReplicaLog/Seqno)、故障恢复、与 Redis 主从/Raft 三方对比。
  - 重写 [`design/02-architecture.md`](design/02-architecture.md)：用户面/管控面/数据面三视角，五组核心模块
    （Client/Proxy/重型SDK、DataNode(Core/Run-to-Complete/共享WAL)、MetaServer、RootServer、DTS）；
    Namespace/Table/Partition/Replica 数据模型；POD 容灾与多地域 Main Replicator。
- **影响范围：** 重写 `design/02-architecture.md`、`design/04-replication-consistency.md`。
- **后续方向：** 同步更新存储引擎（双层引擎/CRDT）、分片路由、部署/备份/指标/路线图。
- **关联：** MR-0007；上述两份文档。

### MR-0009 · 同步存储引擎与分片路由到无主多写模型
- **日期：** 2026-06-30
- **类型：** AI
- **动机：** 让存储引擎与分片/路由文档与无主多写路线自洽。
- **关键决策：**
  - [`design/03-storage-engine.md`](design/03-storage-engine.md)：引入 Replica 三层结构与 **双层引擎**
    （数据暂存层 Conflict Resolver + 可插拔通用引擎 RocksDB/LSH）；区分 **HLC 时间戳**（跨副本冲突/排序）与
    **结构 version**（整 Key 逻辑删除/子键 GC）；以 **WAL（每 Core 共享）+ Operation 日志 + Checkpoint** 取代原 Raft 日志引擎；
    重写"一次写入在 Replica 内的路径"为 Coordinator→WAL→暂存层合并→通用引擎。
  - [`design/05-sharding-routing.md`](design/05-sharding-routing.md)：分片模型改为 Namespace/Table/Partition/Replica（无主）；
    路由以 MetaServer 元信息为准（Redis Cluster 槽位作为 Proxy 层可选兼容）；扩缩容用 Replica 重建 + Anti-Entropy 追平（无成员投票/转主）；
    多租户多维负载均衡与 NRC/Quota/WFQ；多地域 Main Replicator 就近访问。
- **影响范围：** 更新 `design/03-storage-engine.md`、`design/05-sharding-routing.md`。
- **后续方向：** 收尾更新部署 CRD、备份、可观测性指标、路线图。
- **关联：** MR-0007/0008；上述两份文档。

### MR-0010 · 部署/备份/可观测性/路线图对齐无主多写
- **日期：** 2026-06-30
- **类型：** AI
- **动机：** 收尾，让下游文档与无主多写路线完全自洽，消除 Raft/PD/选主等残留术语。
- **关键决策：**
  - [`design/06-deployment-cloud.md`](design/06-deployment-cloud.md)：CRD 改为 `dataNode/partition(replicaCount + consistency.mode/quorum)/meta/rootServer`；
    组件映射改为 DataNode/MetaServer/RootServer；高可用与滚动升级强调"无主、无需转主"。
  - [`design/07-backup-recovery.md`](design/07-backup-recovery.md)：快照基于引擎 Checkpoint(按 Partition)，
    增量改为 **Operation 日志/ReplicaLog 归档 + 按 HLC 重放** 的 PITR；恢复用 Anti-Entropy 补副本；DTS 承载迁移。
  - [`design/08-observability-ops.md`](design/08-observability-ops.md)：指标改为 Quorum 写延迟、quorum_not_met、
    anti_entropy_lag、replicalog_backlog、conflict_resolved、Backup Request、多租户 NRC/Quota/WFQ；SLO 增加"最终一致收敛秒级"。
  - [`design/09-roadmap.md`](design/09-roadmap.md)：里程碑改为 Phase 2 多副本无主多写(Quorum+WAL) → Phase 3 冲突解决与最终一致(HLC/LWW/CRDT/Anti-Entropy)
    → Phase 4 分布式(Partition+MetaServer+Proxy) → Phase 5 云原生 → Phase 6 生产化(多租户)。
- **影响范围：** 更新 `design/06`、`07`、`08`、`09`。至此第一批文档的无主多写改造全部完成。
- **后续方向：** 进入实现阶段（Phase 1）。
- **关联：** MR-0007/0008/0009。

### MR-0011 · 明确接入方式：默认 Proxy + 标准 Redis 客户端，自研 SDK 仅为可选
- **日期：** 2026-06-30
- **类型：** AI+Human（**人类干预**：确认"默认 proxy 即可"，无需复杂 SDK）
- **动机：** 人类参与者质疑"是否必须有复杂 SDK、能否直接用 Redis 协议"。查证后明确：
  Quorum 协调/冲突解决/副本同步都在服务端（Replica Coordinator），**普通 Redis 客户端即可使用**，
  自研 SDK 只是可选的低延迟优化，不应成为接入门槛。结论：**默认走 Proxy。**
- **关键决策：**
  - **Proxy + 标准 Redis 客户端 = 一等公民/默认接入**，零改造、无需自研 SDK。
  - **重型 SDK = 可选优化**（省一跳 + 精细 QoS），面向延迟极敏感的少数重度用户，可推迟到后期。
  - **"Client 核心库" 澄清为 Proxy/SDK 的内部实现，非业务依赖**（消除"必须集成复杂 SDK"的误解）。
  - 方式③（DataNode 直接 RESP + Cluster 重定向、去掉 Proxy）**仅列为未来可选，不作默认**
    （会失去就近路由/慢副本规避/连接收敛/多租户限流等 Proxy 能力）。
- **影响范围：** 重写 [`design/02-architecture.md`](design/02-architecture.md) §3.1 为"接入方式（三档）"+ 对比表；
  更新 [`README.md`](../README.md) 接口协议行、[`design/00-overview.md`](design/00-overview.md) 目标 2、
  [`design/09-roadmap.md`](design/09-roadmap.md) Phase 4 接入定位（SDK 可推迟到 Phase 6）。
- **后续方向：** 实现阶段优先做 Proxy + RESP，不被 SDK 拖累。
- **关联：** MR-0007（无主多写，Coordinator 在服务端是本结论的前提）。

### MR-0012 · 语言选型双语言 spike：Rust 与 C++ 各实现一遍 Phase-1 最小切片
- **日期：** 2026-06-30
- **类型：** AI+Human（**人类干预**：判断"这种存储更适合 C++/Rust 而非 Go"，并要求搭建双语言
  spike、对外提供 Redis 基础接口、用 Redis 测试做功能验证，两种实现一起提一个 PR）
- **动机：** 在正式进入实现前，需要为 Rust vs C++ 的语言选型提供 **可亲手运行、可逐文件对照** 的事实
  依据，而不是纯纸面论证。先排除 Go（GC 长尾延迟、与每 Core Run-to-Complete 模型不契合、调 RocksDB 需 cgo）。
- **关键决策：**
  - 在新增目录 [`spike/`](../spike/) 下用 **Rust 与 C++ 各实现一遍** Phase-1 MVP 的最小垂直切片：
    RESP2 服务端 + RocksDB 通用引擎层 + "元数据键 + 子键 + 版本号"编码（`03-storage-engine.md` §2）
    + **O(1) 逻辑删除 + Compaction Filter 后台回收旧 version/孤儿子键**（§4）。
  - 两实现的 **磁盘编码逐字节一致**；GC 用同一机制（内存 `key->当前version` 映射 + compaction filter），
    保证差异只来自语言/生态而非设计。
  - 支持命令：`PING/ECHO/SET/GET/DEL/EXISTS/TYPE/EXPIRE/PEXPIRE/TTL/PTTL/HSET/HMSET/HGET/HMGET/HGETALL/HDEL/HLEN/HEXISTS/FLUSHDB`，
    外加 spike 内省命令 `RAWCOUNT/DBSIZE/COMPACT`（用于验证 GC）。
  - **功能验证用标准 Redis 客户端**：`test/redis_functional_test.py`（redis-py）对两台服务跑 **同一套 32 项断言**，
    含 compaction-filter 把孤儿子键物理回收到 0、重建后只见新 version 数据；另有 `redis-cli` 冒烟。
    一键脚本 [`spike/test/run_all.sh`](../spike/test/run_all.sh) 编译两者并跑全部测试。
  - 选型差异的"体感点"沉淀在 [`spike/README.md`](../spike/README.md)：RocksDB 集成（C++ 原生 vs Rust `rust-rocksdb` FFI，
    两者都支持关键的 Compaction Filter）、Compaction Filter 写法、并发/内存安全（C++ 裸指针+mutex 靠人 vs Rust `Arc`+`Send/Sync` 编译期强制）、构建体验。
  - **环境坑记录：** 本镜像 `cc/c++` 默认指向 clang 且缺可用 libstdc++；Rust 用 `rust/.cargo/config.toml`
    钉死 `CC/CXX/linker=g++`，C++ 用 `-DCMAKE_CXX_COMPILER=g++`。
- **影响范围：** 新增 `spike/`（`rust/`、`cpp/`、`test/`、`README.md`、`.gitignore`）。不改动既有设计文档结论。
- **后续方向：** 由人类基于该 spike 拍板语言；选定后据此启动 Phase-1 正式实现（并补齐 String 之外的类型与 WAL/Quorum）。
  待决问题清单新增"语言选型"。
- **关联：** [`spike/README.md`](../spike/README.md)；`docs/design/03-storage-engine.md`、`09-roadmap.md`。

### MR-0013 · 语言选型拍板 Rust，并给出实现总体框架与 MR 分解计划
- **日期：** 2026-07-01
- **类型：** AI+Human（**人类干预**：基于 MR-0012 的双语言 spike 拍板"用 Rust 实现"，并要求先出整体框架 plan、再按 plan 逐个 MR 实现）
- **动机：** MR-0012 用 Rust 与 C++ 各实现一遍 Phase-1 最小切片作为选型依据。人类基于"亲手运行 + 逐文件对照"
  后决定 **采用 Rust**。进入正式实现前，需要一份把 [`09-roadmap.md`](design/09-roadmap.md) 的能力里程碑
  拆成 **可独立交付、可验证的 Rust 工程 MR** 的落地计划，并先把整体代码框架（workspace/crate/关键抽象/横切决策）定档，避免边做边返工。
- **关键决策：**
  - **语言：Rust。** 编译期内存/并发安全直接服务于"不丢数据 + 极高可用"的北极星（对照 spike：C++ 裸指针+mutex 靠人保证，Rust 靠 `Arc`+`Send/Sync` 编译期强制）；
    `rust-rocksdb`（TiKV 同款绑定）已在 spike 验证可支撑最关键的 Compaction Filter。C++ 作为"已评估的备选"保留（在"已有资深 C++ 存储团队/直接移植 Kvrocks 源码"场景更优），但不作默认路线。**排除 Go**（GC 长尾、与 Run-to-Complete 不契合、调 RocksDB 需 cgo）见 MR-0012。
  - **工程框架：** 单一 **Cargo workspace + 多 crate**，crate 边界严格对齐设计文档层次：
    `common / protocol / engine / datamodel / wal / replication / datanode / meta / proxy / cli`。定义核心可替换抽象
    （`GeneralEngine`、`CommandHandler`、`Wal`、`ReplicaTransport`、`ConflictResolver`、`RouteTable`）。
  - **横切决策定档：** 起步 `tokio`（把"Core/Run-to-Complete thread-per-core"作为 Phase-2+ 性能 MR，用 `glommio`/`monoio` 评估）；
    `thiserror`(库)/`anyhow`(bin)；`serde`+TOML 配置；`tracing`+`metrics` 可观测；磁盘编码沿用 spike 逐字节布局；
    CI 强制 `fmt+clippy(-D warnings)+test+deny`；测试分层（单元/属性 `proptest`/并发 `loom`/复用 spike 的 Redis 兼容性用例）。
  - **MR 分解：** 计划内以 `I-01…I-24` 标识实现 item，**一个 item 一个 MR**，按 Phase 1~6 分组并给出依赖图与统一"完成定义(DoD)"。
    首个落地为 **I-01（workspace 与工程基线）**（无前置依赖）。
  - **spike 处置：** `spike/` 保留为参考；I-02/03/04 将其 Rust 模块提升/加固/迁移到对应 crate（编码保持逐字节一致），重叠部分待稳定后另行归档。
- **影响范围：** 新增 [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md)（本次核心产出）；
  更新 [`README.md`](../README.md)（当前状态/文档导航）与本文件（新增本记录 + 勾选"语言选型"待决项）。**不改动既有设计结论。**
- **后续方向：** 按 [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) §3 从 **I-01** 开始逐个 MR 实现；每个 MR 合入后在此追加记录。
- **关联：** [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md)；MR-0012（spike）；[`spike/README.md`](../spike/README.md)；[`design/09-roadmap.md`](design/09-roadmap.md)。

### MR-0014 · I-01：Cargo workspace 与工程基线（首个实现 MR）
- **日期：** 2026-07-01
- **类型：** AI（按 [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) 的 I-01 定义实现）
- **动机：** MR-0013 拍板 Rust 并给出总体框架与 MR 分解计划。进入实现阶段的第一步，是把计划 §2 的
  工程骨架真正落地为"能编译、能测、CI 全绿"的 workspace 基线，为后续每个 item（I-02…）提供稳定地基，
  避免边做边搭脚手架而返工。范围严格限定在 **I-01**，不越界实现 I-02 及之后的协议/引擎/命令层。
- **关键决策：**
  - **Workspace 骨架：** 仓库根建 `Cargo.toml`（`[workspace]` + `[workspace.dependencies]` 公共依赖单一来源）、
    `rust-toolchain.toml`（固定 stable `1.83.0` + rustfmt/clippy）、`deny.toml`（cargo-deny v2 策略：advisories/licenses/bans/sources）。
  - **crate 边界对齐计划 §2.1：** 本次建 `gamestore-common / gamestore-protocol / gamestore-engine /
    gamestore-datamodel / gamestore-datanode` 五个 crate；`protocol/engine/datamodel` 为**空骨架**（含 doc 说明各自将在 I-02/03/04 填充），
    `wal / replication / meta / proxy / cli` 暂不建（待各自 MR），并在 `Cargo.toml` 注释登记，保持 workspace 始终可编译。
  - **基础设施门面（`gamestore-common`）：** 统一 `Error`（`thiserror`，`#[non_exhaustive]`）+ `Result`；
    `config`（`serde` + TOML，支持文件/`GAMESTORE_*` 环境变量覆盖）；`telemetry`（`tracing` + `tracing-subscriber`，`EnvFilter`）；
    `metrics`（`metrics` 门面 + Prometheus recorder，`/metrics` HTTP 端点留到 I-07）。
  - **最小 RESP 服务（`gamestore-datanode`）：** tokio 异步 accept 循环 + 每连接读写循环，对 `PING` 回 `PONG`（含带参回显）、`ECHO`、`QUIT`，
    其余命令显式报错（避免"静默忽略未实现命令"）。为便于集成测试拆成 lib + 薄 bin；RESP 解析器为 I-01 自带的极小实现，I-02 起改用 `gamestore-protocol`。
  - **横切依赖定档：** `tokio`（多线程）/`thiserror`/`anyhow`/`serde`+`toml`/`tracing`(+subscriber)/`metrics`(+prometheus-exporter, `default-features=false` 以免拉入 hyper)。
  - **锁定可复现构建：** 提交 `Cargo.lock`；为兼容固定的 1.83 工具链，将传递依赖 `indexmap` 钉到 `2.7.1`（连带 `hashbrown` 0.15，规避新版要求的 `edition2024`）。
  - **CI：** 新增 `.github/workflows/ci.yml`——`cargo fmt --check`、`cargo clippy -D warnings`、`cargo test`、`cargo build` 与独立的 `cargo deny check`（用 `cargo-deny-action@v2`）。
- **影响范围：** 新增仓库根 `Cargo.toml`/`Cargo.lock`/`rust-toolchain.toml`/`deny.toml`/`.gitignore`；
  新增 `crates/gamestore-{common,protocol,engine,datamodel,datanode}/`；新增 `config/datanode.example.toml`；新增 `.github/workflows/ci.yml`。
  **不改动既有设计文档与 `spike/`。**
- **退出标准（已达成）：** `cargo build`/`cargo test`/`cargo fmt --check`/`cargo clippy -D warnings`/`cargo deny check` 全绿；
  `cargo run -p gamestore-datanode -- --port 6390` 起服务后 `redis-cli -p 6390 ping` 返回 `PONG`（另有 `tests/ping_smoke.rs` 集成测试 + `dispatch`/`config` 单元测试覆盖）。
- **后续方向：** 按计划 §4 依赖图推进 **I-02（`gamestore-protocol`：RESP2/RESP3 编解码）** 与 **I-03（`gamestore-engine`：通用引擎 + 编码 + Compaction GC）**，
  逐步把 `spike/rust/` 的模块提升/迁移到对应 crate（磁盘编码保持逐字节一致）。
- **关联：** [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) I-01；MR-0013；`spike/rust/`。

### MR-0015 · I-02：`gamestore-protocol` RESP2/RESP3 编解码
- **日期：** 2026-07-01
- **类型：** AI（按 [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) 的 I-02 定义实现）
- **动机：** MR-0014（I-01）落地了 workspace 骨架，`gamestore-protocol` 当时仅为空壳，`gamestore-datanode`
  内嵌了一个 I-01 自带的极小 RESP 解析器。进入 I-02，需要把 spike 的 `resp.rs` 提升为 **健壮、可独立测试的 sans-IO
  RESP2/RESP3 编解码器**，作为接入层的稳定地基，并让 DataNode 改用它（对齐 I-01 里"I-02 起改用 gamestore-protocol"的约定）。
  范围严格限定在协议层与最小接入改造，不越界实现命令注册表/引擎（那是 I-03/I-04/I-05）。
- **关键决策：**
  - **sans-IO 核心 + tokio 适配分离：** 解析/序列化逻辑（[`decode`]/[`encode`]/[`frame`]）不依赖任何 I/O，
    可用纯单元 + 属性测试穷举；tokio 依赖只集中在 [`codec`] 一处（`tokio_util::codec::{Decoder,Encoder}`）。
    这样协议层既能被 DataNode 用 `Framed` 驱动，也能被未来的 Proxy/SDK/测试直接复用。
  - **增量（streaming）解码：** `decode`/`decode_command` 在数据不足时返回 `Ok(None)` 且 **不消费任何字节**，
    从而 **透明处理分片读**（一个 frame 跨多个 TCP 段）；完整时才 `split_to` 推进缓冲区。
  - **统一值模型 `Frame`（RESP2 ∪ RESP3）：** 覆盖 RESP2（simple/error/integer/bulk/array，含 null bulk/null array）
    与 RESP3（`_` null、boolean、double(含 inf/-inf/nan)、big number、bulk error、verbatim、map、set、push）。
    null 采用 **规范化 + 版本感知编码**：解码 `$-1`/`*-1`/`_` 统一为 `Frame::Null`，编码时按 `RespVersion`
    选择 RESP2 的 `$-1\r\n` 或 RESP3 的 `_\r\n`。
  - **请求解析（`decode_command`）：** 同时支持 **RESP 多 bulk 数组** 与 **inline 命令**；inline 分词对齐 Redis
    `sdssplitargs`（单/双引号、`\xHH` 与 `\n\r\t\b\a\"\'` 转义、引号不闭合报错）。
  - **边界与抗滥用：** 引入 `Limits`（`max_bulk_len` 512MiB、`max_array_len` 1M、`max_inline_len` 64KiB、
    `max_depth` 128，口径对齐 Redis），对超限长度/过深嵌套返回明确错误，避免恶意输入触发无界分配/栈溢出。
  - **错误分层：** 协议层自带可匹配的 `ProtocolError`（`Malformed`/`LimitExceeded`/`InlineSyntax`，`#[non_exhaustive]`）
    并提供 `From<ProtocolError> for gamestore_common::Error`；tokio 适配层额外用 `CodecError` 聚合 I/O 错误
    （满足 `Decoder::Error: From<io::Error>`）。
  - **DataNode 接入改造：** 删除 `gamestore-datanode/src/resp.rs`，连接循环改用 `Framed<TcpStream, CommandCodec>`；
    新增 **`HELLO [protover]` 握手**：按请求切换每连接协议版本（2/3），回复标准 server-info（RESP3 用 map、RESP2 用扁平数组），
    未知版本回 `NOPROTO`。命令面仍限握手/存活子集 `PING/ECHO/HELLO/QUIT`，未知命令显式报错。
  - **工具链兼容：** dev 依赖 `proptest` 钉到 `=1.5.0`（更高版本经 rand 0.9 拉入 `getrandom 0.4`，需未稳定的
    `edition2024`，与固定的 Rust 1.83 冲突）；连带把 `tempfile` 钉到 `3.14.0`（新版同样拉入 `getrandom 0.4.3`）。
- **影响范围：** 重写 `crates/gamestore-protocol/`（新增 `frame.rs`/`decode.rs`/`encode.rs`/`codec.rs`/`error.rs` +
  `tests/roundtrip.rs`(proptest) + `tests/framed.rs`）；改造 `crates/gamestore-datanode/`（删除 `resp.rs`、重写 `server.rs`、
  更新 `lib.rs`/`main.rs`/`tests/ping_smoke.rs`）；根 `Cargo.toml` 新增 `tokio-util`/`proptest`/`futures` 公共依赖；
  更新 `Cargo.lock`（钉 `proptest=1.5.0`、`tempfile=3.14.0`）；更新 [`README.md`](../README.md) 当前状态。**不改动既有设计文档结论与 `spike/`。**
- **退出标准（已达成）：** `cargo fmt --check`/`cargo clippy -D warnings`/`cargo test`/`cargo build` 全绿；
  协议层 42 单测 + 6 属性测试（RESP2/RESP3 round-trip、逐字节分片 round-trip、多 bulk 命令 round-trip、任意字节不 panic）
  + 2 个 `Framed` 集成测试通过；DataNode 起服务后用 **真实 `redis-py`** 分别以 RESP2/RESP3 完成 `PING`/`ECHO`/`HELLO`
  握手（RESP3 客户端连接时自动 `HELLO 3`，服务端正确协商为 proto 3 并回 map）。
- **后续方向：** 按依赖图推进 **I-03（`gamestore-engine`：通用引擎 + 编码 + Compaction GC）**；I-04 起在本协议层之上
  建 `CommandRegistry` 与 String/Hash 命令，复用 spike 的 `redis_functional_test.py` 兼容性用例。
- **关联：** [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) I-02；MR-0014（I-01）；`spike/rust/src/resp.rs`。

### MR-0016 · I-03：`gamestore-engine` 通用引擎 + 编码 + Compaction GC
- **日期：** 2026-07-02
- **类型：** AI（按 [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) 的 I-03 定义实现）
- **动机：** MR-0014（I-01）搭好 workspace 骨架、MR-0015（I-02）落地协议层后，`gamestore-engine`
  仍是空壳。进入 I-03，需要把 spike 的 `encoding.rs`/`gc.rs`/`storage.rs` **提升、加固并迁移** 为
  一个可独立压测/评审的 **通用引擎层**：抽象出 `GeneralEngine`、用 `rust-rocksdb` 落地、实现
  "元数据键 + 子键 + 结构 version" 编码与 **Compaction Filter 后台 GC**，并提供内省能力供后续一致性用例验证。
  范围严格限定在引擎层，不越界实现命令注册表/命令层（I-04）与服务装配（I-05）。
- **关键决策：**
  - **`GeneralEngine` 抽象（plan §2.3）：** 定义 backend 无关的最小 KV 面
    （`get` / `write(WriteBatch)` 组提交 / `scan_prefix` / `compact_range` / `install_gc`），
    令上层编码/version/GC 逻辑与具体引擎解耦，为后续 LSH 引擎（§7）预留可替换点。`WriteBatch`/`Range`/`GcPredicate`
    均为自有类型，调用方不直接依赖 RocksDB 类型。
  - **RocksDB 实现（`RocksEngine`）：** 用 `rocksdb` crate（bundled，TiKV 同款，spike 已验证）。
    因 RocksDB 要求 **Compaction Filter 在 open 时注册**，采用 **trampoline + 可替换槽**：open 时装一个固定 filter，
    它读取共享的 `RwLock<Option<Arc<dyn GcPredicate>>>`；`install_gc` 只是往槽里换 predicate，
    未安装时"全部保留"（退化为普通 KV）。`compact_range` 先 `flush` 再强制 bottommost，保证 filter 真正运行、垃圾被物理回收。
  - **编码逐字节沿用 spike：** `encoding.rs` 保持与 spike/C++ 一致的磁盘布局（`META_PREFIX`/`SUBKEY_PREFIX`、
    `[type][version BE][expire_ms BE][payload]`、`subkey=[prefix][u32 keylen][key][u64 version][field]`）。
  - **版本表 GC（`VersionMap`）：** 内存 `key -> 当前 version` 映射实现 `GcPredicate`；filter 判定
    "子键的 version == 属主当前 version 才保留"，属主被删/version 落后即回收；启动时扫描全部元数据 **重建** version 表。
  - **`Store<E: GeneralEngine>`（移植加固 storage.rs）：** 泛型于引擎（便于测试/压测/未来替换）；
    所有操作返回 `Result`（不再 `.expect()` panic）；类型不匹配返回 Redis 风格 `WRONGTYPE`（`EngineError::WrongType`）；
    实现 String（`SET/GET` + 惰性过期/TTL）与 Hash（`HSET/HGET/HGETALL/HDEL/HLEN/HEXISTS`）及 `DEL/EXISTS/TYPE/EXPIRE/PTTL`
    的 O(1) 逻辑删除；写路径用 `WriteBatch` 组提交。
  - **调优配置位（不调优）：** `EngineConfig` 暴露 Bloom bits / Block Cache 容量 / write buffer / 后台 I/O 限速
    （对齐 `03-storage-engine.md` §5），给出合理默认值，实际调参留待后续结合压测。
  - **内省能力：** `RAWCOUNT`（子键物理条数）/ `DBSIZE`（元数据条数）/ `COMPACT`（强制合并触发 GC），供一致性用例验证回收。
  - **错误分层：** 引擎自带 `EngineError`（`Backend`/`Corruption`/`WrongType`，`#[non_exhaustive]`）+
    `From<rocksdb::Error>` 与 `From<EngineError> for gamestore_common::Error`。
  - **工具链坑：** 在仓库根新增 `.cargo/config.toml`，把 `CC/CXX/linker` 钉到 gcc/g++
    （本镜像默认 clang 编译 bundled RocksDB 缺 libstdc++，与 spike 同因），供全 workspace 一致构建；
    dev 依赖 `tempfile` 钉到 `=3.14.0`（更高版本经 `getrandom 0.4` 拉入未稳定的 `edition2024`，与固定的 Rust 1.83 冲突，同 MR-0015 口径）。
- **影响范围：** 重写 `crates/gamestore-engine/`（新增 `engine.rs`/`rocks.rs`/`encoding.rs`/`gc.rs`/`store.rs`/`error.rs` +
  `tests/encoding_props.rs`(proptest) + `tests/store_rocksdb.rs`）；根 `Cargo.toml` 新增 `rocksdb` 公共依赖与 `tempfile` dev 依赖；
  新增仓库根 `.cargo/config.toml`；更新 `Cargo.lock`；更新 [`README.md`](../README.md) 当前状态。**不改动既有设计文档结论与 `spike/`。**
- **退出标准（已达成）：** `cargo fmt --check`/`cargo clippy -D warnings`/`cargo test`/`cargo build` 全绿；
  引擎层 11 单测 + 3 属性测试（编码 round-trip、subkey round-trip、任意字节 `parse_subkey` 不 panic）
  + 8 个 RocksDB 集成测试通过：**GC 单测**（100 子键经 `DEL`+`COMPACT` 回收到 0；新 version 重建后只见新数据、旧 version 不泄漏）、
  **重启后 version 表正确重建**（重开后 Hash 仍按当前 version 解析、GC 依旧生效）、String/Hash 语义与 `WRONGTYPE` 行为。
- **后续方向：** 按依赖图推进 **I-04（`gamestore-datamodel`：String + Hash + TTL）**——在本引擎之上建
  `CommandRegistry`、做参数/arity 校验与 Redis 一致的错误信息、复用 spike 的 `redis_functional_test.py` 兼容性用例。
- **关联：** [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) I-03；MR-0014（I-01）/MR-0015（I-02）；
  `spike/rust/src/{encoding,gc,storage}.rs`；`spike/rust/.cargo/config.toml`。

### MR-0017 · I-04：`gamestore-datamodel` String + Hash + TTL 命令层
- **日期：** 2026-07-02
- **类型：** AI（按 [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) 的 I-04 定义实现）
- **动机：** MR-0015（I-02）提供了协议层、MR-0016（I-03）提供了引擎层 `Store`，`gamestore-datamodel`
  仍是空壳。进入 I-04，需要在两者之间落地 **命令层**：把解析好的 Redis 请求（`Vec<Bytes>`）翻译为
  `Store` 操作、把引擎结果翻译为 RESP 回复 `Frame`，并以 Redis 一致的口径做 arity/参数校验与错误信息。
  范围严格限定在 I-04：不做 I-05 的服务装配/连接循环，不做 I-06 的 Set/ZSet/List。
- **关键决策：**
  - **`CommandRegistry`/`CommandHandler`/`ExecCtx`（plan §2.3）：** 注册表按 **大小写不敏感** 命令名查找
    handler，并以 **Redis 风格 arity 规格**（正数=精确 argc、负数=最小 argc，含命令名）在进入 handler
    之前统一校验，`ERR wrong number of arguments for 'xxx' command` 全部免费获得。`CommandHandler` 对任意
    `Fn(&mut ExecCtx, &[Bytes]) -> Frame` 有 blanket 实现，普通函数直接注册；handler 收到 **完整 argv**
    （`args[0]`=命令名，同 Redis），使 `HSET`/`HMSET` 共享实现仍按各自名字回复/报错。`ExecCtx` 携带
    `Store` 与连接协商的 `RespVersion`，为版本感知回复留口。
  - **命令面：** 连通性 `PING/ECHO`；String+TTL `SET(EX/PX)/GET/DEL/EXISTS/TYPE/EXPIRE/PEXPIRE/TTL/PTTL`；
    Hash `HSET/HMSET/HGET/HMGET/HGETALL/HDEL/HLEN/HEXISTS`；内省 `DBSIZE/RAWCOUNT/COMPACT`（一致性用例
    验证 GC 所需）。**`HELLO/QUIT/CLIENT` 等连接级命令与 `FLUSHDB` 等全库 admin 留在 I-05 的 DataNode 装配**——
    它们关乎连接/服务器而非数据模型。
  - **TTL 换算归命令层，惰性过期归引擎（03 §3）：** 命令层把相对 `EX` 秒/`PX` 毫秒/`EXPIRE` 秒换算成
    绝对 unix-epoch `expire_ms` 传给 `Store::set`/`expire_at`；`TTL` 把 `Store::pttl` 的剩余毫秒 **向上取整**
    为秒（同 Redis）。对齐 Redis 语义的边界：`SET` 的过期参数必须为正整数（否则 `ERR invalid expire time`）、
    `EX` 与 `PX` 互斥（`ERR syntax error`）、溢出检查；`EXPIRE` 非正数删除 key 且仍回 `1`
    （引擎 `expire_ms==0` 表示"无过期"，故已过期 deadline 收敛到 1ms-after-epoch 而非 0）。
  - **错误口径：** `EngineError::WrongType` 原样映射为裸 `WRONGTYPE ...`，其余引擎错误包 `ERR ...`；
    整数解析失败统一 `ERR value is not an integer or out of range`；未知命令按原始拼写回报。
  - **RESP3 版本感知回复：** `HGETALL` 在 RESP3 连接回 **原生 map**、RESP2 回扁平 field/value 数组
    （与协议层的版本感知编码配合，其余命令回复两版本同形）。
  - **测试：** 42 项——registry 契约（大小写、精确/最小 arity、未知/空命令）7 项；String+TTL 正常/边界/
    错误路径 20 项；Hash 12 项（含奇数对 arity、WRONGTYPE 全覆盖、二进制安全 field/value、RESP2/RESP3
    `HGETALL` 双形态）；以及 **`spike/test/redis_functional_test.py` 全部 32 项断言的 Rust 移植**
    （`functional_parity.rs`，按原顺序对 RocksDB 后端 Store 断言，Python 脚本的 `flushdb` 以"新开 store"
    等价替代——`FLUSHDB` 是 I-05 的 admin 命令）。真正经 TCP 的端到端复跑随 I-05 装配进行。
- **影响范围：** 填充 `crates/gamestore-datamodel/`（`registry.rs` + `commands/{connectivity,string,hash,admin}.rs` +
  `tests/{registry,string_commands,hash_commands,functional_parity}.rs`）；`Cargo.toml`/`Cargo.lock` 仅登记
  既有 workspace 依赖（engine/protocol/bytes/tempfile），**未引入新外部依赖**；更新 [`README.md`](../README.md)
  当前状态。**不改动既有设计文档结论与 `spike/`。**
- **退出标准（已达成）：** `cargo fmt --check`/`cargo clippy --workspace --all-targets -D warnings`/
  `cargo test --workspace`/`cargo build --workspace` 全绿；spike 32 项断言的等价 Rust 移植全部通过；
  每个命令有正常/边界/错误路径（arity、WRONGTYPE、缺失 key、TTL 边界）单测覆盖。
- **后续方向：** 按依赖图推进 **I-05（`gamestore-datanode`：单机服务装配）**——把 `CommandRegistry` 接入
  DataNode 连接循环（`HELLO` 版本协商已有，`ExecCtx.version` 就绪）、加 `--config` 加载与 `FLUSHDB` 等
  admin、优雅关闭，然后用 **真实 redis-py 跑 `spike/test/redis_functional_test.py`** 达成 Phase 1 退出标准
  （标准 Redis 客户端零改造直连读写 Hash、重启不丢已落盘数据）。
- **关联：** [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) I-04；
  MR-0015（I-02 协议层）/MR-0016（I-03 引擎层）；`spike/rust/src/commands.rs`；`spike/test/redis_functional_test.py`。

### MR-0018 · I-05：`gamestore-datanode` 单机服务装配（Phase 1 退出标准达成）
- **日期：** 2026-07-02
- **类型：** AI（按 [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) 的 I-05 定义实现）
- **动机：** MR-0015（I-02 协议层）、MR-0016（I-03 引擎层）、MR-0017（I-04 命令层）已各自就绪，
  但 DataNode 的连接循环仍只回 `PING/ECHO/HELLO/QUIT`。进入 I-05，把三层真正装配成 **单机可服务的
  DataNode**：`--config` 加载（数据目录/监听地址）、启动时 `Store::open` 并以 `Arc<Store<RocksEngine>>`
  共享给所有连接、命令经 `CommandRegistry` 分发、补 `FLUSHDB`/`FLUSHALL` admin 与优雅关闭，
  用 **真实 redis-py** 达成 [`design/09-roadmap.md`](design/09-roadmap.md) 的 Phase 1 退出标准。
  范围严格限定在 I-05：不做 I-06 的 Set/ZSet/List、不做 I-07 的指标端点、不做 I-08 的 WAL。
- **关键决策：**
  - **分层口径（沿用 I-04 约定）：** 连接级命令（`HELLO` 版本协商、`QUIT`）与标准客户端连接时发出的
    housekeeping（`CLIENT SETINFO/SETNAME/ID/GETNAME`、`SELECT`（仅 0 号库）、`COMMAND`（回空数组））
    留在 DataNode 层；`FLUSHDB`/`FLUSHALL` 全库 admin 也在此层（它们关乎服务器而非数据模型）；
    **其余一律走 `CommandRegistry::standard()`**，未知命令由注册表统一回 `ERR unknown command '...'`
    （I-02 时代 DataNode 自带的口径随之删除）。
  - **`FLUSHDB`/`FLUSHALL` 落在引擎层为 `Store::flush_all`：** 单库语义下两者等价；
    一个原子 `WriteBatch` 同时删光元数据与子键记录，**随后清空 version 表**（内存 `key -> version` 映射），
    保证崩溃不会留下"元数据指着已删子键"的中间态；接受并忽略 `ASYNC`/`SYNC` 修饰符（同步执行）。
  - **阻塞引擎调用直接同步执行（不走 `spawn_blocking`）：** Phase-1 的引擎操作是 RocksDB 点读/点写/短前缀扫描
    （memtable/block cache 内微秒级、前台无 fsync），`spawn_blocking` 会给**每条**命令加一次任务切换开销、
    并把工作汇聚到无界的 blocking 池；多线程 runtime 下其它连接在各自 worker 上继续推进。唯一真正长耗时的
    `COMPACT` 是测试/admin 内省动词、不在热路径。此决策待 I-07 基准出数后复核，I-08 引入 WAL 时反正要重审写路径。
  - **优雅关闭：** shutdown future 触发后停止 accept，经 `tokio::sync::watch` 通知每个连接
    "完成在途命令后关闭"，`serve` 用 `JoinSet` 等全部连接 drain 完才返回（集成测试覆盖：
    idle 连接在关闭后收到 EOF、`serve` 正常返回）。
  - **`Core` 仅作逻辑预留（plan §1）：** 今天一个 DataNode = 一个逻辑 Core = 一个共享 `Arc<Store>`；
    注释明确 I-08/Phase-2 时该 `Arc<Store>` 升级为 `Core` 单元（store + WAL + replica 集合）、
    `serve` 扩展为按 Partition 路由的 `Vec<Core>`，不实装 thread-per-core。
  - **配置扩展：** `[storage] data_dir`（默认 `./data`，启动时自动创建），支持
    `GAMESTORE_STORAGE__DATA_DIR` env 与 `--data-dir` CLI 覆盖，`config/datanode.example.toml` 同步更新。
  - **连接 id：** 进程内单调递增的原子计数器，供 `HELLO` 回复的 `id` 字段与 `CLIENT ID` 使用（此前恒为 0）。
- **影响范围：** 重写 `crates/gamestore-datanode/src/server.rs`（注册表接入 + admin/housekeeping + 优雅 drain）、
  `main.rs`（开 store、`--data-dir`）、更新 `lib.rs`/`Cargo.toml`/`tests/ping_smoke.rs`、新增 `tests/e2e_data.rs`；
  `crates/gamestore-engine` 新增 `Store::flush_all` + 2 个集成测试；`gamestore-common` 配置新增 `StorageConfig`；
  更新 `config/datanode.example.toml` 与 [`README.md`](../README.md) 当前状态。**未引入新外部依赖**；
  **不改动既有设计文档结论与 `spike/`。**
- **退出标准（已达成，Phase 1 DoD）：** `cargo run -p gamestore-datanode -- --config ...` 起服务后，
  **真实 redis-py** 跑 `spike/test/redis_functional_test.py` **RESP2 与 RESP3（protocol=3）各 32/32 断言全部通过**
  （首次经 TCP 端到端，覆盖 String/Hash/TTL/GC/FLUSHDB）；**重启不丢数据** 两重验证——手动起停进程
  （写入 → Ctrl-C → 重启 → 数据可读）与自动化集成测试 `restart_preserves_persisted_data`；
  `cargo fmt --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` /
  `cargo build --workspace` 全绿。
- **后续方向：** Phase 1 仅剩 **I-06（Set/ZSet/List 复合类型，依赖 I-04）** 与
  **I-07（可观测性与基准：/metrics 端点、慢日志、criterion 基准，依赖 I-05）**，两者可并行；
  之后进入 Phase 2 的 **I-08（WAL）**。
- **关联：** [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) I-05；
  [`design/09-roadmap.md`](design/09-roadmap.md) Phase 1；MR-0015/0016/0017；`spike/test/redis_functional_test.py`。

### MR-0019 · I-06：Set / ZSet / List 复合类型补全
- **日期：** 2026-07-02
- **类型：** AI（按 [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) 的 I-06 定义实现）
- **动机：** MR-0018（I-05）达成 Phase 1 退出标准后，数据模型仍只有 String/Hash。进入 I-06，按
  [`design/03-storage-engine.md`](design/03-storage-engine.md) §2.3 的编码把 Set/ZSet/List 三类结构
  落到引擎层与命令层，并让 `TYPE`/`DEL`/`EXPIRE` 等通用命令对新类型生效。范围严格限定在 I-06：
  不做 I-07 的指标端点、不做 I-08 的 WAL。
- **关键决策：**
  - **编码只增不改（既有布局逐字节不动）：** 新类型在既有 `[type][version BE][expire_ms BE][payload]`
    元数据布局上新增 type tag（`TYPE_SET=3`/`TYPE_ZSET=4`/`TYPE_LIST=5`）；Set 成员复用 Hash 的
    subkey 布局、值为空（成员性由键表达，03 §2.3）；String/Hash 的磁盘布局与 spike 保持逐字节一致。
  - **ZSet 双重编码（03 §2.3）：** 成员 subkey 存 score（8 字节保序编码），另在 **新前缀
    `ZSCORE_PREFIX=0x03`** 下建 score 索引记录 `[prefix][keylen][key][version][保序score][member]`，
    前缀扫描天然得到 `(score, member)` 升序——`ZRANGE`/`ZRANGEBYSCORE` 即一次有序扫描（同分即按
    member 字典序，与 Redis 一致）。score 保序编码用标准 float 技巧（负数全位取反、非负翻符号位），
    `ZADD` 更新分数时同批删旧索引、写新索引，不留脏记录。
  - **List 编码（03 §2.3）：** 元素 subkey 的 field 为 8 字节大端 index（天然按数值排序），元数据
    payload 存 `[head, tail)` 边界，初始 index 取 `1<<63` 让两端 push 都近乎无界；两端 push/pop O(1)，
    index 始终稠密（只动两端）。
  - **GC 统一覆盖两族版本化记录：** `encoding::parse_owner_version` 同时解析 subkey 与 score 索引
    （同构键形），`VersionMap` 的 Compaction Filter 判定与启动重建对四种复合类型一视同仁；
    `RAWCOUNT`/`FLUSHDB` 也覆盖 `ZSCORE_PREFIX`。`DEL`/过期对新类型仍是 O(1) version 递增。
  - **命令面（Redis 语义对齐）：** `SADD/SREM/SISMEMBER/SMEMBERS/SCARD`；
    `ZADD/ZSCORE/ZRANGE(WITHSCORES)/ZRANGEBYSCORE(WITHSCORES/LIMIT/±inf/`(`排他界)/ZREM/ZCARD`；
    `LPUSH/RPUSH/LPOP/RPOP([count]，Redis 6.2 语义)/LRANGE/LLEN`。空集合/空列表随最后一个成员删除；
    `ZADD` 的 `NX/XX/GT/LT/CH/INCR` 标志与 `ZRANGE` 的 `REV/BYSCORE/BYLEX` 扩展按 I-04 `SET NX/XX`
    先例显式回 `ERR syntax error`（不静默误解析）。score 解析拒绝 NaN、接受 `±inf`/科学计数法，
    回复格式对齐 Redis（整数值不带小数点）。
  - **RESP3 版本感知回复：** `SMEMBERS` 回原生 set、`ZSCORE` 回原生 double、`ZRANGE ... WITHSCORES`
    回嵌套 `[member, score]` 对（score 为 double），RESP2 保持扁平数组/bulk——与 Redis 7 行为一致，
    经真实 redis-py `protocol=3` 验证。
  - **测试分层：** 引擎层 12 个新集成测试（三类型语义、WRONGTYPE 交叉矩阵、GC 回收到 0、TTL、
    重启后 version 表重建与边界恢复、ZADD 更新不留脏索引）；命令层 20 个新测试文件级用例
    （正常/边界/错误路径全覆盖）；DataNode 线级测试扩展（新类型 TCP 往返、RESP3 形态、重启持久化）；
    新增 **`tests/redis_composite_test.py`（46 项断言，真实 redis-py，RESP2/RESP3 双跑）**，
    spike 的 32 项基线套件保持冻结并在 RESP2/RESP3 下无回归（32/32）。
- **影响范围：** `crates/gamestore-engine`（`encoding.rs` 扩展 + `gc.rs` 谓词泛化 + `store.rs` 新增
  三类型操作与 `load_typed`/`new_composite_meta` 收敛重复逻辑）；`crates/gamestore-datamodel`
  （新增 `commands/{set,zset,list}.rs` 与对应测试文件）；`crates/gamestore-datanode/tests/e2e_data.rs`
  扩展；新增仓库根 `tests/redis_composite_test.py`（plan §2.1 的跨 crate 一致性用例位）。
  **未引入新外部依赖**；**不改动既有设计文档结论与 `spike/`**。
- **退出标准（已达成）：** `cargo fmt --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace` / `cargo build --workspace` 全绿；真实 redis-py 经 TCP：spike 32 项断言
  RESP2/RESP3 双双无回归、新类型 46 项断言 RESP2/RESP3 全过；手动起停进程验证新类型数据重启不丢；
  `DEL` 大 Set/ZSet/List 后 `COMPACT` 将物理记录（含 score 索引）回收到 0。
- **后续方向：** Phase 1 仅剩 **I-07（可观测性与基准，依赖 I-05）**；之后进入 Phase 2 的
  **I-08（`gamestore-wal`）**。
- **关联：** [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md) I-06；
  [`design/03-storage-engine.md`](design/03-storage-engine.md) §2.3；MR-0016（I-03 引擎层）/
  MR-0017（I-04 命令层）；`tests/redis_composite_test.py`。

<!-- 后续记录在此向下追加。请勿在已有记录上方插入。 -->

---

## 4. 待决问题清单（Open Questions）

> 这里登记尚未拍板、需要人类干预或后续讨论的问题。解决后在对应 MR 记录中标注。

- [x] **是否使用 Raft：已否决（MR-0007）。** 经查证 Abase 并不使用 Raft，而采用无主多写 + Quorum + CRDT。
  本项目对齐 Abase，默认无主多写、可选单主半同步，不以 Raft 作为主复制机制。
- [ ] 默认最终一致已确定；**玩家财产/金币等强一致字段**的推荐配置（单主半同步 vs `W+R>N`）需结合业务定档。
- [ ] **跨分片事务**是否需要、以何种方式实现，待定（无主多写下事务语义更复杂）。
- [ ] 是否需要 **多地域 Active-Active**；若需要，一致性模型与冲突解决策略待定。
- [ ] 大 Value（如玩家完整快照 JSON）是否启用 **BlobDB / KV 分离**，需结合真实 Value 分布评估。
- [ ] Proxy 是否对所有客户端强制，还是为支持 Redis Cluster 协议的智能客户端提供直连路径。
- [ ] 冷热分层 / TTL 驱逐策略的具体阈值，需结合线上数据画像确定。
- [x] **实现语言选型（Rust vs C++）：已拍板 Rust（MR-0013）。** 已排除 Go；MR-0012 的双语言 spike（[`spike/`](../spike/)）作为对照依据，
  人类基于"亲手运行 + 逐文件对照"后决定采用 **Rust**（编译期内存/并发安全契合"不丢数据"北极星）；
  C++ 作为已评估备选保留（"已有资深 C++ 存储团队 / 直接移植 Kvrocks 源码"场景更优）。
  实现框架与 MR 分解见 [`design/10-implementation-plan-rust.md`](design/10-implementation-plan-rust.md)。
