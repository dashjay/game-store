# I-08 写路径基线：WAL 落地后复跑（2026-07-02）

> **这是什么：** I-08（`gamestore-wal`，每 Core 共享 WAL + 崩溃恢复）把 `fsync`
> 带入了前台写路径。本文件对照 [I-07 基线](2026-07-02-i07-baseline.md) 复跑写路径，
> 记录 **WAL 落地后的新基线**，并给出 **组提交降低 fsync 次数** 的基准佐证与
> MR-0018/0020 "引擎调用同步内联" 决策的再复核结论。
>
> 数字与机器强相关（同一台云 VM，存在邻居噪声），价值在于 **同 flag 复跑可对比** 与
> **量级判断**，绝对值不构成 SLO 承诺。

## 环境

与 I-07 基线同机同工具链：Intel Xeon（4 vCPU）/ 16 GiB 云 VM、Rust 1.83.0、
criterion 走 `bench` profile、端到端服务为 `--release`、`EngineConfig::default()`。
本机 `fsync`（`sync_data`）实测约 **0.1–1 ms**。WAL 路径下 RocksDB 自带 WAL 关闭
（`disable_rocksdb_wal=true`），全部持久化由本 WAL 承担（避免双写日志）。

## 1. 组提交微基准（criterion）—— fsync 次数的直接佐证

复跑：`cargo bench -p gamestore-wal`

`wal_write_path`：对 64 条"玩家字段大小"记录（小 key + 64B value）比较
**每条各 fsync 一次** vs **整批追加后只 fsync 一次**：

| 基准 | 每批(64 条)中位耗时 | 吞吐 | 每条摊薄 |
| --- | --- | --- | --- |
| `fsync_per_record`（64 次 fsync） | ~7.11 ms | ~9.0 Kelem/s | ~111 µs/条 |
| `grouped_single_fsync`（1 次 fsync） | ~231 µs | ~277 Kelem/s | ~3.6 µs/条 |

**把 64 条写入合并为一次 fsync，吞吐提升约 30×。** 这正是 WAL `sync()` 的
leader/follower 组提交在并发写者到达同一提交点时所做的事——`fsync` 是持久化写的主导
成本，摊薄它就是收益。单元测试 `group_commit_coalesces_concurrent_fsyncs`
（`crates/gamestore-wal/tests/file_wal.rs`）进一步以 **fsync 次数 < 并发写者数**
的断言在 CI 中固定这一性质。

## 2. 端到端写路径：WAL 开 vs 关（真实 redis-py 经 TCP）

复跑：

```bash
# WAL 开（默认，崩溃可恢复）
cargo run -p gamestore-datanode --release -- --port 6390 --data-dir /tmp/gs-on
python3 tests/bench_throughput.py --port 6390 --ops 20000 --pipeline 100 \
    --clients 32 --metrics-url http://127.0.0.1:9600/metrics

# WAL 关（no-op WAL，仅用于隔离 fsync 成本，无持久化保证）
GAMESTORE_WAL__ENABLED=false cargo run -p gamestore-datanode --release -- \
    --port 6391 --data-dir /tmp/gs-off
python3 tests/bench_throughput.py --port 6391 --ops 20000 --pipeline 100 --clients 32
```

| 工作负载 | WAL 关（≈I-07 基线） | WAL 开 |
| --- | --- | --- |
| SET 64B 顺序 | ~22,600 ops/s（44 µs/op） | ~5,650 ops/s（177 µs/op） |
| GET 顺序 | ~25,100 ops/s | ~25,000 ops/s |
| HSET 顺序 | ~22,900 ops/s | ~5,950 ops/s |
| HGET 顺序 | ~24,800 ops/s | ~24,000 ops/s |
| ZADD 顺序 | ~22,100 ops/s | ~5,930 ops/s |
| LPUSH 顺序 | ~24,300 ops/s | ~5,920 ops/s |
| SET 64B pipeline(100) | ~102,700 ops/s | ~8,370 ops/s |
| GET pipeline(100) | ~152,900 ops/s | ~152,700 ops/s |
| HSET pipeline(100) | ~94,000 ops/s | ~8,400 ops/s |
| SET 64B 并发 32 连接 | ~17,600 ops/s | ~13,100 ops/s（~1.5 写/fsync） |

读取（GET/HGET）不受影响——WAL 只在写路径上。WAL 关的数字与 I-07 基线一致
（SET 顺序 ~22.6k、pipeline ~102k），确认此次改造对读路径与非 WAL 路径无回归。

**观察与解读：**

- **单连接写入变为 fsync 受限。** 每条写多一次 `fsync`（本机 ~130 µs），顺序写从
  ~22.6k 降到 ~5.6k ops/s。这是持久化的应有代价（I-07 前台无 fsync）。
- **单连接 pipeline 对写入不再放大。** 同一连接上的命令在一个 tokio 任务里 **串行**
  执行，各自 append+sync，彼此不并发 → 无法在提交点合并 → pipeline(100) 写吞吐≈
  单条 fsync 上限（~8.4k）。这是当前实现的已知边界（见 §3）。
- **并发写者靠组提交回补吞吐。** 32 个并发连接下 SET 从单连接 ~5.6k 提升到 ~13.1k
  ops/s；`wal_fsync_latency_seconds_count` 显示运行期 fsync 数略少于写入数
  （~1.5 写/fsync）。合并比随 **写并发度** 与 **fsync 时延** 增大而升高：本机 fsync 很快
  且有效并发受 worker 数（4 vCPU）限制，故 e2e 合并比温和；criterion（§1）在提交点
  完全对齐时给出 30× 的上界。

## 3. MR-0018/0020 "引擎调用同步内联" 决策再复核（I-08 议题，维持）

MR-0020 约定："I-08 WAL 落盘（fsync）进入前台写路径时按新形态重新评估"。现按数据复核：

- **读路径无理由外移。** GET/HGET 不受影响；把每条命令（含读）丢进 `spawn_blocking`
  会给 **每条** 命令加一次任务移交，净负收益。
- **写路径的瓶颈是 fsync 本身，不是"占用 worker"。** `spawn_blocking` 不减少 fsync 次数，
  也不改变单条写的 fsync 时延；真正降低 fsync 次数的是 **组提交**（已实现，并发写者共享
  一次 fsync）。多线程 runtime 下一个 worker 停在 fsync 时，其它连接在别的 worker 继续推进。
- **结论：维持同步内联。** 触发"改造为专用每-Core 写线程 / 请求批处理 actor"的条件：
  单 Core 的写并发度超过 runtime worker 数，或生产环境 fsync 时延占主导，或需要把
  **单连接 pipeline 内的多写** 也合并为一次 fsync（当前实现不覆盖这一场景，是最可能的
  下一步优化）。此触发条件随 I-09/I-11 的副本转发与 Quorum 写一并重估（届时写路径本就
  要再动）。
