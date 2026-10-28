//! File secrets of `hoist secrets exec` (spec §15.7.1 step 5): a directory
//! `/dev/shm/hoist-<random>` (0700) in memory, one 0600 file per value, never on disk. The
//! directory is removed when it is dropped: when the command exits or hoist is signalled.

use ring::rand::{SecureRandom, SystemRandom};
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub struct Dir(PathBuf);

impl Dir {
    /// A new private directory under `root` (`/dev/shm`). `Err` when `root` is missing.
    pub fn create(root: &Path) -> Result<Dir, String> {
        if !root.is_dir() {
            return Err(format!(
                "file secrets need {} (memory only; never a disk)",
                root.display()
            ));
        }
        let mut random = [0u8; 12];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| "no randomness for the file secrets' directory".to_string())?;
        let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let path = root.join(format!("hoist-{name}"));
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| format!("could not create {} ({})", path.display(), e.kind()))?;
        Ok(Dir(path))
    }

    /// Writes `value` to a new 0600 file `name` and returns its path.
    pub fn write(&self, name: &str, value: &[u8]) -> Result<PathBuf, String> {
        let path = self.0.join(name);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path);
        file.and_then(|mut f| f.write_all(value))
            .map_err(|e| format!("could not write the file secret `{name}` ({})", e.kind()))?;
        Ok(path)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
