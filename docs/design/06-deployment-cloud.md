# 06 · 公有云与 Kubernetes 部署

> 本文件说明 GameStore 如何在主流公有云上以云原生方式部署、跨可用区高可用、并自动化运维。

## 1. 部署形态总览

GameStore 以 **Kubernetes Operator** 为核心交付方式，把"集群拓扑、扩缩容、故障自愈、备份"
沉淀为声明式 API，做到 **开箱即用**：

```
┌────────────────────────────────────────────────────────────┐
│                     Kubernetes 集群（跨 3 AZ）                │
│                                                              │
│   ┌──────────────┐   声明式 CR    ┌──────────────────────┐   │
│   │ 运维 / GitOps │ ────────────▶ │ GameStore Operator   │   │
│   └──────────────┘   (apply CRD)  │ (Reconcile 控制循环)  │   │
│                                   └──────────┬───────────┘   │
│              ┌─────────────────┬─────────────┴──────────┐    │
│              ▼                 ▼                         ▼    │
│   ┌──────────────────┐ ┌──────────────────┐ ┌─────────────┐ │
│   │ Proxy Deployment │ │ Storage          │ │ PD          │ │
│   │ (无状态, HPA)     │ │ StatefulSet      │ │ StatefulSet │ │
│   │ + Service/LB     │ │ + 云盘 PVC        │ │ (3 副本)     │ │
│   └──────────────────┘ └──────────────────┘ └─────────────┘ │
└───────────────────────────────────┬──────────────────────────┘
                                     │
                                     ▼
                        对象存储（S3 / GCS / OSS）
                          备份 · 快照 · PITR 日志
```

## 2. 声明式 API（CRD 草案）

引入自定义资源 `GameStoreCluster`，用户只描述"想要什么"，Operator 负责达成：

```yaml
apiVersion: gamestore.io/v1alpha1
kind: GameStoreCluster
metadata:
  name: prod
spec:
  redisCompatVersion: "7"        # 目标兼容的 Redis 版本特性集
  proxy:
    replicas: 6                  # 无状态接入层，可配 HPA 自动扩缩
    service:
      type: LoadBalancer         # 暴露给业务的入口
  storage:
    replicas: 9                  # 存储节点数（>=3，建议为 3 的倍数以便跨 AZ 均衡）
    replicationFactor: 3         # 每分片副本数
    resources:
      requests: { cpu: "8", memory: "32Gi" }
    volume:
      storageClass: "gp3"        # 云块存储（AWS gp3 / GCP pd-ssd / Azure managed-disk）
      size: "500Gi"
  pd:
    replicas: 3                  # 元数据中心，固定 3 节点 Raft
  topology:
    spreadAcrossZones: true      # 跨 AZ 反亲和，保证每分片副本分散到不同 AZ
  backup:
    schedule: "0 */6 * * *"      # 周期全量快照
    objectStore:
      provider: s3
      bucket: gamestore-prod-backup
    pointInTimeRecovery: true    # 开启 PITR（快照 + 日志）
```

## 3. 工作负载映射

| GameStore 组件 | K8s 工作负载 | 关键配置 |
| --- | --- | --- |
| Proxy | Deployment + Service(LB) | 无状态、HPA 按 CPU/QPS 扩缩、就近接入 |
| Storage Node | StatefulSet + PVC | 稳定网络标识与持久卷，挂云块存储或本地 NVMe |
| PD | StatefulSet（3 副本） | 元数据 Raft，独立小规格、强稳定 |
| Operator | Deployment | Reconcile 控制循环，监听 CR 与组件状态 |

## 4. 跨可用区高可用

- **拓扑分布约束（topologySpreadConstraints）/ 反亲和**：保证同一分片的 3 个副本分别落在 3 个 AZ 的不同节点。
- **AZ 级故障**：任一 AZ 整体不可用时，每个分片仍保有 2/3 副本（多数派），整体继续可读可写。
- **PD 跨 AZ**：3 个 PD 节点分散在 3 AZ，元数据服务在单 AZ 故障下仍可用。

## 5. 存储介质选择

| 介质 | 优点 | 缺点 | 适用 |
| --- | --- | --- | --- |
| 云块存储（gp3/pd-ssd） | 数据随卷持久、节点漂移可重挂、运维简单 | 网络存储延迟略高 | 默认推荐，均衡之选 |
| 本地 NVMe SSD | 延迟最低、IOPS 最高 | 实例销毁数据即失，依赖多副本兜底 | 极致写性能场景 + 强多副本 + 快速重建 |

> 因为 GameStore 自身已用 Raft 多副本保证可靠性，本地 NVMe 的"易失"风险由副本与 PD 自动重建覆盖，
> 是"性能 vs 重建成本"的取舍，可按集群定位选择。

## 6. 升级与运维

- **滚动升级**：升级某存储节点前，PD 先把其上分片的 **Leader 迁走**，再重启，最小化写中断；逐节点滚动。
- **自愈**：节点/Pod 故障由 K8s 重建 + PD 副本补齐双重兜底（见 [`04-replication-consistency.md`](04-replication-consistency.md) §4.2）。
- **弹性扩缩**：调大 `storage.replicas` → Operator 加节点 → PD 自动再均衡（见 [`05-sharding-routing.md`](05-sharding-routing.md) §3）。
- **GitOps 友好**：集群形态由 CR 声明，纳入版本管理与审计。

## 7. 多云可移植性

- 仅依赖通用云能力抽象：**块存储（PV/PVC）+ 对象存储（S3 兼容）+ 负载均衡（Service）**。
- 这些在 AWS / GCP / Azure / 阿里云均有等价物，避免被单一云厂商深度绑定。
