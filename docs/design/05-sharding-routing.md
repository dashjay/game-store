# 05 · 分片、路由与扩缩容

> 本文件说明 GameStore 如何把键空间切成 Partition、如何路由请求、如何多地域就近访问，
> 以及如何在不停服的前提下扩缩容。
>
> **路线说明：** 本文件在 MR-0007/0008 由"Redis Cluster 16384 槽位 + Raft Leader + PD"调整为
> Abase 式 **Partition/Replica（无主）+ MetaServer 路由**，详见 [`../EVOLUTION.md`](../EVOLUTION.md)。

## 1. 分片模型：Namespace → Table → Partition → Replica

- 一个 **Table** 的键空间被切成多个 **不重叠的 Partition（分片）**；分片是路由与复制的单位。
- 划分方式：对 Key 做哈希分配到 Partition（玩家 ID 哈希离散 → 负载天然均匀）。
  使用 hash tag 时仅 `{...}` 内子串参与计算（见 [`01-workload-data-model.md`](01-workload-data-model.md) §3.2）。
- 每个 Partition 有 **N 个 Replica（无主，任一副本可读写）**，跨 AZ/POD 放置（见 [`04-replication-consistency.md`](04-replication-consistency.md)）。
- **Partition → Replica 位置** 的映射由 **MetaServer** 维护，并带 **版本号/epoch** 以便检测过期路由。

> **与 Redis Cluster 槽位的关系（可选兼容）：** 若业务使用感知 Redis Cluster 协议的客户端，
> Proxy 可在接入层把 Partition 映射为 16384 槽位对外暴露 `CLUSTER`/`MOVED`/`ASK` 语义；
> 但 **内部路由以 MetaServer 的 Partition 元信息为准**，不依赖去中心 Gossip。

## 2. 路由

### 2.1 经 Proxy 路由（默认）
1. Proxy/Client 经 **MetaSync** 从 MetaServer 同步 **路由表缓存**（Partition → Replica 位置），并订阅增量更新。
2. 对每个请求计算其 Partition → 按 **地理位置等信息** 选择一个合适的 **就近副本** 转发（无主，任一副本可处理）。
3. 若因迁移导致路由过期，后端返回带新位置的提示，Proxy **内部重试** 到正确副本，对客户端透明。

### 2.2 重型 SDK 直连（可选）
- 延迟敏感的重度用户用 **重型 SDK** 缓存路由表、**直连 DataNode**，省去 Proxy 一跳。
- 路由过期时按提示更新本地路由并重试。

> **无需选主路由：** 因为没有 Leader，路由不必"找主"，任一存活副本即可服务——这也是高可用的一部分。

## 3. 扩容（加节点）

目标：吞吐/容量随节点线性增长，过程对业务透明、不丢数据、不长时间阻塞。

1. 新 DataNode 注册到 MetaServer，加入资源池。
2. MetaServer 制定再均衡计划：把部分 **Partition 的 Replica** 调度到新节点，使负载更均匀。
3. 迁移以 **Replica 为单位**：在新节点上新建一个 Replica，通过 **Anti-Entropy（ReplicaLog 拉取）+ 引擎数据传输** 追平，
   达到副本数后移除旧 Replica。**全程不阻塞前台读写**（无主，期间其他副本照常服务）。
4. 迁移完成后 MetaServer 更新路由映射与 epoch，Proxy/SDK 增量感知。

> 相比 Raft 方案：副本搬迁不涉及成员变更投票，也不需要"转移 Leader"，迁移更轻、对可用性影响更小。

## 4. 缩容（减节点）

1. MetaServer 标记待下线 DataNode，将其上所有 Replica **逐个重建** 到其他健康节点（始终保持每个 Partition 副本数达标）。
2. 全部重建完成且副本数满足后，安全移除该节点。
3. 全程不影响整体可用性。

## 5. 负载均衡与热点治理（多租户）

- **多维负载均衡：** MetaServer 依据每个 Core 的 **QPS 与磁盘使用率** 构建二维负载向量，
  把高负载 Core 上的 Replica 调度到低负载 Core，使各 Core 负载趋近全局最优向量。
  目标：同租户 Replica 尽量分散（便于 Quota 扩容）、不被慢节点阻塞、各维度负载百分比接近。
- **跨 AZ/POD 反亲和：** 同一 Partition 的 N 副本强制分散到不同 AZ/POD。
- **多租户 QoS：** DataNode 按 **Normalized Request Cost（NRC）** 量化读/写/Scan 请求，
  经 **Tenant Quota Gate + 分级 WFQ** 双层结构限流与公平调度，防止个别租户突增流量打垮节点或影响其他租户。
- **热点 Key：**
  - 监控识别热点（高 QPS 的单 Key）。
  - 缓解：Client/Proxy 侧热 Key 承载 + Backup Request；对热点写，多写架构本身分散了写入点；
    必要时引导业务用 hash tag 拆分或冗余。
- **倾斜防护：** 限制 hash tag 滥用，避免把过多 Key 钉到同一 Partition 造成倾斜。

## 6. 多地域与就近访问

- 一个集群可跨多地域部署，**任一地域的用户就近读写**（多主天然支持）。
- 每个地域设 **Main Replicator** 主导跨地域同步，避免网状同步的带宽浪费（见 [`02-architecture.md`](02-architecture.md) §4）。
- 跨地域冲突同样由 **HLC + LWW/CRDT** 解决，达成最终一致，无需业务关心同步链路。

## 7. 元数据一致性与防错

- MetaServer 维护权威元信息；RootServer 提供全集群视角与容灾。
- 每个 Partition 携带 **epoch**；携带过期 epoch 的路由会被纠正并触发刷新，防止迁移过程中向旧位置误写。

## 8. 扩展性小结

| 增长来源 | 应对手段 |
| --- | --- |
| 在线人数上升 → 写 QPS 上升 | 加节点 + Replica 再均衡，写入点分散到多副本/多节点，吞吐线性扩展 |
| 数据量上升 | 加节点分担容量，SSD 主存成本可控；多租户资源池化提升利用率 |
| 读 QPS 上升 | 任一副本可读 + 就近路由 + Backup Request；必要时增加副本数 N |
| 局部热点 | 热点识别 + 热 Key 承载 + 多写分散 + Key 设计引导 |
