# Milestones — 分阶段实施计划

> 每阶段给出目标、关键交付与验收标准。优先级自上而下递减。M1 是「原生存储 + Git 升格」第一个可见成果。

---

## M1 — 后端存储（core + proto + git 翻译网关 + server）

- **目标**：搭起单 Rust workspace，落地「原生 jj 存储 + Git 拉取升格 + change-id 锚定 + 原生冲突 + op-log 原生持久化 + HTTP 门面」。
- **交付**：
  - `crates/core`：SQLite schema（org/repo/change/bookmark/op_log/git_sha→change_id 映射表）、领域类型、id、错误。
  - `crates/proto`：Change/Conflict/Operation/Bookmark 强类型定义。
  - `crates/git`：jj-lib 真相源封装 + change-id 锚定三层优先级 + Ingest（gix fetch → 逐 commit 翻译 → 冲突升格）。
  - `crates/server`：HTTP 门面（简单读取 JSON、大载荷 protobuf），change-centric 读写 + 原生冲突读写 + op-log 原生持久化。
- **验收**：
  - `cargo build` + `cargo clippy`（零警告）通过。
  - 能创建 org/repo，从本地 git 仓库 fetch 并升格为 jj change；amend 后 change-id 不漂移。
  - 冲突内容实例化为 first-class conflict 对象并通过接口可读。
  - op-log 记录持久化，重启后可回溯。

> 不含 Web 前端、不含 Export/Push、不含 packages、不含 Actions。

---

## M2 — Git Export/Push 网关

- **目标**：打通 jj → git 反向，实现自研 send-pack。
- **交付**：`crates/git` 降格打包（图展平 → packfile）+ 基于 `gix-protocol` 的自研 push（ref 协商、packfile 上传、force/--mirror 语义）；Change-Id 回写 commit message。SSH 传输列远期。
- **验收**：能把原生 change 打包推送至外部 git 远端；再拉回 change-id 不变；冲突有损明确标注。

---

## M3 — MR/Review（Change-centric）

- **交付**：`mr_reviews`/`mr_comments` + review 接口。
- **验收**：force-push 后 review 状态/讨论不丢。

---

## M4 — Operation Log 云端协同

- **交付**：`SubscribeOps`/`UndoOperation` 流式（gRPC/tonic）+ 历史回滚。
- **验收**：云端 undo 同步给所有客户端。

---

## M5 — Release + Packages（后置）

- **交付**：`releases`/`release_assets`（S3）+ 各协议 adapter（独立 crate）。
- **验收**：release 附件上传下载；各协议 publish/pull 通过。

---

## M6 — Actions（CI，后置）

- **交付**：workflow YAML + runs/jobs/runners 编排，复用 zergx-worker。
- **验收**：事件触发 workflow 并产出日志。

---

## 远期（不排期）

- SSH 传输（Smart HTTP 之外）。
- jj-lib S3 后端 / artifact blob S3 后端。
- 自研 `jj-lab` CLI（消费 gRPC 对象流）。
- 高可用/多实例（SQLite → Postgres 演进）。