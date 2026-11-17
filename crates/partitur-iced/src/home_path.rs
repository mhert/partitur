//! Folder paths as the interface writes them: with the user's home directory shortened to
//! `~`, the way a shell prompt shows them. The `~` is followed by the platform's separator,
//! so a Windows path reads `~\AppData\Local` rather than mixing `/` and `\`.

use std::path::{MAIN_SEPARATOR, Path};

/// `path` as text, with the leading `home` directory replaced by `~`; a path outside
/// `home`, or any path while the home directory is unknown, is written in full.
pub fn abbreviated(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~{MAIN_SEPARATOR}{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_under_the_home_directory_starts_with_a_tilde() {
        let home = Path::new("/home/me");
        let shop = home.join("Development").join("shop");

        assert_eq!(
            abbreviated(&shop, Some(home)),
            format!("~{MAIN_SEPARATOR}Development{MAIN_SEPARATOR}shop")
        );
        assert_eq!(abbreviated(home, Some(home)), "~");
    }

    #[test]
    fn a_path_elsewhere_is_written_in_full() {
        let home = Path::new("/home/me");

        assert_eq!(abbreviated(Path::new("/srv/shop"), Some(home)), "/srv/shop");
        assert_eq!(
            abbreviated(Path::new("/home/merle/shop"), Some(home)),
            "/home/merle/shop"
        );
        assert_eq!(
            abbreviated(Path::new("/home/me/shop"), None),
            "/home/me/shop"
        );
    }
}
