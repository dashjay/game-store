# 08 · 可观测性与运维

> 本文件定义 GameStore 的指标、日志、追踪、告警与 SLO。设计原则：**没有指标的功能视为未完成**。
>
> **路线说明：** 指标对齐无主多写（以 Quorum/Anti-Entropy/慢节点为核心，取代 Raft 提交指标），详见 [`../EVOLUTION.md`](../EVOLUTION.md)。

## 1. 指标（Metrics，Prometheus 风格）

### 1.1 接入层（Proxy）
- `proxy_qps{cmd, type}`：按命令/读写类型的 QPS。
- `proxy_latency_seconds{cmd, quantile}`：端到端延迟分位（p50/p95/p99/p999）。
- `proxy_backup_request_total`：触发 Backup Request 的次数（慢副本健康度信号）。
- `proxy_conn_active` / `proxy_conn_pool`：连接数与连接池使用。
- `proxy_hotkey_total`：识别到的热点 Key 命中。

### 1.2 DataNode / Replica
- `write_quorum_latency_seconds{quantile, w}`：达到 W 个副本落盘的写延迟（写性能核心指标）。
- `quorum_not_met_total`：超时未达到 W 的写失败计数（可用性/可靠性信号）。
- `replica_count_per_core` / `core_count`：每 Core 的 Replica 数与 Core 数（均衡度信号）。
- `anti_entropy_lag_seconds{partition}`：副本间一致性收敛滞后（越小越好，正常应秒级）。
- `replicalog_backlog`：未达成一致的 ReplicaLog 积压量（内存占用与分区风险信号）。
- `conflict_resolved_total{type=lww|crdt}`：冲突解决次数（多写冲突强度观察）。
- `wal_fsync_latency_seconds` / `wal_gc_pending`：WAL 落盘延迟与待 GC 量。
- `rocksdb_write_stall_seconds` / `rocksdb_block_cache_hit_ratio`：写停顿与 Block Cache 命中率。
- `disk_used_bytes` / `disk_iops`：磁盘容量与 IOPS。
- 多租户：`tenant_nrc{tenant}`、`tenant_quota_rejected_total{tenant}`、`wfq_queue_delay_seconds{tenant}`。

### 1.3 MetaServer / 调度
- `meta_nodes{state=up|down}`：DataNode 健康分布。
- `under_replicated_partitions`：副本数不足的 Partition 数（可靠性红线指标）。
- `scheduling_operators_total{type=rebuild-replica|rebalance}`：调度动作计数。
- `partition_migration_progress`：分片迁移/重建进度。

## 2. 日志

- **结构化日志**（JSON），分级（error/warn/info/debug），统一采集到日志平台。
- **慢日志（Redis 风格）**：记录超过阈值的命令（命令、Key、耗时、来源），用于定位慢查询与大 Key。
- **审计日志**：管理类操作（扩缩容、副本重建、备份恢复）留痕，便于追责与回溯。

## 3. 分布式追踪

- 关键路径（Proxy → Replica Coordinator → 并发副本写 → Quorum 确认 → 暂存层/引擎）注入 trace，
  采用 OpenTelemetry 语义，定位跨组件延迟瓶颈与慢副本。

## 4. 告警（建议规则）

| 告警 | 触发条件（示例） | 严重级 |
| --- | --- | --- |
| 副本不足 | `under_replicated_partitions > 0` 持续 N 分钟 | 高（可靠性风险） |
| Quorum 写失败 | `quorum_not_met_total` 上升 | 高（可用性/可靠性受损） |
| 一致性收敛滞后 | `anti_entropy_lag_seconds` 超阈值（远超秒级） | 高（最终一致受损/疑似分区） |
| 慢副本 | `proxy_backup_request_total` 激增 | 中（定位慢节点） |
| 写停顿 | `rocksdb_write_stall_seconds` 升高 | 高（高频写受损） |
| 节点失联 | `meta_nodes{state=down} > 0` | 高 |
| 磁盘将满 | `disk_used_bytes / capacity > 0.8` | 中 |
| 备份失败 | 周期备份任务失败 | 高 |

## 5. SLO（建议初始目标，需结合压测校准）

| SLO | 初始目标 |
| --- | --- |
| 可用性（整集群） | ≥ 99.95%（高优独立部署集群更高） |
| 写 p99 延迟 | 个位数毫秒级（达到 W 个副本 WAL 落盘） |
| 读 p99 延迟 | 毫秒级（就近副本 + Backup Request） |
| 最终一致收敛 | 正常网络下秒级 |
| RPO | 趋近 0（PITR） |
| 数据持久性 | 不丢已确认写（W 个副本 WAL 落盘保证） |

## 6. 运维操作面

- **管理命令 / 控制台**：查看拓扑、Partition/Replica 分布、负载均衡、发起副本重建/再均衡、触发备份恢复。
- **灰度与回退**：任何变更（升级、参数、一致性旋钮、切流）都要能灰度并快速回退。
- **容量规划**：基于 [`01-workload-data-model.md`](01-workload-data-model.md) 的负载模型 + 实时指标做趋势预测与提前扩容。
- **演练**：故障注入（杀副本、断 AZ、注入慢节点）、备份恢复演练定期执行，验证自愈与 RTO/RPO。
