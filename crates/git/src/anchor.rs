//! Change-Id 稳定锚定（ADR-004）。
//!
//! Ingest 时 change-id 的确定完全沿用 jj-lib 的标准：
//!   1. 若 git commit 带 `change-id` extra header（jj 导出时写入），解析之；
//!   2. 否则用 jj 官方的 `synthetic_change_id_from_git_commit_id`（取 commit id
//!      后 16 字节并按位反转）确定性合成。
//!
//! 这保证 jj ↔ git 往返时 change-id 无损可识别，且与 jj 生态一致。

use jj_lib::backend::{ChangeId, CommitId};
use jj_lib::git_backend::{extract_change_id_from_commit, synthetic_change_id_from_git_commit_id};

/// Resolve the stable change-id for a git commit.
///
/// `message`-level trailer (Gerrit `Change-Id:`) is intentionally NOT used;
/// jj writes its change-id into the commit *header* (`change-id`), not the
/// message body.
pub fn resolve_change_id(commit: &gix::objs::CommitRef<'_>, id: &CommitId) -> ChangeId {
    extract_change_id_from_commit(commit).unwrap_or_else(|| synthetic_change_id_from_git_commit_id(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jj_lib::object_id::ObjectId as _;

    #[test]
    fn synthetic_id_is_stable() {
        let id1 = CommitId::from_hex("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        let id2 = CommitId::from_hex("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        let a = synthetic_change_id_from_git_commit_id(&id1);
        let b = synthetic_change_id_from_git_commit_id(&id2);
        assert_eq!(a, b);
        assert_eq!(a.as_bytes().len(), 16);
    }

    #[test]
    fn synthetic_id_differs_by_commit() {
        let id1 = CommitId::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let id2 = CommitId::from_hex("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_ne!(
            synthetic_change_id_from_git_commit_id(&id1),
            synthetic_change_id_from_git_commit_id(&id2)
        );
    }

    #[test]
    fn extract_change_id_from_header_is_preferred() {
        let change_id = ChangeId::new(vec![0x5a; 16]);
        let reverse = change_id.reverse_hex();
        let commit_id = CommitId::from_hex(
            "1111111111111111111111111111111111111111111111111111111111111111",
        );
        let body = format!(
            "tree {t}\nauthor A <a@x> 1700000000 +0000\ncommitter C <c@x> 1700000000 +0000\nchange-id {reverse}\n\nmessage\n",
            t = "0".repeat(40),
        );
        let cref =
            gix::objs::CommitRef::from_bytes(body.as_bytes(), gix::hash::Kind::Sha1).unwrap();
        assert_eq!(resolve_change_id(&cref, &commit_id), change_id);
    }
}