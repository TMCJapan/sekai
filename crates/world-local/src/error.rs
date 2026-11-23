//! Host spellings of [`WorldError`]: the affected path becomes its location.

use std::path::Path;

use sekai_world::WorldError;

pub(crate) fn location(path: impl AsRef<Path>) -> String {
    path.as_ref().display().to_string()
}

pub(crate) fn io(path: impl AsRef<Path>, source: std::io::Error) -> WorldError {
    WorldError::io(location(path), source)
}

pub(crate) fn region_file(path: impl AsRef<Path>, source: sekai_anvil::AnvilError) -> WorldError {
    WorldError::region_file(location(path), source)
}
