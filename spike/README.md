# GameStore Phase-1 Spike · Rust vs C++

> **目的：** 为"语言选型（Rust vs C++）"提供事实依据。本目录用 **两种语言各实现一遍**
> Phase-1 MVP 的最小垂直切片（见 [`../docs/design/09-roadmap.md`](../docs/design/09-roadmap.md)），
> 并用 **同一套基于标准 Redis 客户端的功能测试** 验证两者行为一致。
>
> 这是一个 **spike（探针/对比原型）**，不是生产代码：目标是"用最少代码暴露两门语言在本项目
> 关键维度上的差异"，而不是功能完整。

## 这两个实现都做了什么

完全对齐 `docs/design/03-storage-engine.md` 的通用引擎层设计：

1. **RESP2 服务端**：标准 Redis 客户端（`redis-cli` / `redis-py`）零改造直连。
2. **RocksDB 作为通用引擎层**：
   - Rust 走 `rust-rocksdb`（TiKV 同款绑定，bundled RocksDB 8.10）——即 FFI 路径。
   - C++ 直接链接系统 `librocksdb`（8.9）——即原生零 FFI 路径。
3. **"元数据键 + 子键 + 版本号"编码**（§2）：String 值放在 metadata 记录里；
   Hash 的每个字段是一条 subkey 记录，key 内嵌 owner key 与结构 `version`。
4. **O(1) 逻辑删除 + 后台 GC**（§4）：`DEL`/重建只递增 `version`；旧 version 的子键由
   **RocksDB Compaction Filter** 在后台物理回收。两种语言用同一套机制：一个内存中的
   `user_key -> 当前 version` 映射（即文档说的 metadata/version 缓存），compaction filter
   据此判断"保留当前 version 子键、丢弃旧 version / 孤儿子键"。

> **两边的磁盘编码逐字节一致**（见 `encoding.rs` 与 `encoding.h`），因此同一个数据目录
> 理论上可被两个实现互读——这本身也验证了"编码层与语言无关"。

## 支持的命令

`PING ECHO SET GET DEL EXISTS TYPE EXPIRE PEXPIRE TTL PTTL HSET HMSET HGET HMGET HGETALL HDEL HLEN HEXISTS FLUSHDB`

外加 spike 专用的内省命令（供测试验证 GC 用，非 Redis 标准）：

| 命令 | 作用 |
| --- | --- |
| `RAWCOUNT` | 当前物理存在的 subkey 记录条数（证明 compaction filter 回收了孤儿子键） |
| `DBSIZE` | 逻辑 key（metadata 记录）条数 |
| `COMPACT` | flush + 强制 bottommost compaction，让 compaction filter 同步运行 |

## 目录结构

```
spike/
├── rust/                      # Rust 实现
│   ├── Cargo.toml
│   ├── .cargo/config.toml     # 本镜像上把 CC/CXX/linker 钉到 g++（见下"环境说明"）
│   └── src/{main,resp,commands,storage,encoding,gc}.rs
├── cpp/                       # C++ 实现
│   ├── CMakeLists.txt
│   └── src/{main,resp,commands,storage}.{h,cpp} + {encoding,gc}.h
└── test/
    ├── redis_functional_test.py   # 共享功能测试（用 redis-py）
    └── run_all.sh                 # 一键：编译两者 + 各起服务 + 跑同一套测试
```

两种语言的源码刻意按 **同名模块/同样职责** 组织，方便逐文件对照阅读：
`encoding`（编码）、`gc`（版本表 + compaction filter）、`storage`（数据模型→RocksDB）、
`resp`（RESP2 协议）、`commands`（命令分发）、`main`（TCP 服务）。

## 如何运行

一键编译 + 起服务 + 跑两套测试：

```bash
cd spike
bash test/run_all.sh
```

预期结尾输出：`ALL TESTS PASSED (rust + cpp)`（两边各 32 项断言全过）。

单独构建/运行：

```bash
# Rust
cd spike/rust && cargo build --release
./target/release/gamestore-spike --port 6390 --db /tmp/gs-rust

# C++
cd spike/cpp && cmake -S . -B build -DCMAKE_CXX_COMPILER=g++ && cmake --build build -j
./build/gamestore_spike --port 6391 --db /tmp/gs-cpp

# 用标准 Redis 客户端连
redis-cli -p 6390 hset player:{1001} gold 100 level 5
redis-cli -p 6390 hgetall player:{1001}

# 跑共享功能测试（需要 python3 + `pip install redis`）
python3 spike/test/redis_functional_test.py --port 6390 --label rust
```

## 环境说明（重要）

本镜像默认的 `cc` / `c++` alternative 指向 **clang**，而该 clang 找不到可用的
libstdc++（头文件与 `libstdc++.so` 链接路径）。因此：

- **C++**：用 `-DCMAKE_CXX_COMPILER=g++` 配置（`CMakeLists.txt` 也会兜底找 g++）。
- **Rust**：`rust/.cargo/config.toml` 把 `CC=gcc`、`CXX=g++`、`linker=g++` 钉死
  （因为链接 RocksDB 这个 C++ 库需要 libstdc++）。

依赖（已在本环境安装）：`librocksdb-dev libclang-dev redis-tools` + 压缩库
（`libsnappy/lz4/zstd/bz2/gflags-dev`），以及 `python3-redis`。

## 这个 spike 想让你"亲手感受"的差异点

对照 [`../docs/design/`](../docs/design/) 的语言选型讨论，重点看：

1. **RocksDB 集成成本**：C++ 直接 `#include <rocksdb/...>` 原生调用；Rust 经 `rust-rocksdb`
   绑定。两者都顺利支持本项目最关键的 **Compaction Filter**（TiKV 的 MVCC GC 也靠它）。
2. **Compaction Filter 的写法**：对照 `cpp/src/gc.h`（继承 `rocksdb::CompactionFilter`）
   与 `rust/src/gc.rs` + `storage.rs` 里的 `set_compaction_filter` 闭包。
3. **并发与内存安全**：两实现都是"线程/连接 + 共享 RocksDB + 共享 version 映射"。
   C++ 用裸 `DB*` + `std::mutex`，靠人保证不悬垂/不竞争；Rust 用 `Arc<DB>` + `RwLock`，
   `Send/Sync` 由编译器强制。建议你可以试着 **故意制造一处数据竞争**，看两边分别在
   "编译期 / 运行期(需 TSan)" 暴露问题——这正是选型 plan 里"安全网"那一点的体感来源。
4. **构建体验**：`cargo build`（一条命令、依赖自动拉取，首次需编译 bundled RocksDB 约 3 分钟）
   vs `cmake + 链接系统库`（秒级，但依赖需自行安装到系统）。

> 结论性建议仍见设计讨论：默认 Rust，C++ 在"已有资深 C++ 存储工程师/要直接移植 Kvrocks 源码"
> 时更优。本 spike 只提供可亲手运行、可对照阅读的事实材料。
