//! Persistence of Partitur sessions and settings on the local disk, and the reads of the
//! local file system the [`Files`](partitur_core::Files) port asks for.

pub mod file_store;
pub mod files;
pub mod paths;
mod settings_file;
pub mod writer;

pub use file_store::FileStore;
pub use files::DiskFiles;
pub use paths::StatePaths;
pub use writer::{FileWriter, QueuedStore};
