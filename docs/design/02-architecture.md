# 02 · 系统架构与组件

> 本文件描述 GameStore 的分层架构、各组件职责、以及一次读写在系统中的完整流转。
> 架构对齐字节跳动 Abase2（用户面 / 管控面 / 数据面 三视角，五组核心模块）。
>
> **路线说明：** 本文件在 MR-0007 由"Proxy + Storage Node(Raft) + PD"重写为 Abase 式组件，详见 [`../EVOLUTION.md`](../EVOLUTION.md)。

## 1. 架构总览

```
   用户面                ┌──────────────────────────────────────────────┐
                         │            业务客户端 / Client                 │
                         │  现有 Redis 客户端 / 重型 SDK（直连，跳过 Proxy）│
                         └───────────────┬──────────────────────────────┘
                          RESP2/Thrift   │           ┌──────────────┐
                                         ▼           │   重型 SDK    │ 直连
                         ┌───────────────────────┐   └──────┬───────┘
                         │   Proxy（无状态接入层）  │          │
                         │ RESP/Thrift·路由·QoS    │          │
                         └───────────────┬─────────┘          │
   数据面                                │  内部 RPC           │
            ┌───────────────────────────┼─────────────────────┘
            ▼                            ▼                       ▼
   ┌──────────────────┐       ┌──────────────────┐     ┌──────────────────┐
   │   DataNode (盘1)  │       │   DataNode (盘2)  │     │   DataNode (盘3)  │
   │  ┌────Core────┐  │       │  ┌────Core────┐  │     │  ┌────Core────┐  │
   │  │ Replica ×k │  │       │  │ Replica ×k │  │     │  │ Replica ×k │  │
   │  │ 共享 1×WAL  │  │       │  │ 共享 1×WAL  │  │     │  │ 共享 1×WAL  │  │
   │  └─────┬──────┘  │       │  └─────┬──────┘  │     │  └─────┬──────┘  │
   │  双层引擎(暂存+   │       │  双层引擎(暂存+   │     │  双层引擎(暂存+   │
   │  通用引擎/盘)     │       │  通用引擎/盘)     │     │  通用引擎/盘)     │
   └──────────────────┘       └──────────────────┘     └──────────────────┘
            ▲ 心跳/修复                ▲                          ▲
            └──────────────┬──────────┴──────────────────────────┘
   管控面                  ▼
            ┌──────────────────────────┐      ┌───────────────────────────┐
            │       MetaServer          │      │        RootServer          │
            │ 元信息·多租户QoS总控·      │◀────▶│ 全集群视角·跨集群资源/迁移/ │
            │ 故障检测·数据修复调度       │      │ 容灾·控制爆炸半径           │
            └──────────────────────────┘      └───────────────────────────┘

      旁路： DTS（Data Transfer Service）—— 迁移 / 备份回滚 / Dump / 订阅
```

## 2. 逻辑数据模型

`Namespace（库）→ Table（逻辑表）→ Partition（分片）→ Replica（副本）`：

- **Namespace：** 一个用户/租户的库。
- **Table：** Namespace 下的逻辑表。
- **Partition：** Table 被切成的多个不重叠分片，是路由与复制的单位。
- **Replica：** Partition 的一份副本（默认 N=3/5，跨 AZ/POD）；**无主，任一副本可读写**（见 [`04-replication-consistency.md`](04-replication-consistency.md)）。

## 3. 组件职责（五组核心模块）

### 3.1 Client / Proxy / 重型 SDK（用户面）
- **Client：** 用户侧核心库，向上提供各数据结构接口，向下经 **MetaSync** 从 MetaServer 拉取路由信息、
  直接与 DataNode 交互。集成 **重试、Backup Request、热 Key 承载、流控、鉴权** 等 QoS 能力。
- **Proxy：** 基于 Client 封装的 **无状态接入层**，对外提供 **Redis 协议（RESP2）与 Thrift**；
  按元信息把请求路由到合适的 Partition 副本。可水平扩展、置于负载均衡之后。
- **重型 SDK：** 面向延迟敏感的重度用户，**跳过 Proxy 直连 DataNode**，是 Client 的简单封装，省一跳。

### 3.2 DataNode（数据面）
- 数据存储节点，线上 **每块盘部署一个 DataNode**（隔离磁盘故障）。
- 最小资源单位是 **Core（绑定一个 CPU 核）**：每个 Core 独立 **Busy Polling 协程框架**，
  请求在 Core 内 **Run-to-Complete**，无线程切换开销。多个 Core 共享一块盘的空间与 IO。
- 一个 Core 承载 **多个 Replica**；**一个 Core 内所有 Replica 共享一个 WAL**，合并碎片化提交、减少 IO 次数。
- 每个 Replica 内为三层结构（见 [`03-storage-engine.md`](03-storage-engine.md)）：
  **数据模型层（Redis 类型）→ 一致性协议层（Anti-Entropy/WAL GC）→ 数据引擎层（暂存层 + 可插拔通用引擎）**。

### 3.3 MetaServer（管控面）
- 多租户中心化架构的 **总管理员**：
  - **逻辑视图：** Namespace / Table / Partition / Replica 的状态、配置与关系。
  - **物理视图：** IDC / POD / Rack / DataNode / Disk / Core 的分布与 Replica 位置。
  - **多租户 QoS 总控：** 在异构机器上按租户与机器负载做副本 Balance 调度。
  - **故障检测与数据修复：** 节点生命周期管理、数据可靠性跟踪、下线与副本重建。
- **不在读写关键路径**：MetaServer 抖动时，已有路由仍可读写，仅调度/修复暂停。

### 3.4 RootServer（管控面）
- 轻量级、**全集群视角** 组件：协调多个集群间的资源配比、支持租户跨集群迁移、提供容灾视图、控制爆炸半径。

### 3.5 DTS（Data Transfer Service，旁路）
- 负责一/二代透明迁移、备份回滚、Dump、订阅等数据流转（见 [`07-backup-recovery.md`](07-backup-recovery.md)）。

## 4. 物理与容灾拓扑

- 一个集群可 **跨多地域**（如华东 Region + 华北 Region），每个 Region 含 **3 个 AZ/IDC**。
- **POD** 是介于 IDC 与机架之间的抽象（非 K8s Pod）：保证 **同一 Partition 的多副本不落在同一 POD**，
  使单房间空调故障/过热/失火不会同时影响一个分片的所有副本。
- 多地域下用 **Main Replicator**（每地域一个）主导跨地域同步，避免网状同步的带宽浪费。

## 5. 读写流转

### 5.1 写路径（`HSET player:{id} gold 100`，W=2,N=3）
1. 客户端经 Proxy（或重型 SDK 直连）按元信息路由到该 Partition 的 **某个就近副本**（Replica Coordinator）。
2. Coordinator 为写分配 **HLC 时间戳**，写本地 **WAL**，并 **并发 forward 到其余副本**。
3. 收到 **≥ W 个副本 WAL 落盘** 响应即返回成功。
4. 落盘数据进入 **数据暂存层**，达条件后合并下刷 **通用引擎层**，WAL 随后 GC。

详见 [`04-replication-consistency.md`](04-replication-consistency.md) §3。

### 5.2 读路径（`HGET player:{id} gold`）
- 按元信息 + **地理位置** 路由到合适副本；Coordinator 依一致性策略查询并按冲突规则合并后返回。
- `R=1` 最快（最终一致）；`W+R>N` 可得写后读一致；读慢副本用 **Backup Request** 规避。

## 6. 控制面 vs 数据面

| 平面 | 组件 | 是否在读写关键路径 | 故障影响 |
| --- | --- | --- | --- |
| 用户面 | Client / Proxy / SDK | 是 | 多实例 + 无状态，可水平扩展兜底 |
| 数据面 | DataNode（Core/Replica） | 是 | 无主多副本，单副本/单 AZ 故障不影响可用性 |
| 管控面 | MetaServer / RootServer | 否 | 调度/修复暂停，已有读写不受影响 |

## 7. 与对标系统的对应关系

| GameStore | Abase2 | TiKV（对照） | Redis Cluster |
| --- | --- | --- | --- |
| DataNode / Core / Replica | DataNode / Core / Replica | Store / —/ Region 副本 | Cluster Node |
| Partition（N 副本，无主） | Partition（无主多写） | Region（Raft 组，单 Leader） | —（主从分片） |
| Replica Coordinator + Quorum | Replica Coordinator + Quorum | Raft Leader + 多数派 | 单主 |
| MetaServer + RootServer | MetaServer + RootServer | PD | Gossip（去中心） |
| Proxy / 重型 SDK | Proxy / 重型 SDK | 智能客户端 | 智能客户端/代理 |
| HLC + LWW + CRDT | HLC + LWW + CRDT | —（单主无冲突） | —（单主无冲突） |
