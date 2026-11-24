//! The decoded previews of the images the current session's prompts were sent with.
//!
//! Whether a sent image has a preview to show is decided in the update loop, never while
//! drawing: after every message that is not a stream of frames, [`sync`] looks at the current
//! session's prompts and starts reading and decoding each preview file it has not seen yet, on
//! a thread of its own (`preview-load`). The outcome comes back as [`Message::Preview`] and is
//! kept by the file's path, a picture or the fact that the file could not be read. The folder
//! of each session's preview files is kept as well, so a session that leaves the tree takes
//! its previews with it ([`Previews::forget`]), and a load that finishes after that is
//! dropped. The view only looks a preview up: until its picture is there, and when the file
//! is missing or cannot be decoded, the image is drawn as the chip of its name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use iced::Task;
use iced::widget::image::Handle;
use partitur_core::{Entry, SentAttachment, SessionId, SessionViewModel};

use crate::app::{Message, State};
use crate::background::off_loop;

/// The previews asked for so far, by the path of their file, and the folder each session's
/// preview files are in.
#[derive(Debug, Default)]
pub(crate) struct Previews {
    files: HashMap<PathBuf, Preview>,
    dirs: HashMap<SessionId, PathBuf>,
}

/// Where the preview of one file stands.
#[derive(Debug)]
enum Preview {
    /// The file is being read and decoded.
    Loading,
    /// The decoded picture.
    Ready(Handle),
    /// The file could not be read or decoded.
    Missing,
}

impl Previews {
    /// The decoded picture of the preview file `path`, once there is one.
    pub(crate) fn get(&self, path: &Path) -> Option<&Handle> {
        match self.files.get(path) {
            Some(Preview::Ready(handle)) => Some(handle),
            Some(Preview::Loading | Preview::Missing) | None => None,
        }
    }

    /// Forgets the previews of `sessions`, which left the tree: every preview file in a
    /// session's folder.
    pub(crate) fn forget(&mut self, sessions: &[SessionId]) {
        for session in sessions {
            if let Some(dir) = self.dirs.remove(session) {
                self.files.retain(|path, _| !path.starts_with(&dir));
            }
        }
    }

    /// Whether the preview file `path` was asked for, loaded or not.
    #[cfg(test)]
    pub(crate) fn asked_for(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    /// Keeps the outcome of loading the preview file `path`: its picture, or `None` when it
    /// could not be read.
    pub(crate) fn keep(&mut self, path: PathBuf, picture: Option<Handle>) {
        let preview = picture.map_or(Preview::Missing, Preview::Ready);
        self.files.insert(path, preview);
    }
}

/// Has `previews` start loading the preview of every image the prompts of `model`, the
/// current session, were sent with that was not asked for yet, and returns the work that
/// loads them; nothing while no session is selected.
pub(crate) fn sync(previews: &mut Previews, model: Option<&SessionViewModel<'_>>) -> Task<Message> {
    let Some(model) = model else {
        return Task::none();
    };
    let Some(dir) = model.attachment_dir.as_deref() else {
        return Task::none();
    };
    previews.dirs.insert(model.id.clone(), dir.to_path_buf());
    let files = &mut previews.files;
    let wanted: Vec<PathBuf> = model
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::User { attachments, .. } => Some(attachments),
            _ => None,
        })
        .flatten()
        .filter_map(|attachment| match attachment {
            SentAttachment::Image {
                preview: Some(preview),
                ..
            } => Some(dir.join(preview)),
            SentAttachment::Image { preview: None, .. } | SentAttachment::File { .. } => None,
        })
        .filter(|path| !files.contains_key(path))
        .collect();
    let loads = wanted.into_iter().map(|path| {
        files.insert(path.clone(), Preview::Loading);
        load(path)
    });
    Task::batch(loads.collect::<Vec<_>>())
}

/// Reads and decodes the preview file `path` on a thread of its own (`preview-load`) and
/// resolves with [`Message::Preview`].
pub(crate) fn load(path: PathBuf) -> Task<Message> {
    Task::future(off_loop("preview-load", {
        let path = path.clone();
        move || decode(&path)
    }))
    .map(move |done| Message::Preview(path.clone(), done.flatten()))
}

/// The picture in the file `path`, or `None` when it cannot be read or decoded.
fn decode(path: &Path) -> Option<Handle> {
    let rgba = ::image::open(path).ok()?.into_rgba8();
    Some(Handle::from_rgba(
        rgba.width(),
        rgba.height(),
        rgba.into_raw(),
    ))
}

/// Keeps the outcome of loading the preview file `path`: its picture, or `None` when it could
/// not be read. Nothing follows from it but drawing the picture. An outcome for a file no
/// longer asked for, because its session was forgotten meanwhile, is dropped.
pub(crate) fn landed(state: &mut State, path: PathBuf, picture: Option<Handle>) -> Task<Message> {
    let previews = &mut state.session.previews;
    if previews.files.contains_key(&path) {
        previews.keep(path, picture);
    }
    Task::none()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::test_support::{first_output, temp_dir};

    #[test]
    fn a_preview_file_decodes_into_a_picture() {
        let dir = temp_dir();
        let path = dir.path().join("p.png");
        ::image::RgbaImage::new(4, 2).save(&path).unwrap();

        let message = first_output(load(path.clone()));

        let Message::Preview(loaded, Some(_)) = &message else {
            panic!("{message:?}");
        };
        assert_eq!(loaded, &path);
    }

    #[test]
    fn a_missing_preview_file_has_no_picture() {
        let dir = temp_dir();
        let path = dir.path().join("gone.png");

        let message = first_output(load(path.clone()));

        assert_eq!(message, Message::Preview(path, None));
    }

    #[test]
    fn a_missing_or_loading_preview_is_not_drawn() {
        let mut previews = Previews::default();
        previews.files.insert("a".into(), Preview::Loading);
        previews.files.insert("b".into(), Preview::Missing);

        assert!(previews.get(Path::new("a")).is_none());
        assert!(previews.get(Path::new("b")).is_none());
        assert!(previews.get(Path::new("c")).is_none());
    }
}
