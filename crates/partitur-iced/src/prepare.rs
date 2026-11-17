//! Preparing the files attached to a prompt before it is sent.
//!
//! Every image among the files (see [`image_mime`]) is read, copied into the folder it is
//! given and encoded as the inline content the agent is sent, so the transcript can show
//! exactly what was sent even after the original changed or vanished. The copy is named
//! `<n>-<file name>`, with the smallest `n` from 1 that makes it a new file. Next to it a
//! preview of at most 240×240 pixels is saved as `<copy stem>.preview.png`; an image that
//! cannot be previewed is sent all the same, without one. Any other file is only linked and
//! never read.
//!
//! Reading or copying an image is fatal: the whole preparation fails with
//! `cannot attach <path>: <reason>` and removes every copy and preview it had written.
//! The records keep file names only, so the folder can move with the session.
//!
//! None of this runs on the update loop: [`prepare`] and [`discard`] each run on a thread of
//! their own and resolve as tasks.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use iced::Task;
use partitur_core::{Prepared, PromptContent, SentAttachment, image_mime};

use crate::paste::off_loop;

/// The longest side of a preview, in pixels.
const PREVIEW_SIDE: u32 = 240;

/// Prepares `files` in `dir` as the module describes, one [`Prepared`] per file, in order.
///
/// Blocks, so it runs only through [`prepare`].
///
/// # Errors
///
/// Which image could not be read or copied, and why; nothing is left behind then.
pub(crate) fn prepare_now(dir: &Path, files: &[PathBuf]) -> Result<Vec<Prepared>, String> {
    let mut written = Vec::new();
    let mut prepared = Vec::with_capacity(files.len());
    for file in files {
        match prepare_one(dir, file, &mut written) {
            Ok(one) => prepared.push(one),
            Err(error) => {
                for path in &written {
                    let _ = fs::remove_file(path);
                }
                return Err(format!("cannot attach {}: {error}", file.display()));
            }
        }
    }
    Ok(prepared)
}

/// Prepares one file; every file it creates is added to `written` first.
fn prepare_one(dir: &Path, file: &Path, written: &mut Vec<PathBuf>) -> io::Result<Prepared> {
    let name = file.file_name().map_or_else(
        || file.display().to_string(),
        |n| n.to_string_lossy().into(),
    );
    let Some(mime) = image_mime(file) else {
        return Ok(Prepared {
            content: PromptContent::FileLink {
                path: file.to_path_buf(),
            },
            sent: SentAttachment::File {
                name,
                path: file.to_path_buf(),
            },
        });
    };
    let bytes = fs::read(file)?;
    fs::create_dir_all(dir)?;
    let (copy, mut copied) = new_copy(dir, &name)?;
    written.push(dir.join(&copy));
    copied.write_all(&bytes)?;
    drop(copied);
    let preview = preview(dir, &copy, &bytes);
    if let Some(preview) = &preview {
        written.push(dir.join(preview));
    }
    Ok(Prepared {
        content: PromptContent::image(mime, &bytes),
        sent: SentAttachment::Image {
            name,
            copy: PathBuf::from(copy),
            preview: preview.map(PathBuf::from),
        },
    })
}

/// A new file in `dir` named `<n>-<name>`, with the smallest `n` from 1 that is free.
fn new_copy(dir: &Path, name: &str) -> io::Result<(String, fs::File)> {
    let mut n = 1_u32;
    loop {
        let copy = format!("{n}-{name}");
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(&copy))
        {
            Ok(file) => return Ok((copy, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(error) => return Err(error),
        }
    }
}

/// Saves the preview of `bytes` next to `copy` and returns its file name; none when the
/// image cannot be decoded or the preview cannot be written. A failed write leaves no file.
fn preview(dir: &Path, copy: &str, bytes: &[u8]) -> Option<String> {
    let stem = Path::new(copy).file_stem()?.to_string_lossy();
    let name = format!("{stem}.preview.png");
    let path = dir.join(&name);
    let thumbnail = image::load_from_memory(bytes)
        .ok()?
        .thumbnail(PREVIEW_SIDE, PREVIEW_SIDE);
    if thumbnail
        .save_with_format(&path, image::ImageFormat::Png)
        .is_err()
    {
        let _ = fs::remove_file(&path);
        return None;
    }
    Some(name)
}

/// Runs [`prepare_now`] on a thread of its own (`prepare-prompt`) and resolves with its
/// result.
pub(crate) fn prepare(dir: PathBuf, files: Vec<PathBuf>) -> Task<Result<Vec<Prepared>, String>> {
    Task::future(off_loop("prepare-prompt", move || {
        prepare_now(&dir, &files)
    }))
    .map(|done| done.unwrap_or_else(|| Err("cannot prepare the attachments".to_owned())))
}

/// Removes the copies and previews of `sent` from `dir` on a thread of its own
/// (`discard-attachments`); errors are ignored.
pub(crate) fn discard(dir: PathBuf, sent: Vec<SentAttachment>) -> Task<()> {
    Task::future(off_loop("discard-attachments", move || {
        for attachment in &sent {
            if let SentAttachment::Image { copy, preview, .. } = attachment {
                let _ = fs::remove_file(dir.join(copy));
                if let Some(preview) = preview {
                    let _ = fs::remove_file(dir.join(preview));
                }
            }
        }
    }))
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::fs;
    use std::path::{Path, PathBuf};

    use partitur_core::{PromptContent, SentAttachment};

    use super::{discard, prepare, prepare_now};
    use crate::test_support::first_output;

    fn png(path: &Path, width: u32, height: u32, shade: u8) {
        image::RgbaImage::from_pixel(width, height, image::Rgba([shade, 0, 0, 255]))
            .save(path)
            .unwrap();
    }

    fn files_in(dir: &Path) -> usize {
        fs::read_dir(dir).map_or(0, Iterator::count)
    }

    #[test]
    fn an_image_is_copied_previewed_and_encoded() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("shot.png");
        png(&src, 600, 300, 10);
        let dir = tmp.path().join("out");
        let prepared = prepare_now(&dir, std::slice::from_ref(&src)).unwrap();
        let bytes = fs::read(&src).unwrap();
        assert_eq!(prepared.len(), 1);
        assert_eq!(
            prepared[0].content,
            PromptContent::image("image/png", &bytes)
        );
        assert_eq!(
            prepared[0].sent,
            SentAttachment::Image {
                name: "shot.png".into(),
                copy: "1-shot.png".into(),
                preview: Some("1-shot.preview.png".into()),
            }
        );
        assert_eq!(fs::read(dir.join("1-shot.png")).unwrap(), bytes);
        let preview = image::open(dir.join("1-shot.preview.png")).unwrap();
        assert_eq!((preview.width(), preview.height()), (240, 120));
    }

    #[test]
    fn two_images_with_one_name_get_two_copies() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("a")).unwrap();
        fs::create_dir(tmp.path().join("b")).unwrap();
        let (a, b) = (tmp.path().join("a/shot.png"), tmp.path().join("b/shot.png"));
        png(&a, 10, 10, 1);
        png(&b, 10, 10, 2);
        let dir = tmp.path().join("out");
        let prepared = prepare_now(&dir, &[a.clone(), b.clone()]).unwrap();
        let copies: Vec<_> = prepared
            .iter()
            .map(|p| match &p.sent {
                SentAttachment::Image { copy, .. } => copy.clone(),
                SentAttachment::File { .. } => panic!("not an image"),
            })
            .collect();
        assert_eq!(
            copies,
            [PathBuf::from("1-shot.png"), PathBuf::from("2-shot.png")]
        );
        assert_eq!(
            fs::read(dir.join("1-shot.png")).unwrap(),
            fs::read(&a).unwrap()
        );
        assert_eq!(
            fs::read(dir.join("2-shot.png")).unwrap(),
            fs::read(&b).unwrap()
        );
    }

    #[test]
    fn an_image_without_a_preview_is_still_prepared() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join("bad.png");
        fs::write(&bad, b"not a png").unwrap();
        let dir = tmp.path().join("out");
        let prepared = prepare_now(&dir, &[bad]).unwrap();
        assert_eq!(
            prepared[0].sent,
            SentAttachment::Image {
                name: "bad.png".into(),
                copy: "1-bad.png".into(),
                preview: None,
            }
        );
        assert_eq!(fs::read(dir.join("1-bad.png")).unwrap(), b"not a png");
        assert_eq!(files_in(&dir), 1);
        assert_eq!(
            prepared[0].content,
            PromptContent::image("image/png", b"not a png")
        );
    }

    #[test]
    fn another_file_is_linked_not_copied() {
        let tmp = tempfile::tempdir().unwrap();
        let notes = tmp.path().join("notes.txt");
        fs::write(&notes, "hi").unwrap();
        let dir = tmp.path().join("out");
        let prepared = prepare_now(&dir, std::slice::from_ref(&notes)).unwrap();
        assert_eq!(
            prepared[0].content,
            PromptContent::FileLink {
                path: notes.clone()
            }
        );
        assert_eq!(
            prepared[0].sent,
            SentAttachment::File {
                name: "notes.txt".into(),
                path: notes
            }
        );
        assert_eq!(files_in(&dir), 0);
    }

    #[test]
    fn an_unreadable_image_fails_and_leaves_no_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good.png");
        png(&good, 10, 10, 1);
        let missing = tmp.path().join("missing.png");
        let dir = tmp.path().join("out");
        let error = prepare_now(&dir, &[good, missing.clone()]).unwrap_err();
        assert!(
            error.starts_with(&format!("cannot attach {}: ", missing.display())),
            "{error}"
        );
        assert_eq!(files_in(&dir), 0);
    }

    #[test]
    fn files_keep_their_order() {
        let tmp = tempfile::tempdir().unwrap();
        let notes = tmp.path().join("notes.txt");
        fs::write(&notes, "hi").unwrap();
        let shot = tmp.path().join("shot.png");
        png(&shot, 10, 10, 1);
        let prepared = prepare_now(&tmp.path().join("out"), &[notes, shot]).unwrap();
        assert!(matches!(prepared[0].sent, SentAttachment::File { .. }));
        assert!(matches!(prepared[1].sent, SentAttachment::Image { .. }));
    }

    #[test]
    fn discard_removes_copies_and_previews() {
        let tmp = tempfile::tempdir().unwrap();
        let shot = tmp.path().join("shot.png");
        png(&shot, 10, 10, 1);
        let dir = tmp.path().join("out");
        let sent = prepare_now(&dir, &[shot])
            .unwrap()
            .into_iter()
            .map(|p| p.sent)
            .collect();
        assert_eq!(files_in(&dir), 2);
        first_output(discard(dir.clone(), sent));
        assert_eq!(files_in(&dir), 0);
    }

    #[test]
    fn prepare_runs_off_the_caller() {
        let tmp = tempfile::tempdir().unwrap();
        let done = first_output(prepare(tmp.path().join("out"), vec![]));
        assert_eq!(done, Ok(vec![]));
    }
}
