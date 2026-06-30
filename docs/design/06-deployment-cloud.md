# 06 · 公有云与 Kubernetes 部署

> 本文件说明 GameStore 如何在主流公有云上以云原生方式部署、跨可用区高可用、并自动化运维。
>
> **路线说明：** 组件命名对齐无主多写架构（MetaServer/RootServer/DataNode），详见 [`02-architecture.md`](02-architecture.md)、[`../EVOLUTION.md`](../EVOLUTION.md)。

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
│         ┌────────────┬────────────┴──────┬──────────────┐   │
│         ▼            ▼                    ▼              ▼   │
│  ┌────────────┐ ┌────────────┐ ┌────────────────┐ ┌────────┐│
│  │ Proxy      │ │ DataNode   │ │ MetaServer     │ │ Root   ││
│  │ Deployment │ │ StatefulSet│ │ StatefulSet    │ │ Server ││
│  │ (无状态,HPA)│ │ +云盘(每盘  │ │ (元信息+QoS+   │ │ (跨集群)││
│  │ +Service/LB│ │ 一 DataNode)│ │ 故障检测/修复)  │ │        ││
│  └────────────┘ └────────────┘ └────────────────┘ └────────┘│
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
    replicas: 6                  # 无状态接入层(RESP2/Thrift)，可配 HPA 自动扩缩
    service:
      type: LoadBalancer         # 暴露给业务的入口
  dataNode:
    replicas: 9                  # 数据节点数（线上每块盘一个 DataNode；>=3，建议为 3 的倍数以便跨 AZ 均衡）
    resources:
      requests: { cpu: "8", memory: "32Gi" }
    volume:
      storageClass: "gp3"        # 云块存储（AWS gp3 / GCP pd-ssd / Azure managed-disk）
      size: "500Gi"
  partition:
    replicaCount: 3              # 每个 Partition 的副本数 N
    consistency:                 # 一致性旋钮（见 04-replication-consistency）
      mode: multi-leader         # multi-leader(默认,无主多写) | leader-followers(单主半同步)
      quorum: { w: 1, r: 1 }     # 默认 W=1,R=1(最终一致)；强一致可设 W=3,R=3 且 N=5(W+R>N)
  meta:
    replicas: 3                  # MetaServer 元信息中心（高可用）
  rootServer:
    enabled: true                # 多集群协调（可选）
  topology:
    spreadAcrossZones: true      # 跨 AZ 反亲和
    spreadAcrossPods: true       # 跨 POD 反亲和：同 Partition 副本不落同一 POD
  backup:
    schedule: "0 */6 * * *"      # 周期全量快照(引擎 Checkpoint)
    objectStore:
      provider: s3
      bucket: gamestore-prod-backup
    pointInTimeRecovery: true    # 开启 PITR（快照 + Operation/ReplicaLog 归档）
```

## 3. 工作负载映射

| GameStore 组件 | K8s 工作负载 | 关键配置 |
| --- | --- | --- |
| Proxy | Deployment + Service(LB) | 无状态、HPA 按 CPU/QPS 扩缩、就近接入 |
| DataNode | StatefulSet + PVC | 稳定网络标识与持久卷，每块盘一个 DataNode，挂云块存储或本地 NVMe |
| MetaServer | StatefulSet（3 副本） | 元信息 + 多租户 QoS + 故障检测/修复，独立小规格、强稳定 |
| RootServer | Deployment（可选） | 全集群视角，多集群协调与容灾 |
| Operator | Deployment | Reconcile 控制循环，监听 CR 与组件状态 |

## 4. 跨可用区高可用

- **拓扑分布约束（topologySpreadConstraints）/ 反亲和**：保证同一 Partition 的 N 个副本分别落在不同 AZ/POD 的不同节点。
- **AZ 级故障**：任一 AZ 整体不可用时，每个 Partition 仍保有存活副本——**无主架构下只要有副本即可读写**，无选主等待。
- **MetaServer 跨 AZ**：3 个 MetaServer 节点分散在 3 AZ，管控面在单 AZ 故障下仍可用；且其不在读写关键路径。

## 5. 存储介质选择

| 介质 | 优点 | 缺点 | 适用 |
| --- | --- | --- | --- |
| 云块存储（gp3/pd-ssd） | 数据随卷持久、节点漂移可重挂、运维简单 | 网络存储延迟略高 | 默认推荐，均衡之选 |
| 本地 NVMe SSD | 延迟最低、IOPS 最高 | 实例销毁数据即失，依赖多副本兜底 | 极致写性能场景 + 多副本 + 快速重建 |

> 因为 GameStore 自身已用 **无主多副本 + Quorum** 保证可靠性，本地 NVMe 的"易失"风险由副本与 MetaServer 自动重建覆盖，
> 是"性能 vs 重建成本"的取舍，可按集群定位选择。

## 6. 升级与运维

- **滚动升级**：无主架构下重启某 DataNode 前 **无需转移 Leader**——其上 Replica 暂不可用期间，
  同 Partition 的其他副本继续服务；逐节点滚动，写入自动落到存活副本。
- **自愈**：节点/Pod 故障由 K8s 重建 + MetaServer 副本重建（Anti-Entropy 追平）双重兜底（见 [`04-replication-consistency.md`](04-replication-consistency.md) §9）。
- **弹性扩缩**：调大 `dataNode.replicas` → Operator 加节点 → MetaServer 自动再均衡（见 [`05-sharding-routing.md`](05-sharding-routing.md) §3）。
- **GitOps 友好**：集群形态由 CR 声明，纳入版本管理与审计。

## 7. 多云可移植性

- 仅依赖通用云能力抽象：**块存储（PV/PVC）+ 对象存储（S3 兼容）+ 负载均衡（Service）**。
- 这些在 AWS / GCP / Azure / 阿里云均有等价物，避免被单一云厂商深度绑定。
