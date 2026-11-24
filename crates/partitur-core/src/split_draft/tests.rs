#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use super::*;
use crate::app::test_support::{Family, choice, family, propose_now, proposed, two_children};
use crate::backend::ProposedChild;
use crate::ids::{ProposalId, SessionId};
use crate::session::{BranchChoice, BranchMode};
use crate::testing::{GitOp, repo_info, settle};

fn draft_of(children: &[ProposedChild]) -> SplitDraft {
    SplitDraft::from_entry(children, false, |_| BranchChoice::default())
}

fn checks(problems: &[Option<&str>]) -> Vec<ChildCheck> {
    let checks = problems.iter().map(|problem| ChildCheck {
        problem: problem.map(str::to_owned),
        ..ChildCheck::default()
    });
    checks.collect()
}

fn two() -> SplitDraft {
    draft_of(&two_children())
}

#[test]
fn a_draft_starts_from_the_proposed_children_unchecked() {
    let worktree = BranchChoice {
        mode: BranchMode::Worktree,
        ..BranchChoice::default()
    };
    let draft = SplitDraft::from_entry(&two_children(), true, |child| {
        if child.title == "API endpoint" {
            worktree.clone()
        } else {
            BranchChoice::default()
        }
    });

    assert_eq!(draft.children.len(), 2);
    assert_eq!(
        draft.children[0].repository,
        proposed("api", "", "").repository
    );
    assert_eq!(draft.children[0].title, "API endpoint");
    assert_eq!(draft.children[0].prompt, "Add the endpoint");
    assert!(draft.children[0].auto_allow);
    assert_eq!(draft.children[0].branch, worktree);
    assert_eq!(draft.children[1].title, "Web page");
    assert_eq!(draft.children[1].branch, BranchChoice::default());
    assert_eq!(draft.note, "");
    assert_eq!(draft.checks, Ok(Vec::new()));
}

#[test]
fn auto_allow_is_off_unless_the_draft_is_made_with_it() {
    assert!(!two().children[0].auto_allow);
}

#[test]
fn the_choices_trim_the_prompts_and_titles_and_keep_the_rest() {
    let mut draft = two();
    draft.children[0].prompt = "  Add it \n".into();
    draft.children[0].title = "\t API  ".into();
    draft.children[1].branch.branch = " keep ".into();
    draft.children[1].auto_allow = true;

    let choices = draft.choices();

    assert_eq!(choices[0].prompt, "Add it");
    assert_eq!(choices[0].title, "API");
    assert_eq!(choices[0].repository, draft.children[0].repository);
    assert_eq!(choices[1].prompt, "Add the page");
    assert_eq!(choices[1].title, "Web page");
    assert_eq!(choices[1].branch.branch, " keep ");
    assert!(choices[1].auto_allow);
    // The draft keeps what was typed.
    assert_eq!(draft.children[0].prompt, "  Add it \n");
}

#[test]
fn a_split_is_approvable_once_every_child_is_read_and_without_a_problem() {
    let mut draft = two();
    assert!(!draft.approvable(), "not checked yet");

    draft.checks = Ok(checks(&[None, None]));
    assert!(draft.approvable());

    draft.checks = Ok(checks(&[None]));
    assert!(!draft.approvable(), "one check short");
    draft.checks = Ok(checks(&[None, None, None]));
    assert!(!draft.approvable(), "one check too many");
    draft.checks = Ok(checks(&[None, Some("busy")]));
    assert!(!draft.approvable(), "a problem");
    draft.checks = Err("unreadable".into());
    assert!(!draft.approvable(), "no check");
    draft.checks = Ok(checks(&[None, None]));
    draft.checks.as_mut().unwrap()[1].loading = true;
    assert!(!draft.approvable(), "loading");
    draft.checks.as_mut().unwrap()[1].loading = false;
    draft.checks.as_mut().unwrap()[0].loading = true;
    assert!(!draft.approvable(), "the first one loading");
}

#[test]
fn a_split_without_children_is_not_approvable() {
    let mut draft = draft_of(&[]);
    draft.checks = Ok(Vec::new());

    assert!(!draft.approvable());
}

#[test]
fn the_blocker_is_the_first_problem_and_the_count_of_all() {
    let mut draft = two();
    assert_eq!(draft.blocker(), None);
    draft.checks = Ok(checks(&[None, None]));
    assert_eq!(draft.blocker(), None);
    draft.checks = Err("unreadable".into());
    assert_eq!(draft.blocker(), None);

    draft.checks = Ok(checks(&[None, Some("busy")]));
    assert_eq!(
        draft.blocker(),
        Some(Blocker {
            index: 1,
            problem: "busy",
            count: 1
        })
    );
    draft.checks = Ok(checks(&[
        Some("first"),
        Some("second"),
        None,
        Some("third"),
    ]));
    assert_eq!(
        draft.blocker(),
        Some(Blocker {
            index: 0,
            problem: "first",
            count: 3
        })
    );
}

#[test]
fn a_check_is_found_by_the_childs_position() {
    let mut draft = two();
    draft.checks = Ok(checks(&[None, Some("busy")]));

    assert_eq!(draft.check_of(0).unwrap().problem, None);
    assert_eq!(draft.check_of(1).unwrap().problem.as_deref(), Some("busy"));
    assert_eq!(draft.check_of(2), None);
    draft.checks = Err("unreadable".into());
    assert_eq!(draft.check_of(0), None);
}

#[test]
fn a_blank_note_sends_none_and_any_other_is_trimmed() {
    let mut draft = two();
    assert_eq!(draft.rejection_note(), None);
    draft.note = " \n\t".into();
    assert_eq!(draft.rejection_note(), None);
    draft.note = "  not now \n".into();
    assert_eq!(draft.rejection_note().as_deref(), Some("not now"));
}

#[test]
fn checking_asks_the_core_about_the_children_as_they_are() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let id = propose_now(&mut fixture, &parent, two_children());
    let mut draft = two();
    draft.children.push(choice("docs", "Do it", "Docs", false));

    let wanted = draft.check(&mut fixture.app, &parent, &id, true);

    assert!(wanted);
    let checks = draft.checks.unwrap();
    assert_eq!(checks.len(), 3);
    assert_eq!(
        checks[2].problem.as_deref(),
        Some("unknown repository \"docs\"")
    );
}

#[test]
fn a_draft_whose_proposal_or_orchestrator_is_gone_is_not_wanted() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let id = propose_now(&mut fixture, &parent, two_children());
    let mut draft = two();

    for by_user in [true, false] {
        assert!(!draft.check(&mut fixture.app, &SessionId::new(), &id, by_user));
        assert!(draft.checks.as_ref().unwrap_err().contains("session"));
        assert!(!draft.check(&mut fixture.app, &parent, &ProposalId::new(), by_user));
        assert!(draft.checks.as_ref().unwrap_err().contains("proposal"));
        assert!(draft.check(&mut fixture.app, &parent, &id, by_user));
        assert!(draft.checks.is_ok());
    }
}

#[test]
fn a_draft_is_still_wanted_when_the_check_fails_for_another_reason() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let id = propose_now(&mut fixture, &parent, two_children());
    let mut draft = two();
    fixture.store.fail_next_settings_load();

    assert!(draft.check(&mut fixture.app, &parent, &id, true));
    assert!(draft.checks.is_err());
}

#[test]
fn only_a_check_the_user_caused_reads_a_failed_repository_again() {
    let Family {
        mut fixture,
        dir,
        parent,
        ..
    } = family();
    let id = propose_now(&mut fixture, &parent, two_children());
    let web = dir.path().join("web");
    fixture
        .git
        .add_repository(web.clone(), repo_info(&web, "main", &["main"], &[]));
    fixture.git.fail_next(GitOp::Inspect, &web);
    let mut draft = two();
    draft.children.remove(0);
    draft.children[0].branch = BranchChoice {
        mode: BranchMode::Worktree,
        branch: "feat/x".into(),
        base: None,
        fetch: false,
    };
    assert!(draft.check(&mut fixture.app, &parent, &id, true));
    assert!(draft.check_of(0).unwrap().loading);
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    assert!(draft.check(&mut fixture.app, &parent, &id, false));
    assert!(draft.check_of(0).unwrap().problem.is_some(), "kept");
    assert!(!draft.approvable());
    assert!(draft.blocker().is_some());

    assert!(draft.check(&mut fixture.app, &parent, &id, true));
    assert!(draft.check_of(0).unwrap().loading, "read again");
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());
    assert!(draft.check(&mut fixture.app, &parent, &id, false));
    assert!(draft.approvable());
}
