# 03 · 存储引擎与 Redis 数据编码

> 本文件说明 GameStore 如何在 **RocksDB（LSM-Tree）** 之上实现 Redis 数据类型，
> 以及为高频写场景所做的引擎选型与调优方向。

## 1. 为什么是 RocksDB / LSM-Tree

- **写优化。** LSM 把随机写转化为内存 MemTable 追加 + 顺序刷盘，天然适配"高频小写入"。
- **覆盖友好。** 同一 Key 的高频更新在 LSM 中表现为多版本追加，读取取最新、Compaction 合并旧版本，
  无需原地更新，避免读改写放大。
- **成本友好。** 数据主体在 SSD，内存只承担 MemTable 与 Block Cache，单位容量成本远低于全内存。
- **成熟可靠。** RocksDB 经大规模生产验证，参数丰富，便于按负载调优；TiKV/Kvrocks 均以其为基。

代价是 **写放大与 Compaction 开销**，这正是后文调优与"写合并"策略要控制的重点。

## 2. 编码总原则：元数据键 + 子键 + 版本号

Redis 的复合类型（Hash/Set/ZSet/List）不能直接塞进扁平 KV，需要编码。GameStore 采用与
Kvrocks/TiKV-Redis 类似的 **"元数据键 + 子键 + 版本号"** 方案：

- 每个用户可见 Key 有一条 **元数据（metadata）记录**，存类型、版本号 `version`、过期时间 `expire`、
  以及类型相关的统计（如 Hash 的字段数）。
- 复合类型的每个成员是一条 **子键（subkey）记录**，键里嵌入所属 Key 与其 **当前 version**。
- 删除/重建 Key 时只需 **递增 version**（O(1) 逻辑删除），旧 version 的子键由
  **Compaction Filter** 在后台物理回收，无需同步扫描删除海量子键。

所有 Key 还会带上 **分片前缀**（用于同机多分片在共享 RocksDB 中的隔离，见 §6）。下文为简洁省略该前缀。

### 2.1 String

```
metakey = enc(key)                → | type=string | version | expire | value |
```

String 较简单，可将值直接放在元数据记录中。

### 2.2 Hash（玩家数据的主载体）

```
metakey = enc(key)                → | type=hash | version | expire | field_count |
subkey  = enc(key) | version | field   → value
```

- `HSET key f v`：写/更新 `subkey`，必要时更新元数据的 `field_count`。
- `HGET key f`：先读元数据校验存活与 version，再读对应 `subkey`。
- `HGETALL key`：按 `enc(key) | version` 前缀做范围扫描，一次取全（同分片局部性保证高效）。
- `DEL key`：递增 version → 旧 version 的所有 `subkey` 成为垃圾，由 Compaction Filter 回收。

> 针对 [`01-workload-data-model.md`](01-workload-data-model.md) 中"Hash 聚合玩家字段"的模型，
> Hash 是一等公民：`HSET` 多字段、`HMGET`、`HGETALL` 都在单分片内高效完成。

### 2.3 Set / ZSet / List（概要）

- **Set：** `subkey = enc(key)|version|member → ∅`，成员存在性由键表达。
- **ZSet：** 双重编码——`enc(key)|version|member → score`（按成员查分）与
  `enc(key)|version|score|member → ∅`（按分数范围有序扫描，支持 `ZRANGEBYSCORE`）。
- **List：** `subkey = enc(key)|version|index → value`，用单调递增/递减的 index 支持两端 push/pop；
  元数据维护 head/tail index。

## 3. 过期（TTL）实现

- 过期时间戳存在 **元数据** 中。读取时若发现已过期，视为不存在并触发惰性删除（递增 version）。
- 真正的空间回收交给 **Compaction Filter**：在 Compaction 时丢弃"已过期 Key"或"version 已失效"的记录。
- 可选后台扫描线程主动清理长期不被访问的过期 Key，避免空间长期占用。

## 4. 版本号 + Compaction Filter 的价值

这是该编码方案的核心收益，尤其契合游戏场景：

- **快速整体删除/重建。** 玩家数据重置、赛季清档等"删大 Hash"操作变成 O(1) 的 version 递增。
- **后台异步 GC。** 海量过期/失效子键的物理回收与前台请求解耦，避免删除尖刺。
- **天然多版本隔离。** 重建后的新数据用新 version，与旧 version 物理隔离，读不会串到旧数据。

## 5. 面向高频写的 RocksDB 调优方向

> 具体数值需结合 [`01-workload-data-model.md`](01-workload-data-model.md) 与压测确定，这里给方向。

- **MemTable：** 适当增大单个 MemTable 与最大 MemTable 数，吸收写峰值、减少刷盘频率。
- **WAL 与组提交：** 开启 group commit，把多笔写的 WAL 落盘合并，降低 fsync 次数（与 Raft batch 协同）。
- **Compaction：** 评估 Level vs Universal Compaction 在本负载下的写放大/空间放大权衡；
  对高频覆盖的 Hash 子键，Compaction 能高效合并同 Key 多版本。
- **限速（Rate Limiter）：** 给 Compaction/Flush 限速，避免 I/O 抢占前台写、平滑长尾延迟。
- **Block Cache：** 内存主要用于 Block Cache 命中热数据（在线玩家），冷数据留在 SSD。
- **Bloom Filter：** 为点查（`HGET`）开启，减少无效 SST 访问。
- **KV 分离（可选）：** 若实测大 Value（如整玩家快照 JSON）占比高，评估 BlobDB 以降低 Compaction 搬运成本；
  小值为主时不必启用（属 [`EVOLUTION.md`](../EVOLUTION.md) 待决问题）。

## 6. 数据引擎与 Raft 日志引擎分离

写入路径上有两类持久化：**Raft 日志**（顺序追加、提交后可截断）与 **KV 数据**（状态机应用结果）。
二者混在同一个 RocksDB 会相互干扰（日志的高频追加扰动数据的 Compaction）。因此：

- **KV 数据：** 每节点一个共享 RocksDB 实例，多分片以 **分片前缀** 隔离 Key 空间。
- **Raft 日志：** 使用 **独立日志引擎**（专用于顺序追加 + 区间截断的轻量引擎，类 TiKV Raft Engine，
  或独立 RocksDB 实例），与数据引擎分盘/分实例，互不干扰。

这一分离对"高频写"尤为关键：它把"日志落盘"和"数据 Compaction"两条 I/O 流解耦，稳定写延迟。

## 7. 一次写入在引擎内的完整路径

```
Raft committed entry
        │
        ▼
 状态机 apply：解码命令（如 HSET）
        │
        ├─ 读/更新元数据记录（type/version/expire/field_count）
        ├─ 写入/更新子键记录（enc(key)|version|field → value）
        │
        ▼
 RocksDB WriteBatch（原子写入元数据 + 子键，含数据引擎 WAL）
        │
        ▼
 记录 applied_index（用于重启恢复与 ReadIndex）
```

元数据与子键的更新打包进 **同一个 WriteBatch 原子提交**，保证状态机应用的原子性与可重放性。
