# 10 · Rust 实现总体框架与 MR 分解计划

> **本文件是什么：** 在 [`MR-0012` 双语言 spike](../EVOLUTION.md) 的事实依据之上，语言选型已拍板为 **Rust**
> （理由见 [`EVOLUTION.md` MR-0013](../EVOLUTION.md)）。本文件给出从 spike 走向生产实现的
> **整体代码框架**（Cargo workspace / crate 划分 / 关键抽象 / 横切关注点）与 **逐个 MR 的落地计划**。
>
> **怎么用：** 本文件是"实现阶段的路线图"。它与 [`09-roadmap.md`](09-roadmap.md) 的关系是——
> `09` 描述 **能力里程碑（Phase 0~6）**；本文件把这些里程碑 **拆成可独立交付、可验证的 Rust 工程 MR**。
> 我们 **一个 item 一个 MR** 顺序推进；每完成一个 MR 在 [`EVOLUTION.md`](../EVOLUTION.md) 追加一条记录。
>
> **原则：** 渐进式交付；每个 MR 都能编译、能测、能独立评审；先立骨架与抽象，再逐层填肉；
> 尽量不返工（抽象边界一次画对，实现可后续替换）。

---

## 1. 设计约束与技术取舍（先定基调）

这些是贯穿所有 MR 的横切决策，先在此定档，避免每个 MR 重复讨论。

| 关注点 | 决策 | 理由 |
| --- | --- | --- |
| 语言 / edition | **Rust**，edition 2021，固定 MSRV（起步用 stable + `rust-toolchain.toml`） | 编译期内存/并发安全契合"不丢数据"北极星（见 MR-0013） |
| 工程组织 | **单一 Cargo workspace + 多 crate** | 清晰的层次边界；crate 即"可替换实现"的抽象单元 |
| 异步运行时 | 起步用 **`tokio`**（多线程调度器）；把引擎调用保持 **同步可调用**，为后续切换 thread-per-core 留口 | 先要开发速度与生态；`glommio`/`monoio` 的 thread-per-core（对齐设计文档的 "Core / Run-to-Complete"）作为 Phase-2+ 的性能优化项，不阻塞早期交付 |
| RocksDB 绑定 | **`rust-rocksdb`（`rocksdb` crate）**，bundled | spike 已验证；TiKV 同款；支持我们最关键的 Compaction Filter |
| 错误处理 | 库 crate 用 **`thiserror`** 定义具体错误；二进制 crate 用 **`anyhow`** 聚合 | 库要可匹配的错误类型，应用层要方便的上下文链 |
| 配置 | **`serde` + TOML**，支持文件 + 环境变量覆盖 | 与云原生部署（ConfigMap/env）对齐 |
| 可观测性 | **`tracing`**（结构化日志 + span）+ **`metrics`**（Prometheus exporter） | 对齐 [`08-observability-ops.md`](08-observability-ops.md) 的指标口径 |
| 序列化（内部 RPC / 元信息） | 起步 **`prost`（protobuf/gRPC via `tonic`）** 或 `bincode`（待 I-09 定档） | RPC 需要跨进程演进兼容；元信息落盘可用紧凑格式 |
| 磁盘编码 | **沿用 spike 的 `元数据键 + 子键 + 版本号` 逐字节布局** | 与 `03-storage-engine.md` §2 一致；spike 已验证可与 C++ 互读 |
| 测试 | 单元（每 crate）+ 集成（复用 spike 的 `redis_functional_test.py` 兼容性用例）+ 属性测试（`proptest` 覆盖编码/CRDT）+ 并发（`loom` 覆盖共享状态） | 分层验证；兼容性用例是"对 Redis 行为一致"的回归网 |
| 代码质量门禁 | CI 强制 `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test`、`cargo deny`（License/审计） | 每个 MR 合入前必须绿 |

> **关于 "Core / Run-to-Complete"：** 设计文档（[`02-architecture.md`](02-architecture.md) §3.2）描述的是"每 Core 绑核 + busy-poll + 单核内 Run-to-Complete + 多 Replica 共享 WAL"的目标形态。
> 我们 **不在 Phase-1 就上 thread-per-core**：先用 tokio 把功能与正确性做扎实，把"每 Core 一个 WAL / 一组 Replica"建模为 **逻辑上的 `Core` 单元**（一个 tokio 任务组），
> 待 Phase-2 引入 WAL 与多副本后，再评估切换到 `glommio`/`monoio` 的 shard 化运行时（作为独立性能 MR）。这条边界在 I-01 的 crate 抽象里预留。

---

## 2. 整体代码框架（Cargo Workspace）

### 2.1 Workspace 与 crate 布局

在仓库根引入 `Cargo.toml`（`[workspace]`），代码统一放在 `crates/`（`spike/` 保留为参考，直到被对应 crate 取代）：

```
gamestore/                         # 仓库根（现有 docs/ 与 spike/ 不动）
├─ Cargo.toml                      # [workspace] 成员清单 + 公共 [workspace.dependencies]
├─ rust-toolchain.toml             # 固定工具链
├─ deny.toml                       # cargo-deny 策略
├─ crates/
│  ├─ gamestore-common/            # 基础设施：错误、配置、HLC、ID、指标/日志门面
│  ├─ gamestore-protocol/          # RESP2/RESP3 编解码（sans-IO 核心 + tokio 适配）
│  ├─ gamestore-engine/            # 通用引擎层：GeneralEngine trait + RocksDB 实现 + 编码 + Compaction GC
│  ├─ gamestore-datamodel/         # 数据模型层：String/Hash/Set/ZSet/List 命令 → 引擎
│  ├─ gamestore-wal/               # 每 Core 共享 WAL + 崩溃恢复
│  ├─ gamestore-replication/       # HLC、暂存层/冲突解决、CRDT、Coordinator/Quorum、Anti-Entropy、副本 RPC
│  ├─ gamestore-datanode/          # [bin] DataNode：Core/连接服务/命令分发/装配
│  ├─ gamestore-meta/              # [bin] MetaServer：元信息/路由表(带 epoch)/心跳/调度
│  ├─ gamestore-proxy/             # [bin] Proxy：RESP 前端/路由/QoS/Backup Request
│  └─ gamestore-cli/               # [bin] 运维与压测工具（bench、admin）
├─ operator/                       # (Phase 5) Kubernetes Operator（可能独立技术栈）
└─ tests/                          # 跨 crate 集成测试 + Redis 兼容性一致性用例
```

### 2.2 分层与文档的对应关系

crate 边界严格对齐设计文档中的层次，便于逐文件对照评审：

```
用户面 ── gamestore-proxy  ─────────────┐  (02-architecture §3.1)
                                        │  RESP → 路由 → QoS
数据面 ── gamestore-datanode ───────────┤  (02-architecture §3.2)
           │  连接/分发/装配             │
           ├─ gamestore-protocol        │  RESP2/RESP3      (spike: resp.rs)
           ├─ gamestore-datamodel        │  Redis 类型层     (03-storage-engine §2, spike: commands/storage)
           ├─ gamestore-replication      │  一致性协议层     (04-replication-consistency)
           │    ├─ HLC / 暂存层 / CRDT
           │    ├─ Coordinator + Quorum
           │    └─ Anti-Entropy
           ├─ gamestore-wal              │  持久化           (03-storage-engine §6)
           └─ gamestore-engine           │  数据引擎层(通用) (03-storage-engine §1/§2/§4, spike: encoding/gc/storage)
管控面 ── gamestore-meta ────────────────┘  (02-architecture §3.3, 05-sharding-routing)
```

### 2.3 关键抽象（trait 草图，示意用，非最终 API）

这些是"一次画对、后续可替换实现"的核心接口。以下为 **拟定** 代码，供评审讨论：

**通用引擎层**（`gamestore-engine`）——抽象 RocksDB/LSH，令上层与具体引擎解耦：

```rust
/// 通用引擎层：只面对"单版本最终值"（多版本冲突已在暂存层解决）。
pub trait GeneralEngine: Send + Sync {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;
    fn write(&self, batch: WriteBatch) -> Result<()>;          // 组提交
    fn scan_prefix(&self, prefix: &[u8]) -> ScanIter<'_>;       // HGETALL/范围
    fn compact_range(&self, range: Option<Range>) -> Result<()>;
    /// 注册"结构 version"GC 判定（Compaction Filter 用），见 03 §4。
    fn install_gc(&self, predicate: Arc<dyn GcPredicate>);
}
```

**数据模型层**（`gamestore-datamodel`）——把 Redis 命令翻译成引擎操作：

```rust
/// 一个 Redis 命令的执行单元；对 String/Hash/... 各有实现。
pub trait CommandHandler {
    fn execute(&self, ctx: &mut ExecCtx, args: &[Bytes]) -> Reply;
}
/// 命令注册表：名字（大小写不敏感）→ handler + arity 校验。
pub struct CommandRegistry { /* ... */ }
```

**持久化 / 复制**（`gamestore-wal` / `gamestore-replication`）：

```rust
pub trait Wal: Send + Sync {                      // 每 Core 共享
    fn append(&self, records: &[WalRecord]) -> Result<Lsn>;   // 合并提交
    fn sync(&self) -> Result<()>;                              // fsync
    fn replay(&self, from: Lsn) -> ReplayIter<'_>;             // 崩溃恢复
    fn truncate(&self, upto: Lsn) -> Result<()>;               // 下刷后 GC
}

pub trait ReplicaTransport: Send + Sync {         // 副本间 RPC
    async fn forward(&self, peer: ReplicaId, write: &WriteOp) -> Result<Ack>;
    async fn pull_oplog(&self, peer: ReplicaId, from: Seqno) -> Result<Vec<OpLogEntry>>;
}

/// 无主多写协调：任一副本可作为 Coordinator，分配 HLC、并发 forward、收满 W 即确认。
pub struct ReplicaCoordinator<T: ReplicaTransport> { /* ... */ }
```

**HLC 与冲突解决**（`gamestore-common` / `gamestore-replication`）：

```rust
pub struct Hlc { physical_ms: u64, logical: u32 }              // 04 §5
pub trait ConflictResolver {                                   // 暂存层
    /// 按 HLC 做 LWW；对 INCR/APPEND 等走 Operation-based CRDT。
    fn merge(&self, existing: Option<Versioned>, incoming: Versioned) -> Versioned;
}
```

**路由**（`gamestore-meta` / `gamestore-proxy`）：

```rust
pub trait RouteTable {                                         // 带 epoch
    fn route(&self, key: &[u8]) -> (PartitionId, Vec<ReplicaEndpoint>);
    fn epoch(&self) -> u64;
}
```

> 这些 trait 让每个 MR 都能"实现一个具体后端 + 单测其契约"，而不必等上下游就绪。

---

## 3. 逐个 MR 分解（实现计划）

约定：计划里的 item 编号为 `I-01, I-02, …`（**Implementation item**）；实际合入时按顺序领取全局 `MR-####` 编号并在 `EVOLUTION.md` 记录。
每个 item 一个 MR，均需：**能编译 + 有测试 + 有退出标准 + 更新 EVOLUTION.md**。分组对齐 [`09-roadmap.md`](09-roadmap.md) 的 Phase。

### Phase 1 · 单机 MVP（把 spike 转成生产骨架）

- **I-01 · Workspace 与工程基线**
  - **范围：** 建 `Cargo.toml` workspace、`rust-toolchain.toml`、`deny.toml`；建空 crate 骨架（`common/protocol/engine/datamodel/datanode`）；接入 `tracing`/`metrics` 门面、统一 `Error`；CI（fmt + clippy `-D warnings` + test + deny）。
  - **产出：** `cargo build`/`cargo test` 全绿的空骨架 + CI 配置。
  - **退出标准：** CI 绿；`gamestore-datanode` 能起一个"只回 PONG"的 RESP 服务。
  - **依赖：** 无。

- **I-02 · `gamestore-protocol`：RESP2/RESP3 编解码**
  - **范围：** 把 spike 的 `resp.rs` 升级为健壮的 sans-IO 编解码器：完整 RESP2、`HELLO`/RESP3 基础、inline 命令、错误与边界（大 bulk、分片读）；tokio `Framed`/`Decoder` 适配层。
  - **产出：** 独立可测的协议 crate + 属性测试（round-trip）。
  - **退出标准：** 对 `redis-cli`/`redis-py` 的握手与基础命令编解码通过；模糊/边界用例通过。
  - **依赖：** I-01。

- **I-03 · `gamestore-engine`：通用引擎 + 编码 + Compaction GC**
  - **范围：** 定义 `GeneralEngine` trait；RocksDB 实现；移植并加固 spike 的 `encoding.rs`/`gc.rs`/`storage.rs`（元数据键+子键+结构 version、内存 version 表、Compaction Filter GC、启动重建）；`WriteBatch` 组提交；引擎调优参数（Bloom/Block Cache/限速）留出配置位。
  - **产出：** 可独立压测的引擎 crate + `RAWCOUNT/DBSIZE/COMPACT` 内省能力（供一致性用例）。
  - **退出标准：** GC 单测（旧 version/孤儿子键回收到 0）+ 属性测试（编码 round-trip）通过；重启后 version 表正确重建。
  - **依赖：** I-01。

- **I-04 · `gamestore-datamodel`：String + Hash + TTL**
  - **范围：** `CommandRegistry`；String（`SET/GET/DEL/EXISTS/TYPE/EXPIRE/PEXPIRE/TTL/PTTL`）与 Hash（`HSET/HMSET/HGET/HMGET/HGETALL/HDEL/HLEN/HEXISTS`）；惰性过期；参数/arity 校验与 Redis 一致的错误信息。
  - **产出：** 命令层 crate + 单测。
  - **退出标准：** 复用 spike 的 `test/redis_functional_test.py` 全部断言在新实现上通过。
  - **依赖：** I-02, I-03。

- **I-05 · `gamestore-datanode`：单机服务装配**
  - **范围：** tokio 异步连接服务、每连接读写循环、命令分发、优雅关闭、`--config` 加载、`FLUSHDB` 等 admin；把 `Core` 建模为逻辑单元（为后续多 Replica/WAL 预留）。
  - **产出：** 可 `cargo run -p gamestore-datanode` 起服务、标准 Redis 客户端零改造直连。
  - **退出标准：** [`09-roadmap.md`](09-roadmap.md) Phase 1 退出标准——现有 Redis 客户端可直连读写 Hash；重启不丢已落盘数据；通过兼容性用例。
  - **依赖：** I-04。

- **I-06 · 复合类型补全：Set / ZSet / List**
  - **范围：** 按 [`03-storage-engine.md`](03-storage-engine.md) §2.3 的编码实现三类结构及其核心命令；扩展一致性用例。
  - **产出：** 更全的数据模型层 + 用例。
  - **退出标准：** 各类型核心命令与 Redis 语义一致，通过新增用例。
  - **依赖：** I-04（可与 I-05 并行推进，按需拆成多个子 MR：Set / ZSet / List 各一）。

- **I-07 · 可观测性与基准**
  - **范围：** Prometheus 指标（QPS/延迟直方图/引擎统计）、慢日志、`criterion` 微基准（编码/单命令）+ 端到端吞吐脚本。
  - **产出：** `/metrics` 端点 + 基准套件。
  - **退出标准：** 指标口径对齐 [`08-observability-ops.md`](08-observability-ops.md)；有可复现的基线数据。
  - **依赖：** I-05。

### Phase 2 · 多副本无主多写（Quorum + WAL）

- **I-08 · `gamestore-wal`：每 Core 共享 WAL + 崩溃恢复**
  - **范围：** `Wal` trait + 文件实现（分段、组提交/合并 fsync、CRC、`replay`、下刷后 `truncate`）；DataNode 写路径接入"先写 WAL 再入引擎"。
  - **退出标准：** 注入崩溃后重放不丢已确认写、不重复应用；组提交降低 fsync 次数（基准佐证）。
  - **依赖：** I-05。

- **I-09 · 副本 RPC 与集群装配（静态成员）**
  - **范围：** 定 RPC 框架（`tonic`/`prost` 或 `bincode`，本 MR 定档）；`ReplicaTransport`；静态副本拓扑配置；健康检查。
  - **退出标准：** 多个 DataNode 进程能互相建连、转发心跳；契约测试通过。
  - **依赖：** I-05。

- **I-10 · HLC 时间戳**
  - **范围：** `Hlc` 生成/合并（物理+逻辑）、时钟回拨保护；写入分配 HLC 并随记录持久化。
  - **退出标准：** HLC 单调性与并发正确性单测（含 `loom`）；跨节点 HLC 合并符合 04 §5。
  - **依赖：** I-08。

- **I-11 · Replica Coordinator + Quorum 写/读**
  - **范围：** 任一副本可作 Coordinator：分配 HLC → 写本地 WAL → 并发 forward → 收满 **W** 确认；可配置 `N/W/R`；`W+R>N` 写后读一致；读路径按 R 收集并按 HLC 合并。
  - **退出标准：** [`09-roadmap.md`](09-roadmap.md) Phase 2 退出标准——杀任一副本写入不中断、不丢已确认写；`W=1` 极致可用、`W+R>N` 写后读一致。故障注入用例通过。
  - **依赖：** I-09, I-10。

### Phase 3 · 冲突解决与最终一致（HLC + LWW + CRDT + Anti-Entropy）

- **I-12 · 双层引擎：暂存层 + Operation 日志**
  - **范围：** `gamestore-replication` 引入内存暂存层（多版本按 HLC）+ Operation 日志/ReplicaLog（Seqno+HLC）；写先入暂存层，达条件/Checkpoint 合并下刷通用引擎。
  - **退出标准：** 查询走点查（不需合并多版本）；下刷后 WAL/oplog 可 GC；单测覆盖合并触发。
  - **依赖：** I-11。

- **I-13 · LWW + Operation-based CRDT**
  - **范围：** 幂等命令 LWW；非幂等（`INCR`/`APPEND`）与结构类型的 CRDT 语义，保证与 Redis 语义一致。
  - **退出标准：** 并发 `INCR` 无丢更新；乱序应用收敛到同一结果（属性测试 + 交换律/幂等律测试）。
  - **依赖：** I-12。

- **I-14 · Anti-Entropy（ReplicaLog / Seqno 进度向量）**
  - **范围：** 副本间进度向量交换、缺口拉取与修复；替代全量 Merkle diff。
  - **退出标准：** [`09-roadmap.md`](09-roadmap.md) Phase 3 退出标准——分区注入恢复后秒级收敛、`INCR` 等不丢更新、语义与 Redis 一致。
  - **依赖：** I-13。

- **I-15 · Checkpoint 与日志截断**
  - **范围：** 定期把"已一致时间戳前"的 Operation 合并为单版本写入通用引擎并截断日志；最终值 Cache。
  - **退出标准：** 长跑下日志不膨胀；恢复时间可控。
  - **依赖：** I-14。

### Phase 4 · 分布式（Partition + MetaServer + Proxy）

- **I-16 · `gamestore-meta`：MetaServer（元信息 + 路由 + 心跳）**
  - **范围：** Namespace/Table/Partition/Replica 元信息存储；带 epoch 的路由映射；DataNode 心跳/生命周期；故障检测与副本重建调度骨架。
  - **退出标准：** 路由可下发/更新；MetaServer 抖动不影响已有读写（不在关键路径）。
  - **依赖：** I-11。

- **I-17 · Partition 分片与哈希路由 + 扩缩容**
  - **范围：** 哈希分片（兼容 Redis Cluster 16384 槽作为 Proxy 层可选兼容）；Replica 重建 + Anti-Entropy 追平的扩缩容，迁移期不阻塞前台。
  - **退出标准：** 加/减节点对业务透明、负载自动均衡。
  - **依赖：** I-16, I-14。

- **I-18 · `gamestore-proxy`：默认接入层**
  - **范围：** 无状态 Proxy：RESP2/Thrift 前端、按 MetaServer 路由、连接复用、重试、Backup Request、热 Key 承载、限流/鉴权。
  - **退出标准：** [`09-roadmap.md`](09-roadmap.md) Phase 4 退出标准——标准 Redis 客户端零改造经 Proxy 读写；单 AZ 故障整体仍可读写。
  - **依赖：** I-17。

### Phase 5 · 云原生部署（Operator + 备份）

- **I-19 · Kubernetes Operator + `GameStoreCluster` CRD**
  - **范围：** 对齐 [`06-deployment-cloud.md`](06-deployment-cloud.md) 的 CRD（`dataNode/partition/meta/rootServer`）；StatefulSet + 云盘 + 跨 AZ 拓扑；Proxy HPA；滚动升级（无需转主）。
  - **退出标准：** 声明式拉起跨 3 AZ 集群。
  - **依赖：** I-18。

- **I-20 · 备份 / 恢复 / PITR**
  - **范围：** 引擎 Checkpoint 全量 + Operation 日志/ReplicaLog 归档到对象存储；按 HLC 重放 PITR；DTS 迁移骨架。
  - **退出标准：** 备份恢复演练达 [`07-backup-recovery.md`](07-backup-recovery.md) 的 RTO/RPO 目标。
  - **依赖：** I-19。

### Phase 6 · 生产化（类型/多租户/性能）

- **I-21 · 多租户 QoS（NRC / Quota / WFQ）与多维负载均衡**
- **I-22 · 命令与类型补全、按需 Pub/Sub / Lua / Stream、单主半同步模式完善**
- **I-23 · 性能：thread-per-core 运行时评估（`glommio`/`monoio`）、Run-to-Complete、KV 分离、引擎调参、SLO 校准**
- **I-24 · DTS 从 Redis 在线迁移工具链 + 完整可观测性与告警**
  - **退出标准：** 承接真实业务流量达成 SLO，完成 Redis 平滑下线。
  - **依赖：** I-18/I-20（各项可并行，按需继续细拆）。

---

## 4. MR 依赖关系图

```
I-01 ─┬─ I-02 ─┐
      ├─ I-03 ─┴─ I-04 ─┬─ I-05 ─┬─ I-07
      │                 └─ I-06 ─┘        └─ I-08 ─┬─ I-10 ─┐
      │                                   I-09 ────┴────────┴─ I-11 ─ I-12 ─ I-13 ─ I-14 ─┬─ I-15
      │                                                                                   └─ I-16 ─ I-17 ─ I-18 ─ I-19 ─ I-20
                                                                                                             └─ I-21..I-24
```

---

## 5. 每个 MR 的"完成定义"（Definition of Done）

对每一个 item 统一适用，避免遗漏：

1. **可编译可测：** `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test`、`cargo deny` 全绿。
2. **有测试：** 新增逻辑有单元/属性/集成测试；涉及 Redis 语义的必须过兼容性用例。
3. **可独立验证：** 有明确的手动/脚本验证步骤与退出标准。
4. **文档同步：** 若改变对外行为或架构，更新对应 `docs/design/*`。
5. **演进记录：** 在 [`EVOLUTION.md`](../EVOLUTION.md) 追加一条 `MR-####` 记录（动机/决策/范围/后续/关联）。
6. **可回退：** 变更聚焦、单一逻辑；不夹带无关重构。

---

## 6. 与既有 spike 的关系

- `spike/` 目录 **保留为参考与对照材料**（尤其 `rust/` 的 `encoding.rs`/`gc.rs`/`storage.rs`/`resp.rs`）。
- I-02/I-03/I-04 会把 spike 的 Rust 模块 **提升、加固并迁移** 到对应 crate；磁盘编码保持逐字节一致。
- 当对应能力在 `crates/` 中稳定且被兼容性用例覆盖后，可在一个收尾 MR 中删除或归档 spike 的重叠部分（另行决定，不在本计划强制）。

---

## 7. 首个要做的 MR

**I-01（Workspace 与工程基线）** 是唯一没有前置依赖的 item，作为实现阶段第一个 MR 落地；随后按 §4 依赖图推进。
