# 03 · 存储引擎与 Redis 数据编码

> 本文件说明 GameStore 在单个 Replica 内的 **三层结构** 与 **双层引擎**，
> 如何用 **HLC 版本 + 暂存层冲突合并 + 可插拔通用引擎** 支撑无主多写，
> 以及在通用引擎（RocksDB）上如何编码 Redis 数据类型。
>
> **路线说明：** 本文件在 MR-0007/0008 由"单层 RocksDB + Raft 日志引擎"调整为 Abase 式 **双层引擎**，
> 详见 [`../EVOLUTION.md`](../EVOLUTION.md) 与 [`04-replication-consistency.md`](04-replication-consistency.md)。

## 0. Replica 三层结构与双层引擎（总览）

每个 Replica 内为三层（对齐 Abase2）：

```
┌─────────────────────────────────────────────────────────┐
│ 数据模型层   String / Hash / Set / ZSet / List 的 Redis 接口 │
├─────────────────────────────────────────────────────────┤
│ 一致性协议层 Anti-Entropy 冲突合并 + 下刷协调 + WAL GC       │
├─────────────────────────────────────────────────────────┤
│ 数据引擎层（双层）                                          │
│   ① 数据暂存层(Conflict Resolver)：多版本(按 HLC)冲突合并    │
│      —— SkipList / RocksDB memtable，常驻内存、小时间窗      │
│   ② 通用引擎层(可插拔)：仅存合并后的单版本最终值             │
│      —— LSM(RocksDB/TerarkDB) 或 LSH(点查延迟更稳)          │
└─────────────────────────────────────────────────────────┘
            ▲ 持久化：每 Core 一个共享 WAL（先写 WAL 再入暂存层）
```

**为什么是双层引擎？** 多写下同一 Key 会有多个带 HLC 的版本，但正常网络下 **秒级即可确定最终有效值**。
若把所有版本直接写入 RocksDB，会导致：查询要 `seek` 多版本（比 `get` 慢）、需后台扫描回收无效版本（耗 CPU/IO）、
引擎层与多版本耦合而无法插件化。因此把"多版本冲突解决"收敛到 **内存暂存层**，**只把最终单版本落到通用引擎层**——
查询走点查、引擎层可按业务插拔（见 §6）。

## 1. 通用引擎层：为什么默认 RocksDB / LSM-Tree

- **写优化。** LSM 把随机写转化为内存 MemTable 追加 + 顺序刷盘，天然适配"高频小写入"。
- **覆盖友好。** 同一 Key 的高频更新在 LSM 中表现为多版本追加，读取取最新、Compaction 合并旧版本。
- **成本友好。** 数据主体在 SSD，内存只承担 MemTable 与 Block Cache，单位容量成本远低于全内存。
- **可插拔。** 通用引擎层抽象了接口：有顺序需求用 **RocksDB/TerarkDB（LSM）**；
  纯点查、要求延迟更稳定用 **LSH 引擎**。大 Value 还可走 KV 分离（见 §5）。

## 2. 编码总原则：元数据键 + 子键 + 版本号

这里指 **通用引擎层** 上的编码（即冲突已被暂存层合并后的最终单版本数据）。
Redis 的复合类型（Hash/Set/ZSet/List）不能直接塞进扁平 KV，GameStore 采用与
Kvrocks 类似的 **"元数据键 + 子键 + 版本号"** 方案：

- 每个用户可见 Key 有一条 **元数据（metadata）记录**，存类型、版本号 `version`（结构版本）、过期时间 `expire`、
  以及类型相关的统计（如 Hash 的字段数）。
- 复合类型的每个成员是一条 **子键（subkey）记录**，键里嵌入所属 Key 与其 **当前 version**。
- 删除/重建 Key 时只需 **递增 version**（O(1) 逻辑删除），旧 version 的子键由
  **Compaction Filter** 在后台物理回收，无需同步扫描删除海量子键。

> 区分两类"版本"：**HLC 时间戳** 用于 **跨副本冲突解决/全排序**（暂存层、Operation 日志，见 [`04-replication-consistency.md`](04-replication-consistency.md)）；
> 这里的 **结构 version** 用于通用引擎层的 **整 Key 逻辑删除与子键 GC**。二者职责不同。

所有 Key 还会带上 **Partition 前缀**（用于同一 Core 内多 Replica 在引擎中的隔离）。下文为简洁省略该前缀。

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

- **MemTable / 暂存层：** 暂存层本身常用 RocksDB memtable/SkipList；通用引擎适当增大 MemTable 吸收写峰值。
- **限速（Rate Limiter）：** 给 Compaction/Flush 限速，避免 I/O 抢占前台写、平滑长尾延迟。
- **Block Cache + 最终值 Cache：** 热数据（在线玩家）命中内存；CRDT 最终值缓存在内存 Cache，使查询不必合并 Operation 日志。
- **Bloom Filter：** 为点查（`HGET`）开启，减少无效 SST 访问。
- **KV 分离（可选）：** 大 Value（整玩家快照 JSON）占比高时启用 KV 分离降低写放大；
  **带 TTL 的大 Value 可只写 Log 等其失效、不进通用引擎**（Abase 实践），小值为主时不必启用。

## 6. WAL、Operation 日志与 Checkpoint

无主多写下，单个 Replica 内的持久化与一致性数据流如下（取代原 Raft 日志引擎方案）：

- **WAL（每 Core 共享一个）：** 写入先落 WAL 保证持久化，再进暂存层；一个 Core 内多个 Replica 共享一个 WAL，
  **合并碎片化提交、减少 IO**。数据下刷通用引擎后，对应 WAL 即可 GC。
- **Operation 日志 / ReplicaLog：** 用于 CRDT 合并与 Anti-Entropy（见 [`04-replication-consistency.md`](04-replication-consistency.md) §7、§8）。
  每条带严格递增 Seqno 与 HLC 时间戳；正常情况下只需常驻内存，极端（网络分区）才 dump 到盘。
- **Checkpoint：** 定期把"已达成一致时间戳之前"的 Operation 合并为单一结果写入通用引擎层，并截断对应日志，
  防止日志膨胀、并让查询走点查。

## 7. 引擎可插拔

通用引擎层抽象统一接口，按业务选择：

| 引擎 | 特性 | 适用 |
| --- | --- | --- |
| RocksDB / TerarkDB（LSM） | 有序、范围扫描、写优化 | 需要顺序/范围（如 ZSet 排行榜）、通用场景 |
| LSH 引擎 | 纯点查、延迟更稳定 | 无序点查为主、对长尾延迟敏感 |

> 因为多版本冲突已在暂存层解决，通用引擎层只面对"单版本最终值"，因此能保持简单、可替换。

## 8. 一次写入在 Replica 内的完整路径

```
Replica Coordinator 收到写（已分配 HLC 时间戳）
        │
        ▼
 写入本地 WAL（持久化）—— 与同 Core 其他 Replica 合并提交
        │  并发 forward 到其余副本（满足 Quorum W 即向客户端返回成功）
        ▼
 提交到数据暂存层（按 HLC 做 LWW / CRDT 冲突合并）
        │
        ▼（达到条件 / Checkpoint）
 合并为单版本最终值，编码为「元数据 + 子键」写入通用引擎层
        │
        ▼
 对应 WAL / Operation 日志可被 GC；最终值进入内存 Cache 供点查
```

> 与原 Raft 方案的关键差异：**没有"committed→apply"的单一定序**；写入在副本间可乱序，
> 最终一致由 **HLC 排序 + 暂存层合并 + Anti-Entropy** 保证。
