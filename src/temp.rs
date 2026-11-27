use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_name(prefix: &str) -> String {
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(".{prefix}-{}-{seq}-{:x}", std::process::id(), nanos)
}

pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn tempdir() -> io::Result<TempDir> {
    tempdir_in(std::env::temp_dir())
}

pub fn tempdir_in(dir: impl AsRef<Path>) -> io::Result<TempDir> {
    let dir = dir.as_ref();
    for _ in 0..64 {
        let path = dir.join(unique_name("drvjs-dir"));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(TempDir { path }),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create unique temp directory",
    ))
}

pub struct NamedTempFile {
    file: Option<File>,
    path: PathBuf,
    persisted: bool,
}

impl NamedTempFile {
    pub fn new() -> io::Result<Self> {
        Self::new_in(std::env::temp_dir())
    }

    pub fn new_in(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref();
        for _ in 0..64 {
            let path = dir.join(unique_name("drvjs-tmp"));
            match OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)
            {
                Ok(file) => {
                    return Ok(Self {
                        file: Some(file),
                        path,
                        persisted: false,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create unique temp file",
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn persist(mut self, dest: impl AsRef<Path>) -> io::Result<()> {
        drop(self.file.take());
        fs::rename(&self.path, dest)?;
        self.persisted = true;
        Ok(())
    }
}

impl Write for NamedTempFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file
            .as_mut()
            .expect("temp file already closed")
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .expect("temp file already closed")
            .flush()
    }
}

impl Drop for NamedTempFile {
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.persisted {
            let _ = fs::remove_file(&self.path);
        }
    }
}
