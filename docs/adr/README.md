# ADR — jj-lab 架构决策记录

> 每一条记录一个关键决策、理由与影响。顺序大致对应决策的依赖关系。

---

## ADR-001：纯原生 jj Server + 非对称 Git 翻译网关

- **决策**：内部存储与协议完全使用 jj 语义（change-id 中心、first-class 冲突、op-log 历史）；Git 仅作为对外网关，且非对称——Ingest（git → jj 升格）与 Export（jj → git 有损打包）。
- **理由**：这正是「原生 jj Server」的本义。Git 兼容不是能力上限，而是与世界互操作的翻译官。
- **影响**：
  - first-class 冲突是单向优势：进 jj 无损，回 git 有损，需显式标注。
  - 需要「升格」与「降格」两个方向的翻译器。

---

## ADR-002：真相源使用 jj-lib，Git 互操作复用 jj 官方 fetch/push（git 子进程）

> **已修订**（见 ADR-009）。原「自研 send-pack」方案废止。

- **决策**：jj-lib（.44）作为真相源（change/tree/op-log/对象库）；拉取/推送复用 jj 官方路径 —— `clone/init_external_git + import_refs`（拉）与 `export_refs + git push`（推）。
- **理由**：gix 0.85 无 push 实现，jj 官方亦无纯 Rust send-pack —— `jj git push` 内部就是 `Command::new(git) push`（`git_subprocess.rs::spawn_push`）。自研 send-pack 是与整个生态（jj / Gitea / Forgejo）对抗的负收益复杂度。
- **影响**：
  - 网络传输（clone/fetch/push）shell 到 `git` 二进制。
  - 对象/change/tree/conflict 语义一律留在 jj-lib 内，不强类型外泄。

---

## ADR-003：双向同步 = 全 jj 官方 import/export，不自建翻译层

> **已修订**（见 ADR-009）。原「拒绝 import_refs」的理由不成立。

- **决策**：拉取用 `import_refs`（ref 级 import），推送用 `export_refs`；不逐 commit 自建「git ↔ jj change」翻译层。
- **理由**：change-id 在 jj 里靠 git commit 的 `change-id` extra header 无损往返（`git_backend.rs::extract_change_id_from_commit`），不存在「ref 级映射丢 change-id」的问题。唯一有损的是 first-class **多路冲突**对非 jj git 客户端的可读性 —— 那是 git 对象模型的物理上限，jj 自身 round-trip 无损，且与「自研 vs 官方」无关。
- **影响**：
  - change-id 锚定直接复用 jj 的 header + `synthetic_change_id_from_git_commit_id`。
  - 外部 git 客户端只能看到冲突的 diff3 materialization，多路语义仅 jj 可见。

---

## ADR-004：Change-Id 稳定锚定（复用 jj 官方机制）

> **已修订**：不再自研 trailer 优先级，直接对齐 jj 官方。

- **决策**：change-id 的确定完全沿用 jj-lib 标准：
  1. 读 git commit 的 `change-id` extra header（jj export 时写入）；
  2. 无 header 时用 jj 官方 `synthetic_change_id_from_git_commit_id`（commit id 末 16 字节按位反转）确定性合成。
- **理由**：保证 jj ↔ git 往返 change-id 无损，且与 jj 生态一致。
- **影响**：无需自管理 `git_sha → change_id` 映射表；锚定逻辑即 `resolve_change_id`。

---

## ADR-005：存储 = SQLite 元数据 + 本地 FS 对象 + S3 仅附件/备份

- **决策**：元数据用 SQLite（rusqlite bundled，无外部进程）；change/repo 对象用本地文件系统（jj-lib Backend 仅本地）；S3/MinIO 只承载 release 附件与备份归档。
- **理由**：对齐 GitLab/Forgejo 成熟共识——对象存储延迟高，不适合大量随机读写。
- **影响**：单机可跑，无外部 DB；横向扩展留待远期。

---

## ADR-006：传输层 = proto 强类型真相源 + HTTP 先落地，gRPC 后置

- **决策**：领域层用 protobuf 定义强类型（Change/Conflict/Operation/Bookmark）作为单一真相源；`server` 先暴露 HTTP（简单读取走 JSON、大载荷走 protobuf body），满足浏览器渲染冲突与 change 浏览。tonic gRPC 服务层在 CLI / `SubscribeOps` 全双工流阶段再叠加同一套 proto。
- **理由**：浏览器无法直接发原生 gRPC；M1 的 Web 需要立即通过 HTTP 消费。而 proto 已是真相源，后续加 gRPC 层近乎零返工。
- **影响**：`proto` crate 先行；`op_log` 持久化第一阶段即原生，但推送订阅接口后续再接流式。

---

## ADR-007：架构 = 三层四模块

- **决策**：面对 Git 远端划分为三层：
  - **原生核心层**（Native Engine）：jj-lib，处理 Change ID / Op-log / First-class 冲突 / 对象库。
  - **Git 翻译网关层**（Translation Gateway）：git 数据升格为 jj 对象、jj 对象降格打包为 git。
  - **网络与传输层**（Protocol Layer）：对内 gRPC（后置），对外 Git 智能协议（Smart HTTP）。
- **理由**：职责清晰，翻译层可独立测试。
- **影响**：`git`（翻译网关）与 `server`（协议层）严格隔离。

---

## ADR-008：同步走 jj 官方 fetch/push（git 子进程），不自研协议

> **已修订**（见 ADR-009）。原「运行时零 git 二进制」废止。

- **决策**：clone/fetch/push 的**网络传输层** shell 到 `git` 二进制，与 jj 官方、Gitea、Forgejo 完全一致；对象/change/tree/conflict 语义留在 jj-lib 内。不自研 send-pack/fetch 协议。
- **理由**：连 Jujutsu 官方自己都不用纯 Rust 协议 —— `jj git fetch/push` 内部就是 `git fetch/push` 子进程。自研协议是与整个生态对抗的纯负收益。
- **影响**：
  - `crates/git/src/sync.rs`：`clone_remote` / `fetch_remote`（GitFetch）/ `push_mirror`（export_refs + git push --mirror）。
  - `git` 二进制的 `executable-path` 由 `git.executable-path`（默认 `git`）决定，测试/运行时同源。

---

## ADR-009：放弃自研协议，全面复用 jj 官方（取代 ADR-002/003/008 原案）

- **决策**：jj-lab 的 Git 双向同步**不自研任何协议**。拉取 = jj 官方 `import_refs`，推送 = jj 官方 `export_refs` + `git push`；change-id 复用 jj 官方 header 机制。定位为「替代 git 托管」，需要**双向同步**（既能 push，也能增量 fetch）。
- **理由**：自研 send-pack 是最大单点风险且反生态；change-id 并无「ref 级导入会丢」的问题（靠 `change-id` header 无损往返）；唯一有损的 first-class 多路冲突对外部 git 客户端不可读，是 git 模型的物理上限。
- **影响**：
  - 撤销 ADR-002/003/008 的「自研 send-pack / 拒绝 import_refs / 零 git 二进制」。
  - `crates/git` 增加 `sync.rs`；纯 gix 的 `ingest.rs` 保留作参考，但主路径改为官方 fetch/import。