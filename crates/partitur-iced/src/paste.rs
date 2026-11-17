//! Reading what the user pastes into the prompt from the system clipboard.
//!
//! A paste looks at the clipboard in a fixed order: a list of copied files first, then an
//! image, then text. Text that is nothing but `file://` URLs, one per line (what some file
//! managers put on the clipboard instead of a file list), counts as copied files too.
//! Anything else is pasted as text. A clipboard that cannot be opened or holds none of these
//! pastes nothing.
//!
//! Neither the read nor the saving of an image runs on the update loop: [`read`] runs the
//! reader on a thread of its own (`clipboard`) and [`save`] writes the image on another
//! (`paste-save`), each resolving as a task. A pasted image is saved as a PNG, one uniquely
//! named file per paste, in the folder it is given; the Session tab passes [`default_dir`],
//! `partitur-paste` in the user's runtime folder or else in the system's temporary folder.
//! On Unix that folder is created for the user alone, and a folder there that other users
//! can reach, or a link in its place, is refused.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::thread;

use futures::channel::oneshot;
use iced::Task;
use image::ImageEncoder;
use image::codecs::png::PngEncoder;

/// What a paste found on the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pasted {
    /// Copied files, to attach.
    Files(Vec<PathBuf>),
    /// A copied image, to save and attach.
    Image(Image),
    /// Text, to paste into the prompt.
    Text(String),
    /// Nothing that can be pasted.
    Nothing,
}

/// The pixels of a copied image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// The width in pixels.
    pub width: usize,
    /// The height in pixels.
    pub height: usize,
    /// The pixels, row by row, four bytes (red, green, blue, alpha) each.
    pub rgba: Vec<u8>,
}

/// Reads the clipboard; blocks, so it runs only through [`read`].
pub(crate) type Reader = fn() -> Pasted;

/// Where pasted images go: `partitur-paste` in the user's runtime folder
/// (`XDG_RUNTIME_DIR`), which no other user can reach, or else in the system's temporary
/// folder.
pub(crate) fn default_dir() -> PathBuf {
    dir_in(std::env::var_os("XDG_RUNTIME_DIR"), std::env::temp_dir())
}

/// `partitur-paste` in `runtime` when it is set and not empty, in `temp` otherwise.
fn dir_in(runtime: Option<OsString>, temp: PathBuf) -> PathBuf {
    runtime
        .filter(|runtime| !runtime.is_empty())
        .map_or(temp, PathBuf::from)
        .join("partitur-paste")
}

/// Reads the system clipboard in the order the module describes.
///
/// It blocks on the windowing system, so it must never run on the update loop.
pub(crate) fn system() -> Pasted {
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return Pasted::Nothing;
    };
    if let Ok(files) = clipboard.get().file_list()
        && !files.is_empty()
    {
        return Pasted::Files(files);
    }
    if let Ok(image) = clipboard.get().image() {
        return Pasted::Image(Image {
            width: image.width,
            height: image.height,
            rgba: image.bytes.into_owned(),
        });
    }
    match clipboard.get().text() {
        Ok(text) => file_urls(&text).map_or_else(
            || {
                if text.is_empty() {
                    Pasted::Nothing
                } else {
                    Pasted::Text(text)
                }
            },
            Pasted::Files,
        ),
        Err(_) => Pasted::Nothing,
    }
}

/// The files named by `text` when it is nothing but `file://` URLs, one per line.
///
/// Blank lines and `#` comment lines are skipped, as in a `text/uri-list`. The URLs name no
/// host or `localhost`, and their percent escapes are decoded as UTF-8. Text with any other
/// line, or with no URL at all, names no files.
pub(crate) fn file_urls(text: &str) -> Option<Vec<PathBuf>> {
    let paths = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(file_url)
        .collect::<Option<Vec<_>>>()?;
    (!paths.is_empty()).then_some(paths)
}

/// The path of one `file://` URL.
fn file_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    if !(host.is_empty() || host == "localhost") {
        return None;
    }
    let path = percent_decode(path)?;
    Some(PathBuf::from(drive_path(&path)))
}

/// `path` without the slash before a Windows drive letter (`/C:/work` is `C:/work`).
#[cfg(windows)]
fn drive_path(path: &str) -> &str {
    let bytes = path.as_bytes();
    match (bytes.get(1), bytes.get(2)) {
        (Some(letter), Some(b':')) if letter.is_ascii_alphabetic() => path.get(1..).unwrap_or(path),
        _ => path,
    }
}

/// `path` as it is: only Windows paths start with a drive letter.
#[cfg(not(windows))]
fn drive_path(path: &str) -> &str {
    path
}

/// `text` with its `%XX` escapes decoded, when they are all well formed and spell UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = tail.get(..2)?;
            let hex = std::str::from_utf8(hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = tail.get(2..)?;
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}

/// Saves `image` as a PNG of its own in `dir`, which it creates, and returns its path.
///
/// # Errors
///
/// Why the image could not be saved: its pixels do not match its size, or the folder or the
/// file cannot be written.
pub(crate) fn save_png(dir: &Path, image: &Image) -> Result<PathBuf, String> {
    let fail = |error: &dyn std::fmt::Display| format!("cannot save the pasted image: {error}");
    let width = u32::try_from(image.width).map_err(|error| fail(&error))?;
    let height = u32::try_from(image.height).map_err(|error| fail(&error))?;
    let expected = image
        .width
        .checked_mul(image.height)
        .and_then(|n| n.checked_mul(4));
    if expected != Some(image.rgba.len()) {
        return Err(fail(&format!(
            "{} bytes do not make a {}×{} image",
            image.rgba.len(),
            image.width,
            image.height
        )));
    }
    private_dir(dir).map_err(|error| fail(&error))?;
    let (path, file) = new_file(dir).map_err(|error| fail(&error))?;
    PngEncoder::new(BufWriter::new(file))
        .write_image(&image.rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|error| fail(&error))?;
    Ok(path)
}

/// Creates `dir` for the user alone (mode `0700`) unless it exists, and makes sure it is a
/// real folder that no other user can reach, so no one else reads the pasted images or
/// puts the folder there in their place.
#[cfg(unix)]
fn private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error),
        _ => {}
    }
    let metadata = fs::symlink_metadata(dir)?;
    #[expect(
        clippy::verbose_bit_mask,
        reason = "the group and others permission bits read clearer as a mask"
    )]
    let private = metadata.permissions().mode() & 0o077 == 0;
    if metadata.is_dir() && private {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{} can be reached by other users",
            dir.display()
        )))
    }
}

/// Creates `dir` unless it exists; the temporary folder is the user's own on this system.
#[cfg(not(unix))]
fn private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

/// A file in `dir` that did not exist before, named after the time.
fn new_file(dir: &Path) -> io::Result<(PathBuf, fs::File)> {
    let stem = format!("pasted-{}", jiff::Timestamp::now().as_nanosecond());
    let mut attempt = 0_u32;
    loop {
        let name = if attempt == 0 {
            format!("{stem}.png")
        } else {
            format!("{stem}-{attempt}.png")
        };
        let path = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => attempt += 1,
            Err(error) => return Err(error),
        }
    }
}

/// Runs `reader` on a thread of its own and resolves with what it found; nothing when the
/// thread cannot run.
pub(crate) fn read(reader: Reader) -> Task<Pasted> {
    Task::future(off_loop("clipboard", reader)).map(|found| found.unwrap_or(Pasted::Nothing))
}

/// Saves `image` in `dir` on a thread of its own and resolves with the file, or with why it
/// could not be saved.
pub(crate) fn save(dir: PathBuf, image: Image) -> Task<Result<PathBuf, String>> {
    Task::future(off_loop("paste-save", move || save_png(&dir, &image)))
        .map(|saved| saved.unwrap_or_else(|| Err("cannot save the pasted image".to_owned())))
}

/// Runs `work` on a thread called `name` and resolves with its result, or with none when the
/// thread could not start or ended without one.
pub(crate) async fn off_loop<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (sender, receiver) = oneshot::channel();
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // The receiver may be gone when the window closed meanwhile.
            let _ = sender.send(work());
        })
        .ok()?;
    receiver.await.ok()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::path::PathBuf;

    use super::{Image, Pasted, file_urls, read, save_png};
    use crate::test_support::first_output;

    #[test]
    fn file_urls_reads_one_path_per_line() {
        assert_eq!(
            file_urls("file:///work/a.png\r\nfile:///work/b.txt\n"),
            Some(vec![
                PathBuf::from("/work/a.png"),
                PathBuf::from("/work/b.txt")
            ])
        );
    }

    #[test]
    fn file_urls_decodes_percent_escapes() {
        assert_eq!(
            file_urls("file:///home/u/My%20Shots/b%C3%A4r.png"),
            Some(vec![PathBuf::from("/home/u/My Shots/bär.png")])
        );
    }

    #[test]
    fn file_urls_accepts_the_local_host() {
        assert_eq!(
            file_urls("file://localhost/a"),
            Some(vec![PathBuf::from("/a")])
        );
        assert_eq!(file_urls("file://elsewhere/a"), None);
    }

    #[test]
    fn file_urls_skips_comment_lines() {
        // text/uri-list allows `#` comments.
        assert_eq!(
            file_urls("# copied\nfile:///a"),
            Some(vec![PathBuf::from("/a")])
        );
    }

    #[test]
    fn file_urls_refuses_text_that_is_not_only_file_urls() {
        assert_eq!(file_urls("see file:///a"), None);
        assert_eq!(file_urls("file:///a\nhello"), None);
        assert_eq!(file_urls("/work/a.png"), None);
        assert_eq!(file_urls("https://example.com/a.png"), None);
        assert_eq!(file_urls("file:///a%zz"), None);
        assert_eq!(file_urls("file:///a%ff"), None);
        assert_eq!(file_urls(""), None);
        assert_eq!(file_urls("\n\n"), None);
        assert_eq!(file_urls("# only a comment"), None);
    }

    #[cfg(windows)]
    #[test]
    fn file_urls_reads_windows_drive_paths() {
        assert_eq!(
            file_urls("file:///C:/work/a.png"),
            Some(vec![PathBuf::from("C:/work/a.png")])
        );
    }

    #[test]
    fn save_png_writes_a_png_of_the_image() {
        let dir = tempfile::tempdir().unwrap();
        let image = Image {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
        };
        let path = save_png(&dir.path().join("partitur-paste"), &image).unwrap();
        assert_eq!(path.extension().unwrap(), "png");
        let decoded = image::open(&path).unwrap().into_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (2, 1));
        assert_eq!(decoded.into_raw(), image.rgba);
    }

    #[test]
    fn save_png_gives_each_paste_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let image = Image {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        let folder = dir.path().join("partitur-paste");
        let first = save_png(&folder, &image).unwrap();
        let second = save_png(&folder, &image).unwrap();
        assert_ne!(first, second);
        assert!(first.exists() && second.exists());
    }

    #[test]
    fn save_png_refuses_pixels_that_do_not_match_the_size() {
        let dir = tempfile::tempdir().unwrap();
        let image = Image {
            width: 2,
            height: 2,
            rgba: vec![0; 4],
        };
        let error = save_png(dir.path(), &image).unwrap_err();
        assert!(error.starts_with("cannot save the pasted image"), "{error}");
    }

    #[test]
    fn save_png_reports_a_folder_it_cannot_create() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a-file");
        std::fs::write(&file, b"").unwrap();
        let image = Image {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        let error = save_png(&file.join("below"), &image).unwrap_err();
        assert!(
            error.starts_with("cannot save the pasted image: "),
            "{error}"
        );
    }

    #[test]
    fn pasted_images_go_to_the_runtime_folder_when_there_is_one() {
        assert_eq!(
            super::dir_in(Some("/run/user/1000".into()), PathBuf::from("/tmp")),
            PathBuf::from("/run/user/1000/partitur-paste")
        );
        assert_eq!(
            super::dir_in(Some("".into()), PathBuf::from("/tmp")),
            PathBuf::from("/tmp/partitur-paste")
        );
        assert_eq!(
            super::dir_in(None, PathBuf::from("/tmp")),
            PathBuf::from("/tmp/partitur-paste")
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_png_creates_its_folder_for_the_user_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("partitur-paste");
        let image = Image {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        save_png(&folder, &image).unwrap();
        let mode = std::fs::metadata(&folder).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn save_png_refuses_a_folder_other_users_can_reach() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("partitur-paste");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        let image = Image {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        let error = save_png(&folder, &image).unwrap_err();
        assert!(error.contains("other users"), "{error}");
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn save_png_refuses_a_link_in_place_of_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        let folder = dir.path().join("partitur-paste");
        std::os::unix::fs::symlink(&target, &folder).unwrap();
        let image = Image {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        assert!(save_png(&folder, &image).is_err());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    }

    #[test]
    fn read_runs_the_reader_off_the_caller() {
        let task = read(|| Pasted::Text("hi".into()));
        assert_eq!(first_output(task), Pasted::Text("hi".into()));
    }

    #[test]
    fn save_writes_the_image_off_the_caller() {
        let dir = tempfile::tempdir().unwrap();
        let image = Image {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        let path = first_output(super::save(dir.path().join("partitur-paste"), image)).unwrap();
        assert!(path.starts_with(dir.path()));
    }
}
