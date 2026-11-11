//! Private data, cache and runtime paths.

use std::io;
use std::path::{Path, PathBuf};

const NAME: &str = "gmessages-tui";

pub fn data() -> io::Result<PathBuf> {
    dirs::data_dir()
        .map(|path| path.join(NAME))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "data directory unavailable"))
}

pub fn cache() -> io::Result<PathBuf> {
    dirs::cache_dir()
        .map(|path| path.join(NAME))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cache directory unavailable"))
}

pub fn runtime() -> io::Result<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join(NAME))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR unavailable"))
}

/// Creates `dir` for state that only this user may read.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}
