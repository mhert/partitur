//! Persistence of Partitur sessions and settings on the local disk.

pub mod file_store;
pub mod paths;
mod settings_file;

pub use file_store::FileStore;
pub use paths::StatePaths;
