#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

use super::Owner;
use crate::app::test_support::{answer_files, fixture};
use crate::files::{FilesAnswer, FilesError, FilesEvent, FilesQuery, FilesRequestId, ResolvedPath};

fn resolves(files: &crate::testing::MemoryFiles) -> Vec<PathBuf> {
    files
        .queries()
        .into_iter()
        .filter_map(|query| match query {
            FilesQuery::Resolve(mut paths) if paths.len() == 1 => paths.pop(),
            _ => None,
        })
        .collect()
}

#[test]
fn a_folder_is_asked_about_once_at_a_time_and_again_only_when_wanted() {
    let mut fixture = fixture();
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("wt");
    let never = dir.path().join("never");
    fixture
        .app
        .folders
        .show(Owner::Draft, vec![folder.clone(), never.clone()]);

    fixture.app.ask_folder(&folder, true);
    fixture.app.ask_folder(&folder, true);
    assert_eq!(resolves(&fixture.files), std::slice::from_ref(&folder));
    assert!(!fixture.app.folders.exists(&folder));

    std::fs::create_dir(&folder).unwrap();
    answer_files(&mut fixture);
    assert!(fixture.app.folders.exists(&folder));

    fixture.app.ask_folder(&folder, false);
    assert_eq!(resolves(&fixture.files).len(), 1);
    fixture.app.ask_folder(&folder, true);
    assert_eq!(resolves(&fixture.files).len(), 2);
    std::fs::remove_dir(&folder).unwrap();
    answer_files(&mut fixture);
    assert!(!fixture.app.folders.exists(&folder));

    fixture.app.ask_folder(&never, false);
    assert_eq!(resolves(&fixture.files).last(), Some(&never));
    answer_files(&mut fixture);
}

#[test]
fn a_folder_the_port_could_not_tell_about_counts_as_missing() {
    let mut fixture = fixture();
    let folder = Path::new("/wt/feat-x");
    fixture
        .app
        .folders
        .show(Owner::Draft, vec![folder.to_path_buf()]);
    let request = |fixture: &mut crate::app::test_support::Fixture| {
        fixture.app.ask_folder(folder, true);
        let FilesEvent::Answered { request, .. } = fixture.files.answer().pop().unwrap();
        request
    };

    for answers in [
        vec![Err(FilesError::Unavailable("gone".into()))],
        vec![Ok(FilesAnswer::Read(None))],
        vec![Ok(FilesAnswer::Resolved(vec![ResolvedPath {
            given: PathBuf::from("/elsewhere"),
            canonical: Some(PathBuf::from("/elsewhere")),
            is_symlink: false,
            present: true,
        }]))],
        Vec::new(),
    ] {
        let request = request(&mut fixture);
        let event = FilesEvent::Answered { request, answers };
        assert_eq!(fixture.app.apply_files_event(event).unwrap(), None);
        assert!(!fixture.app.folders.exists(folder));
    }

    let request = request(&mut fixture);
    let found = vec![Ok(FilesAnswer::Resolved(vec![ResolvedPath {
        given: folder.to_path_buf(),
        canonical: Some(folder.to_path_buf()),
        is_symlink: false,
        present: true,
    }]))];
    let event = FilesEvent::Answered {
        request,
        answers: found.clone(),
    };
    fixture.app.apply_files_event(event).unwrap();
    assert!(fixture.app.folders.exists(folder));

    // An answer no question waits for changes nothing.
    assert_eq!(
        fixture
            .app
            .folder_answered(FilesRequestId(999), found.clone()),
        Some(found)
    );
}

#[test]
fn only_the_answers_about_folders_still_shown_are_kept() {
    let mut fixture = fixture();
    let dir = tempfile::tempdir().unwrap();
    let (first, second) = (dir.path().join("first"), dir.path().join("second"));
    std::fs::create_dir(&first).unwrap();
    let proposal = crate::ids::ProposalId::new();
    fixture
        .app
        .folders
        .show(Owner::Proposal(proposal.clone()), vec![first.clone()]);
    fixture.app.ask_folder(&first, true);
    answer_files(&mut fixture);
    assert!(fixture.app.folders.exists(&first));

    // Another owner showing another folder keeps the first.
    fixture.app.folders.show(Owner::Draft, vec![second.clone()]);
    assert!(fixture.app.folders.exists(&first));
    // An answer about a folder nothing shows is not kept.
    fixture.app.folders.show(Owner::Draft, Vec::new());
    fixture.app.ask_folder(&second, true);
    std::fs::create_dir(&second).unwrap();
    answer_files(&mut fixture);
    assert!(!fixture.app.folders.known.contains_key(&second));

    fixture.app.folders.hide(&Owner::Proposal(proposal));
    assert!(fixture.app.folders.known.is_empty());
}

#[test]
fn an_older_answer_arriving_last_does_not_undo_a_newer_one() {
    let mut fixture = fixture();
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("wt");
    fixture.app.folders.show(Owner::Draft, vec![folder.clone()]);
    fixture.app.ask_folder(&folder, true);
    let older = fixture.files.answer();
    std::fs::create_dir(&folder).unwrap();
    fixture.app.ask_folder_now(&folder);
    let newer = fixture.files.answer();

    for event in newer.into_iter().chain(older) {
        assert_eq!(fixture.app.apply_files_event(event).unwrap(), None);
    }

    assert!(fixture.app.folders.exists(&folder));
}
