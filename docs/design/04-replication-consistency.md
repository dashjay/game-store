# 04 · 复制、一致性与高可用（Multi-Raft）

> 本文件说明 GameStore 如何用 **Multi-Raft** 同时满足 **不丢数据**、**单 Key 线性一致** 与 **高可用**，
> 以及故障如何被自动检测与恢复。

## 1. 复制模型：每分片一个 Raft 组

- 键空间被切成多个 **分片（Shard）**，每个分片由 **一个独立的 Raft 组** 负责复制。
- 每个 Raft 组默认 **3 副本**，分布在 **3 个不同可用区（AZ）** 的不同节点上。
- 一个存储节点同时参与 **很多** 分片的 Raft 组（Multi-Raft），既可能是某些分片的 Leader，
  也可能是另一些分片的 Follower。Leader 由 PD 在节点间均衡。

```
分片 S1: [Node A:Leader(AZ1)]  [Node B:Follower(AZ2)]  [Node C:Follower(AZ3)]
分片 S2: [Node B:Leader(AZ2)]  [Node C:Follower(AZ3)]  [Node A:Follower(AZ1)]
分片 S3: [Node C:Leader(AZ3)]  [Node A:Follower(AZ1)]  [Node B:Follower(AZ2)]
```

> 跨 AZ 放置让"单 AZ 整体故障"最多损失每个分片的 1 个副本，多数派仍在，分片继续可用。

## 2. 写入的一致性与持久性

一次写入的 Raft 流程（详见 [`02-architecture.md`](02-architecture.md) §3.1）：

1. 客户端写只接受由 **Leader** 处理。
2. Leader 追加日志条目并 **持久化到本地日志引擎**，同时复制给 Followers。
3. 当 **多数派（≥2/3）持久化** 该条目后，条目 **committed**。
4. Leader 将 committed 条目 **apply** 到 RocksDB 状态机，然后回包成功。

由此得到两条强保证：

- **不丢数据：** 成功返回 ⇒ 至少多数派已落盘 ⇒ 任意单点（甚至单 AZ）故障后，新 Leader 必含该数据。
- **单 Key 线性一致：** 同一分片所有写经由唯一 Leader 线性定序，配合下文一致读，读总能看到已确认的最新写。

### 2.1 写入批处理（高频写关键优化）
- Leader 把短时间窗内的多条写 **打包成一批** 走一次 Raft 提交（log batching + group commit），
  把"每玩家 50~100 次/秒"的高 QPS 转化为更少的提交次数，显著降低 fsync 与复制开销。
- 与存储引擎的 WriteBatch / WAL 组提交协同（见 [`03-storage-engine.md`](03-storage-engine.md) §5）。

## 3. 读取的一致性级别

| 级别 | 路由 | 机制 | 一致性 | 适用 |
| --- | --- | --- | --- | --- |
| 线性一致读（默认） | Leader | Leader Lease 或 ReadIndex | 读到最新已确认写 | 玩家财产、强一致读 |
| 有界陈旧读（可选） | Follower | Follower Read + apply 进度约束 | 可能滞后有限时间 | 读多写少、可容忍轻微滞后 |

- **Leader Lease：** Leader 在租约有效期内确信自己仍是唯一 Leader，可直接本地读，**无需多数派往返**，延迟最低。
- **ReadIndex：** 读前记录当前 commit index 并确认仍为 Leader，待状态机 apply 到该 index 后再读，
  不依赖时钟假设，更稳健。
- **Follower Read：** 显式开启时，Follower 用从 Leader 获取的 ReadIndex 保证不读到比该点更旧的数据，
  以扩展读吞吐，代价是有界陈旧——这是业务按需选择的明确让步。

## 4. 故障检测与转移

### 4.1 Leader 故障
- Followers 在 **选举超时** 内未收到心跳即发起选举，多数派投票选出新 Leader（秒级）。
- 期间该分片短暂不可写；其他分片不受影响。
- Proxy 通过重试 + 路由表更新自动切到新 Leader，对客户端尽量透明。

### 4.2 节点故障与副本补齐
- PD 通过 **心跳** 监控节点健康。节点失联超过阈值，PD 判定其上副本失效。
- PD 选择健康节点，通过 Raft **成员变更** 在其上 **新增一个副本**，由 Leader 发送
  **快照（RocksDB Checkpoint）+ 增量日志** 完成数据追平，恢复复制因子到 3。
- 随后移除故障节点上的旧副本成员。

### 4.3 成员变更的安全性
- 副本增减使用 Raft 的 **Joint Consensus（联合共识）** 或单步成员变更，确保变更过程中始终存在唯一确定的多数派，
  不会出现脑裂或双多数派。

## 5. 可用性边界（与 CAP 立场一致）

- 分片 **多数派存活** ⇒ 可读可写。
- 分片 **丢失多数派**（如同时挂掉 2/3 副本，或 2 个 AZ 同时不可用）⇒ 该分片 **暂停写入以保证不丢数据/不脑裂**，
  待 PD 重建多数派或节点恢复。其余分片照常服务。
- 这是有意的 **CP 取舍**：对玩家财产，宁可短暂不可写，也不接受丢数据或不一致。

## 6. 与 Redis 主从复制的对比

| 维度 | Redis 主从 + 哨兵 | GameStore（Raft） |
| --- | --- | --- |
| 持久性 | 异步复制，主挂可能丢已确认写 | 多数派落盘才确认，不丢已确认写 |
| 故障转移 | 哨兵选主，存在数据回退风险 | Raft 选主，新 Leader 必含已提交数据 |
| 一致性 | 最终一致（异步） | 单 Key 线性一致 |
| 脑裂 | 可能出现双主写入 | Joint Consensus 保证唯一多数派 |

这正是 GameStore 取代"Redis 当持久库"的核心价值：**用一致性协议把"可能丢数据"变成"不丢数据"**。
