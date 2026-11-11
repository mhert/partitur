//! Parsers for the machine-readable output of the git commands the port runs.
//!
//! The parsers are pure and forgiving: a line they do not recognise is skipped rather than
//! failing the whole answer, so a newer git that adds attributes keeps working.

use std::path::PathBuf;

use partitur_core::{Head, LocalBranch, WorktreeInfo};

/// The worktrees in the output of `git worktree list --porcelain -z`, main checkout first.
///
/// Every attribute ends with a NUL and every record with one more. A record names its path
/// with `worktree`, its commit with `HEAD` and its branch with `branch refs/heads/<name>`; a
/// record without a branch is detached. The bare repository's record, which has no commit, is
/// skipped. `prunable` marks a worktree git would prune; `locked` and `detached` add nothing.
pub(crate) fn parse_worktrees(z: &str) -> Vec<WorktreeInfo> {
    z.split("\0\0").filter_map(parse_worktree).collect()
}

fn parse_worktree(record: &str) -> Option<WorktreeInfo> {
    let mut path = None;
    let mut commit = None;
    let mut branch = None;
    let mut prunable = false;
    for attribute in record.split('\0') {
        let (key, value) = attribute.split_once(' ').unwrap_or((attribute, ""));
        match key {
            "worktree" => path = Some(PathBuf::from(value)),
            "HEAD" => commit = Some(value),
            "branch" => branch = Some(value.strip_prefix("refs/heads/").unwrap_or(value)),
            "prunable" => prunable = true,
            _ => {}
        }
    }
    let head = match (branch, commit) {
        (Some(branch), _) => Head::Branch(branch.to_owned()),
        (None, Some(commit)) => Head::Detached(commit.to_owned()),
        (None, None) => return None,
    };
    Some(WorktreeInfo {
        path: path?,
        head,
        prunable,
    })
}

/// The local branches in the output of
/// `git for-each-ref --format=%(refname)%00%(upstream:remotename) refs/heads`: one line per
/// branch, its full ref name, a NUL and the remote of its upstream, empty when it has none.
pub(crate) fn parse_branches(z: &str) -> Vec<LocalBranch> {
    z.lines()
        .filter_map(|line| {
            let (refname, remote) = line.split_once('\0')?;
            Some(LocalBranch {
                name: refname.strip_prefix("refs/heads/")?.to_owned(),
                upstream_remote: (!remote.is_empty()).then(|| remote.to_owned()),
            })
        })
        .collect()
}

/// The remote-tracking branches, such as `origin/main`, in the output of
/// `git for-each-ref --format=%(refname) refs/remotes`, without the `<remote>/HEAD` that only
/// points at another of them.
pub(crate) fn parse_remote_branches(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.strip_prefix("refs/remotes/"))
        .filter(|name| {
            name.split_once('/')
                .is_some_and(|(_, branch)| branch != "HEAD")
        })
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree(path: &str, head: Head, prunable: bool) -> WorktreeInfo {
        WorktreeInfo {
            path: PathBuf::from(path),
            head,
            prunable,
        }
    }

    #[test]
    fn worktrees_name_their_branch_or_detached_commit_and_skip_the_bare_repository() {
        let z = concat!(
            "worktree /repo.git\0bare\0\0",
            "worktree /repo\0HEAD abc\0branch refs/heads/main\0\0",
            "worktree /wt/detached\0HEAD def\0detached\0locked why\nnot\0\0",
            "worktree /wt/feat\0HEAD abc\0branch refs/heads/feat/x\0locked\0\0",
            "worktree /wt/gone\0HEAD abc\0branch refs/heads/gone\0prunable gitdir file points to non-existent location\0\0",
            "worktree /wt/bare-prunable\0HEAD abc\0detached\0prunable\0\0",
        );

        assert_eq!(
            parse_worktrees(z),
            [
                worktree("/repo", Head::Branch("main".into()), false),
                worktree("/wt/detached", Head::Detached("def".into()), false),
                worktree("/wt/feat", Head::Branch("feat/x".into()), false),
                worktree("/wt/gone", Head::Branch("gone".into()), true),
                worktree("/wt/bare-prunable", Head::Detached("abc".into()), true),
            ]
        );
    }

    #[test]
    fn a_record_without_a_path_or_any_output_gives_no_worktree() {
        assert_eq!(parse_worktrees(""), []);
        assert_eq!(parse_worktrees("HEAD abc\0branch refs/heads/main\0\0"), []);
    }

    #[test]
    fn branches_carry_the_remote_of_their_upstream() {
        let z = "refs/heads/main\0origin\nrefs/heads/feat/x\0\nrefs/heads/up\0upstream\n";

        assert_eq!(
            parse_branches(z),
            [
                LocalBranch {
                    name: "main".into(),
                    upstream_remote: Some("origin".into()),
                },
                LocalBranch {
                    name: "feat/x".into(),
                    upstream_remote: None,
                },
                LocalBranch {
                    name: "up".into(),
                    upstream_remote: Some("upstream".into()),
                },
            ]
        );
    }

    #[test]
    fn lines_that_are_no_local_branch_are_skipped() {
        assert_eq!(parse_branches(""), []);
        assert_eq!(parse_branches("refs/heads/main\n"), []);
        assert_eq!(parse_branches("refs/tags/v1\0\n"), []);
    }

    #[test]
    fn remote_branches_drop_the_remote_head() {
        let text = "refs/remotes/origin/HEAD\nrefs/remotes/origin/main\nrefs/remotes/origin/feat/HEAD\nrefs/remotes/up/dev\nrefs/heads/main\n";

        assert_eq!(
            parse_remote_branches(text),
            ["origin/main", "origin/feat/HEAD", "up/dev"]
        );
    }
}
