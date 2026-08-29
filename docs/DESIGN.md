# jj-lab — 纯原生 jj Server + Git 翻译网关

> 状态：规划中（v0.1）
> 定位：纯原生 jj 语义的托管服务端，通过「双向协议翻译与状态映射引擎」无缝拉取/推送标准 Git 远端。

---

## 1. 愿景

jj（Jujutsu）在本地提供 Change-centric、First-class 冲突、Operation Log 等 Git 无法企及的能力，但这些能力一旦推送到以 Git 为底的托管平台就被迫降级。jj-lab 做那个「底层完全懂 jj」的原生 Server：对内彻底摆脱 Git 历史包袱，对外仍能与 GitHub/GitLab 无缝同步。

---

## 2. 核心不变量

1. **存储与内部协议 = 纯 jj 语义**：change-id 中心、first-class conflict 为对象、op-log 为历史。
2. **git 只是对外网关，非对称**：Ingest（git → jj 升格）、Export（jj → git 有损打包）。
3. **元数据 SQLite（无外部进程），对象本地 FS，S3 仅附件/备份**。

---

## 3. 三层四模块

```
┌─────────────────────────────────────────────────────┐
│ 网络与传输层（Protocol Layer）                         │
│   对内 gRPC（后置） / 对外 Smart HTTP（Git 智能协议）  │
├─────────────────────────────────────────────────────┤
│ Git 翻译网关层（Translation Gateway: crates/git）      │
│   升格（git→jj）/ 降格打包（jj→git）/ change-id 锚定    │
├─────────────────────────────────────────────────────┤
│ 原生核心层（Native Engine: crates/core + jj-lib 真相源）│
│   Change ID / Op-log / First-class 冲突 / 对象库        │
└─────────────────────────────────────────────────────┘
```

---

## 4. Crate 划分（单 Rust workspace）

```
jj-lab/
  Cargo.toml                 # workspace
  crates/
    core/                    # SQLite schema、领域类型、id、错误
    proto/                   # protobuf 强类型（真相源）
    git/                     # jj-lib 真相源封装 + git 翻译网关
    server/                  # HTTP(REST+protobuf) 门面，gRPC 后置
  docs/
    adr/  schema.md  milestones.md
```

| crate | 职责 | 依赖 jj-lib | 状态 |
|---|---|---|---|
| `core` | 元数据、id、错误 | 否 | 规划 |
| `proto` | Change/Conflict/Operation/Bookmark 强类型 | 否 | 规划 |
| `git` | jj-lib 真相源 + 升格/降格/锚定 | 是 | 规划 |
| `server` | HTTP 门面 + op-log 持久化 | 间接（经 git） | 规划 |

---

## 5. 数据流

```
GitHub/GitLab ──fetch──▶ [翻译网关: Ingest 升格] ──▶ jj 语义存储（真相源）
                                ▲                        │
                                │ gix fetch             │ gRPC/HTTP
                                │                        ▼
                       [翻译网关: Export 降格] ◀── Web / Agent / jj 客户端
                                │
                                ▼
                          GitHub/GitLab
```

- 下行（客户端读写）：直接 jj 语义，不经 git 层。
- git 兼容门面：仅作翻译官，永远不是能力上限。

---

## 6. 双向翻译器

### Ingest（git → jj 升格）
1. gix fetch 拉取到隔离临时 git 存储。
2. 逐 commit 翻译成 jj change 写入原生存储。
3. Change-Id 稳定锚定（幂等）：trailer → 内容哈希派生 → 映射表。
4. 冲突格式（git conflict marker）实例化为 first-class conflict。

### Export（jj → git 降格）
1. jj 图结构展平，打包成 git packfile。
2. Change-Id 写入 commit message 尾部（或 git notes），保证再拉回可辨识。
3. first-class conflict 无法无损映射回 git，能导则导、不能导则标记。

---

## 7. Bookmarks ↔ Git 分支映射

- 维护双向映射表：`refs/heads/<name>` ⟺ 原生 bookmark `<name>`。
- 远端追踪书签：`<name>@origin`。
- `refs/pull/*`：先映射为 namespace bookmark（MR 语义后置于 M3）。

---

## 8. 异步 Mirror 调度

- Worker 守护进程：定时或 Webhook 触发。
- 流水线：远端变更 → 隔离 fetch → 翻译 → 写入原生存储。
- 冲突/非快进：不崩溃，生成分叉或原生冲突对象，Web UI 标记待解决。

---

## 9. 里程碑（详见 milestones.md）

- M1（第一阶段）：后端存储 = core + proto + git(锚定+Ingest) + server(HTTP 门面 + op-log 原生持久化)。
- M2：Git Export/Push（自研 send-pack）+ SSH（远期）。
- M3：MR/Review（change-centric）。
- M4：Operation Log 云端协同（SubscribeOps 流式）。
- M5：Release + Packages。
- M6：Actions（CI）。