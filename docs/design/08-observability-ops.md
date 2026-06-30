# 08 · 可观测性与运维

> 本文件定义 GameStore 的指标、日志、追踪、告警与 SLO。设计原则：**没有指标的功能视为未完成**。

## 1. 指标（Metrics，Prometheus 风格）

### 1.1 接入层（Proxy）
- `proxy_qps{cmd, type}`：按命令/读写类型的 QPS。
- `proxy_latency_seconds{cmd, quantile}`：端到端延迟分位（p50/p95/p99/p999）。
- `proxy_redirect_total{kind=moved|ask}`：路由重定向次数（迁移/选主健康度信号）。
- `proxy_conn_active` / `proxy_conn_pool`：连接数与连接池使用。
- `proxy_hotkey_total`：识别到的热点 Key 命中。

### 1.2 存储节点
- `store_raft_commit_latency_seconds{quantile}`：Raft 提交延迟（写性能核心指标）。
- `store_raft_apply_lag`：commit 与 apply 之间的滞后。
- `store_leader_count` / `store_region_count`：本节点 Leader 数与分片数（均衡度信号）。
- `store_rocksdb_compaction_pending_bytes`：待 Compaction 字节数（写放大/积压预警）。
- `store_rocksdb_write_stall_seconds`：写停顿时长（高频写场景重点监控）。
- `store_rocksdb_block_cache_hit_ratio`：Block Cache 命中率（内存是否够用）。
- `store_disk_used_bytes` / `store_disk_iops`：磁盘容量与 IOPS。

### 1.3 PD / 调度
- `pd_cluster_nodes{state=up|down}`：节点健康分布。
- `pd_underreplicated_regions`：副本数不足的分片数（可靠性红线指标）。
- `pd_scheduling_operators_total{type=transfer-leader|move-replica|split}`：调度动作计数。
- `pd_slot_migration_progress`：槽位迁移进度。

## 2. 日志

- **结构化日志**（JSON），分级（error/warn/info/debug），统一采集到日志平台。
- **慢日志（Redis 风格）**：记录超过阈值的命令（命令、Key、耗时、来源），用于定位慢查询与大 Key。
- **审计日志**：管理类操作（扩缩容、成员变更、备份恢复）留痕，便于追责与回溯。

## 3. 分布式追踪

- 关键路径（Proxy → 分片 Leader → Raft 提交 → 状态机 apply）注入 trace，
  采用 OpenTelemetry 语义，定位跨组件延迟瓶颈。

## 4. 告警（建议规则）

| 告警 | 触发条件（示例） | 严重级 |
| --- | --- | --- |
| 副本不足 | `pd_underreplicated_regions > 0` 持续 N 分钟 | 高（可靠性风险） |
| 写停顿 | `store_rocksdb_write_stall_seconds` 升高 | 高（高频写受损） |
| 提交延迟劣化 | `store_raft_commit_latency p99` 超阈值 | 高 |
| 节点失联 | `pd_cluster_nodes{state=down} > 0` | 高 |
| 磁盘将满 | `store_disk_used_bytes / capacity > 0.8` | 中 |
| 缓存命中下降 | `block_cache_hit_ratio` 持续走低 | 中（成本/延迟信号） |
| 备份失败 | 周期备份任务失败 | 高 |

## 5. SLO（建议初始目标，需结合压测校准）

| SLO | 初始目标 |
| --- | --- |
| 可用性（整集群） | ≥ 99.95% |
| 写 p99 延迟 | 个位数毫秒级（多数派落盘） |
| 读 p99 延迟（线性一致读） | 毫秒级 |
| RPO | 趋近 0（PITR） |
| 数据持久性 | 不丢已确认写（多数派落盘保证） |

## 6. 运维操作面

- **管理命令 / 控制台**：查看拓扑、分片分布、Leader 均衡、发起 split/transfer、触发备份恢复。
- **灰度与回退**：任何变更（升级、参数、切流）都要能灰度并快速回退。
- **容量规划**：基于 [`01-workload-data-model.md`](01-workload-data-model.md) 的负载模型 + 实时指标做趋势预测与提前扩容。
- **演练**：故障注入（杀 Leader、断 AZ）、备份恢复演练定期执行，验证自愈与 RTO/RPO。
