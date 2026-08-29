# Schema — SQLite 表设计

> 统一在 `crates/core` 管理，SQLite（rusqlite bundled），无外部进程。命名为 jj 语义优先，注释说明与 git 概念的映射。

---

## 身份与组织

### orgs
| 列 | 类型 | 说明 |
|---|---|---|
| id | TEXT PK | 组织 id |
| name | TEXT | 组织名 |
| created_at | TEXT | |

### repos
| 列 | 类型 | 说明 |
|---|---|---|
| id | TEXT PK | |
| org_id | TEXT FK→orgs | |
| name | TEXT | repo 名 |
| default_bookmark | TEXT | 默认 bookmark（非主键） |
| git_url | TEXT NULL | 上游 git 镜像地址（供网关） |
| created_at | TEXT | |

---

## 变更（Change-centric）

### changes（核心，change-id 为真相主键）
| 列 | 类型 | 说明 |
|---|---|---|
| change_id | TEXT PK | **Change-centric 主键** |
| repo_id | TEXT FK→repos | |
| description | TEXT | commit message |
| author / committer | TEXT | |
| created_at / updated_at | TEXT | |
| git_commit_sha | TEXT NULL | export 对应的 git sha（有损映射） |

### change_parents
| 列 | 说明 |
|---|---|
| change_id | FK→changes |
| parent_change_id | FK→changes |

> Change 父子即「变更血缘」，取代 git parent-commit。bookmark 只是指向某 change 的别名。

### bookmarks
| 列 | 说明 |
|---|---|
| repo_id | |
| name | 如 `main` / `feature/a` / `main@origin` |
| change_id | 指向当前 change |
| is_remote | 是否远端追踪书签 |

---

## 映射（Change-Id 锚定）

### change_id_map
| 列 | 说明 |
|---|---|
| repo_id | |
| git_commit_sha | 原始 git commit sha |
| change_id | 合成/关联的 change-id |
| PRIMARY KEY (repo_id, git_commit_sha) | |

> 防 force-push/amend 漂移，见 ADR-004。

---

## 冲突

### conflicts
| 列 | 说明 |
|---|---|
| id | conflict id |
| repo_id | |
| change_id | 所在 change |
| path | 冲突文件路径 |
| adds | JSON（正项 term 列表） |
| removes | JSON（负项 term 列表） |

> first-class conflict，结构化保留多方，不下沉 git marker。

---

## Operation Log

### op_log（云端 undo 的凭据）
| 列 | 说明 |
|---|---|
| id | op id |
| repo_id | |
| op_type | rebase/commit/resolve/amend/... |
| payload | JSON（操作还原数据） |
| undo_of | 反向操作链 |
| created_at | |

---

## 远期（不在 M1）

- `merge_requests` / `mr_reviews` / `mr_comments`（M3）
- `issues` / `labels` / `milestones`
- `releases` / `release_assets`（M5）
- `workflows` / `runs` / `jobs` / `runners`（M6）