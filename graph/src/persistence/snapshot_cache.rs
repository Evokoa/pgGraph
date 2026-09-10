//! Immutable Linux snapshots shared through weak descriptor hints.
//!
//! Hints locate sealed bytes; they never replace artifact validation. Callers
//! advertise only after the complete persistence loader accepts a snapshot.
//! Concurrent cold misses may create separate sealed copies. Registry locks
//! cover only discovery and advertisement, never validation or source copying.

use memmap2::{Mmap, MmapMut};
use std::fs::File;
#[cfg(target_os = "linux")]
use std::fs::{self, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Write;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::sync::Arc;

/// Effective ownership mode of an immutable base snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SnapshotMode {
    /// Bytes reopened from another retained, sealed descriptor.
    SharedHit,
    /// A newly copied sealed object that may be advertised after validation.
    SealedCreated,
    /// Private anonymous bytes on an unsupported or unavailable cache path.
    PrivateFallback,
}

impl SnapshotMode {
    /// Stable diagnostic name for the effective storage mode.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::SharedHit => "shared_hit",
            Self::SealedCreated => "sealed_created",
            Self::PrivateFallback => "private_fallback",
        }
    }
}

/// Owns mapped bytes and, on Linux, the descriptor used for discovery.
pub(super) struct Snapshot {
    mmap: Arc<Mmap>,
    backing: Backing,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Snapshot")
            .field("bytes", &self.mmap.len())
            .field("mode", &self.mode().as_str())
            .finish()
    }
}

#[derive(Debug)]
enum Backing {
    Private,
    #[cfg(target_os = "linux")]
    Sealed {
        snapshot: linux::SealedSnapshot,
        reused: bool,
    },
}

impl Snapshot {
    /// Immutable bytes; callers still must validate their artifact contents.
    pub(super) fn mmap(&self) -> &Arc<Mmap> {
        &self.mmap
    }

    /// Reports whether acquisition shared a sealed object or copied bytes.
    pub(super) fn mode(&self) -> SnapshotMode {
        match &self.backing {
            Backing::Private => SnapshotMode::PrivateFallback,
            #[cfg(target_os = "linux")]
            Backing::Sealed { reused: true, .. } => SnapshotMode::SharedHit,
            #[cfg(target_os = "linux")]
            Backing::Sealed { reused: false, .. } => SnapshotMode::SealedCreated,
        }
    }

    /// Whether bytes came from a hint and may need a source retry on corruption.
    pub(super) fn is_shared_hit(&self) -> bool {
        self.mode() == SnapshotMode::SharedHit
    }

    /// Whether this backing can share physical pages across backends.
    pub(super) fn is_shareable(&self) -> bool {
        let mode = self.mode();
        mode == SnapshotMode::SharedHit || mode == SnapshotMode::SealedCreated
    }

    /// Advertises this backend's descriptor after all artifact validation.
    ///
    /// Registry failure does not invalidate the already owned immutable bytes.
    pub(super) fn advertise(&self) -> io::Result<()> {
        match &self.backing {
            Backing::Private => Ok(()),
            #[cfg(target_os = "linux")]
            Backing::Sealed { snapshot, .. } => snapshot.advertise(),
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    fn advertise_with_access(&self, access: &impl linux::DescriptorAccess) -> io::Result<()> {
        match &self.backing {
            Backing::Private => Ok(()),
            Backing::Sealed { snapshot, .. } => snapshot.advertise_with_access(access),
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    fn sealed_file(&self) -> Option<&File> {
        match &self.backing {
            Backing::Private => None,
            Backing::Sealed { snapshot, .. } => Some(&snapshot.owner.file),
        }
    }
}

/// Acquires sealed cached bytes or copies the source into immutable storage.
///
/// `key` must identify the canonical source path and exact source stamp. The
/// caller bounds `expected_len` with its operation memory budget. Source read
/// errors propagate; cache and sealing failures select a private snapshot.
pub(super) fn load_snapshot(
    source: &mut File,
    cache_dir: &Path,
    key: &str,
    expected_len: usize,
) -> io::Result<Snapshot> {
    #[cfg(target_os = "linux")]
    {
        linux::load_snapshot_with_access(source, cache_dir, key, expected_len, &linux::ProcFs)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (cache_dir, key);
        copy_private_snapshot(source, expected_len)
    }
}

/// Copies from the beginning of the source, bypassing descriptor hints.
pub(super) fn copy_private_snapshot(
    source: &mut File,
    expected_len: usize,
) -> io::Result<Snapshot> {
    source.seek(SeekFrom::Start(0))?;
    let mut mapping = MmapMut::map_anon(expected_len)?;
    source.read_exact(&mut mapping)?;
    Ok(Snapshot {
        mmap: Arc::new(mapping.make_read_only()?),
        backing: Backing::Private,
    })
}

#[cfg(all(target_os = "linux", not(test), feature = "development"))]
thread_local! {
    static CANCEL_POINT: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[cfg(all(target_os = "linux", not(test), feature = "development"))]
pub(super) fn arm_test_cancel(registry: bool) {
    CANCEL_POINT.with(|point| point.set(Some(registry)));
}

#[cfg(target_os = "linux")]
fn test_cancel_checkpoint(registry: bool) {
    #[cfg(all(not(test), feature = "development"))]
    if CANCEL_POINT.with(|point| {
        if point.get() == Some(registry) {
            point.set(None);
            true
        } else {
            false
        }
    }) {
        if let Err(error) =
            pgrx::Spi::run("SELECT pg_cancel_backend(pg_backend_pid()), pg_sleep(10)")
        {
            pgrx::error!("snapshot cancellation injection failed: {error}");
        }
    }
    #[cfg(any(test, not(feature = "development")))]
    let _ = registry;
}

/// Publishes arbitrary sealed fixture bytes to exercise the caller's validator.
#[cfg(all(test, target_os = "linux"))]
pub(super) fn advertise_test_snapshot(
    source: &mut File,
    cache_dir: &Path,
    key: &str,
    expected_len: usize,
) -> io::Result<Snapshot> {
    let snapshot =
        linux::copy_sealed(source, cache_dir, key, expected_len).map_err(|error| match error {
            linux::CopyError::Source(error) => error,
            linux::CopyError::Cache => io::Error::other("cannot create sealed fixture"),
        })?;
    snapshot.advertise()?;
    Ok(snapshot)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use memmap2::MmapOptions;
    use serde::{Deserialize, Serialize};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    // This private on-disk protocol has one bounds/format definition here.
    pub(super) const MAX_KEY_BYTES: usize = 16 * 1024;
    const MAX_HINT_BYTES: u64 = 32 * 1024;
    const MAX_HINTS: usize = 128;
    const MAX_SCAN_ENTRIES: usize = MAX_HINTS * 2;
    const REQUIRED_SEALS: libc::c_int =
        libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;

    /// Descriptor discovery boundary, also used to simulate inaccessible procfs.
    pub(super) trait DescriptorAccess {
        fn open(&self, pid: u32, fd: i32) -> io::Result<File>;
    }

    pub(super) struct ProcFs;

    impl DescriptorAccess for ProcFs {
        fn open(&self, pid: u32, fd: i32) -> io::Result<File> {
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOCTTY)
                .open(format!("/proc/{pid}/fd/{fd}"))
        }
    }

    pub(super) fn load_snapshot_with_access(
        source: &mut File,
        directory: &Path,
        key: &str,
        bytes: usize,
        access: &impl DescriptorAccess,
    ) -> io::Result<Snapshot> {
        if key.len() <= MAX_KEY_BYTES {
            match lookup(directory, key, bytes, access) {
                Ok(Some(snapshot)) => return Ok(snapshot),
                Ok(None) => match copy_sealed_with_access(source, directory, key, bytes, access) {
                    Ok(snapshot) => return Ok(snapshot),
                    Err(CopyError::Source(error)) => return Err(error),
                    Err(CopyError::Cache) => {}
                },
                Err(_) => {}
            }
        }
        copy_private_snapshot(source, bytes)
    }

    #[derive(Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct Hint {
        version: u8,
        key: String,
        pub(super) pid: u32,
        fd: i32,
        device: u64,
        inode: u64,
        bytes: u64,
    }

    impl Hint {
        pub(super) fn new(key: &str, file: &File, expected_len: usize) -> io::Result<Self> {
            let metadata = file.metadata()?;
            Ok(Self {
                version: 1,
                key: key.to_owned(),
                pid: std::process::id(),
                fd: file.as_raw_fd(),
                device: metadata.dev(),
                inode: metadata.ino(),
                bytes: expected_len as u64,
            })
        }

        pub(super) fn file_name(&self) -> String {
            format!("{}-{}-{}.json", self.pid, self.fd, self.inode)
        }
    }

    /// Accounts for a retained descriptor in PostgreSQL's descriptor budget.
    #[derive(Debug)]
    struct ExternalDescriptor {
        #[cfg(not(test))]
        registered: bool,
        // PostgreSQL accounting must be released on its acquiring thread.
        _backend_thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    impl ExternalDescriptor {
        fn acquire() -> io::Result<Self> {
            #[cfg(not(test))]
            {
                // SAFETY: Persistence entry points execute on the PostgreSQL
                // backend thread. Standalone callers skip PostgreSQL state.
                let registered = unsafe { pgrx::pg_sys::IsUnderPostmaster };
                // SAFETY: This is the backend thread, and no descriptor has
                // been acquired yet. A false result leaves no reservation.
                if registered && !unsafe { pgrx::pg_sys::AcquireExternalFD() } {
                    return Err(io::Error::other("PostgreSQL descriptor budget exhausted"));
                }
                Ok(Self {
                    registered,
                    _backend_thread: std::marker::PhantomData,
                })
            }
            #[cfg(test)]
            Ok(Self {
                _backend_thread: std::marker::PhantomData,
            })
        }
    }

    impl Drop for ExternalDescriptor {
        fn drop(&mut self) {
            #[cfg(not(test))]
            if self.registered {
                // SAFETY: This backend-owned reservation is released exactly
                // once, after its File has closed, on the backend thread.
                unsafe { pgrx::pg_sys::ReleaseExternalFD() };
            }
        }
    }

    #[derive(Debug)]
    pub(super) struct SealedOwner {
        pub(super) file: File,
        // Fields drop in declaration order, closing the File first.
        _descriptor: ExternalDescriptor,
    }

    #[derive(Debug)]
    pub(super) struct SealedSnapshot {
        pub(super) owner: SealedOwner,
        directory: PathBuf,
        hint: Hint,
    }

    impl SealedSnapshot {
        fn map(self, reused: bool) -> io::Result<Snapshot> {
            verify_sealed(&self.owner.file, &self.hint)?;
            let bytes = usize::try_from(self.hint.bytes)
                .map_err(|_| io::Error::other("snapshot length exceeds this platform"))?;
            // SAFETY: The exact opened inode has irreversible write, grow and
            // shrink seals. Its checked size covers the mapping, no writable
            // shared mappings can exist, and Mmap retains the kernel backing.
            let mmap = unsafe { MmapOptions::new().len(bytes).map(&self.owner.file)? };
            Ok(Snapshot {
                mmap: Arc::new(mmap),
                backing: Backing::Sealed {
                    snapshot: self,
                    reused,
                },
            })
        }

        pub(super) fn advertise(&self) -> io::Result<()> {
            self.advertise_with_access(&ProcFs)
        }

        pub(super) fn advertise_with_access(
            &self,
            access: &impl DescriptorAccess,
        ) -> io::Result<()> {
            let lock = registry_lock(&self.directory)?;
            check_self_access(&lock, access)?;
            let paths = hint_paths(&self.directory)?;
            let own_path = self.directory.join(self.hint.file_name());
            let mut live = 0;
            for path in paths {
                crate::resource::check_postgres_interrupts();
                if inspect_hint(&path, access)?.is_some() {
                    if path == own_path {
                        return Ok(());
                    }
                    live += 1;
                } else {
                    prune_stale_hint(&path, &lock, access)?;
                }
            }
            if live >= MAX_HINTS {
                return Err(io::Error::other("snapshot hint registry is full"));
            }
            let encoded = serde_json::to_vec(&self.hint)?;
            if encoded.len() as u64 > MAX_HINT_BYTES {
                return Err(io::Error::other("snapshot hint exceeds its byte limit"));
            }
            // All readers hold the same lock. A crash during this bounded
            // write leaves a malformed hint that a later scan discards.
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&own_path)?;
            if let Err(error) = file.write_all(&encoded) {
                let _ = fs::remove_file(&own_path);
                return Err(error);
            }
            Ok(())
        }
    }

    impl Drop for SealedSnapshot {
        fn drop(&mut self) {
            // Our descriptor is still open here, so its numeric identity
            // cannot yet have been reused by another snapshot in this backend.
            let _ = fs::remove_file(self.directory.join(self.hint.file_name()));
        }
    }

    fn registry_lock(directory: &Path) -> io::Result<File> {
        fs::create_dir_all(directory)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join("registry.lock"))?;
        file.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
            fs::TryLockError::Error(error) => error,
        })?;
        Ok(file)
    }

    fn hint_paths(directory: &Path) -> io::Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for (index, entry) in fs::read_dir(directory)?.enumerate() {
            if index >= MAX_SCAN_ENTRIES {
                return Err(io::Error::other("snapshot directory scan limit exceeded"));
            }
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
                continue;
            };
            let mut parts = name.split('-');
            if parts
                .next()
                .and_then(|value| value.parse::<u32>().ok())
                .is_some()
                && parts
                    .next()
                    .and_then(|value| value.parse::<u32>().ok())
                    .is_some()
                && parts
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .is_some()
                && parts.next().is_none()
            {
                paths.push(entry.path());
            }
        }
        Ok(paths)
    }

    fn read_hint(path: &Path) -> io::Result<Hint> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_HINT_BYTES {
            return Err(invalid_hint("invalid snapshot hint file"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_HINT_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_HINT_BYTES {
            return Err(invalid_hint("snapshot hint exceeds its byte limit"));
        }
        let hint: Hint = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if hint.version != 1
            || hint.key.len() > MAX_KEY_BYTES
            || hint.pid == 0
            || hint.fd < 0
            || hint.bytes == 0
            || path.file_name().and_then(|name| name.to_str()) != Some(hint.file_name().as_str())
        {
            return Err(invalid_hint("invalid snapshot hint identity"));
        }
        Ok(hint)
    }

    fn verify_sealed(file: &File, hint: &Hint) -> io::Result<()> {
        // SAFETY: The descriptor is live and F_GET_SEALS takes no third argument.
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        if seals < 0 {
            let error = io::Error::last_os_error();
            return Err(if error.raw_os_error() == Some(libc::EINVAL) {
                invalid_hint("snapshot descriptor does not support seals")
            } else {
                error
            });
        }
        if seals & REQUIRED_SEALS != REQUIRED_SEALS {
            return Err(invalid_hint("snapshot descriptor is not fully sealed"));
        }
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.dev() != hint.device
            || metadata.ino() != hint.inode
            || metadata.len() != hint.bytes
            || metadata.len() == 0
        {
            return Err(invalid_hint("snapshot descriptor identity changed"));
        }
        Ok(())
    }

    fn reopen(hint: &Hint, access: &impl DescriptorAccess) -> io::Result<SealedOwner> {
        let descriptor = ExternalDescriptor::acquire()?;
        let file = access.open(hint.pid, hint.fd)?;
        verify_sealed(&file, hint)?;
        Ok(SealedOwner {
            file,
            _descriptor: descriptor,
        })
    }

    fn invalid_hint(reason: &'static str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, reason)
    }

    // Ok(Some) is usable, Ok(None) is definitely stale or invalid, and Err
    // means discovery is unavailable. Only the second outcome permits pruning.
    fn inspect_hint(
        path: &Path,
        access: &impl DescriptorAccess,
    ) -> io::Result<Option<(Hint, SealedOwner)>> {
        let hint = match read_hint(path) {
            Ok(hint) => hint,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidData | io::ErrorKind::NotFound
                ) || error.raw_os_error() == Some(libc::ELOOP) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        match reopen(&hint, access) {
            Ok(owner) => Ok(Some((hint, owner))),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidData | io::ErrorKind::NotFound
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn check_self_access(anchor: &File, access: &impl DescriptorAccess) -> io::Result<()> {
        let _descriptor = ExternalDescriptor::acquire()?;
        let reopened = access.open(std::process::id(), anchor.as_raw_fd())?;
        let expected = anchor.metadata()?;
        let actual = reopened.metadata()?;
        if expected.dev() != actual.dev() || expected.ino() != actual.ino() {
            return Err(io::Error::other(
                "procfs returned a different local descriptor",
            ));
        }
        Ok(())
    }

    fn prune_stale_hint(
        path: &Path,
        anchor: &File,
        access: &impl DescriptorAccess,
    ) -> io::Result<()> {
        // Procfs disappearing must not look like every provider has exited.
        // Recheck the live local descriptor immediately before deleting a hint.
        check_self_access(anchor, access)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn lookup(
        directory: &Path,
        key: &str,
        bytes: usize,
        access: &impl DescriptorAccess,
    ) -> io::Result<Option<Snapshot>> {
        let lock = registry_lock(directory)?;
        check_self_access(&lock, access)?;
        test_cancel_checkpoint(true);
        let mut live = 0;
        for path in hint_paths(directory)? {
            crate::resource::check_postgres_interrupts();
            let Some((hint, owner)) = inspect_hint(&path, access)? else {
                prune_stale_hint(&path, &lock, access)?;
                continue;
            };
            if hint.key == key && hint.bytes == bytes as u64 {
                let hint = Hint::new(key, &owner.file, bytes)?;
                return SealedSnapshot {
                    owner,
                    directory: directory.to_owned(),
                    hint,
                }
                .map(true)
                .map(Some);
            }
            live += 1;
        }
        if live >= MAX_HINTS {
            return Err(io::Error::other("snapshot hint registry is full"));
        }
        Ok(None)
    }

    pub(super) enum CopyError {
        Source(io::Error),
        Cache,
    }

    #[cfg(test)]
    pub(super) fn copy_sealed(
        source: &mut File,
        directory: &Path,
        key: &str,
        bytes: usize,
    ) -> Result<Snapshot, CopyError> {
        copy_sealed_with_access(source, directory, key, bytes, &ProcFs)
    }

    fn copy_sealed_with_access(
        source: &mut File,
        directory: &Path,
        key: &str,
        bytes: usize,
        access: &impl DescriptorAccess,
    ) -> Result<Snapshot, CopyError> {
        let descriptor = ExternalDescriptor::acquire().map_err(|_| CopyError::Cache)?;
        // SAFETY: The static C string is terminated and flags are supported
        // memfd options. A returned descriptor is immediately uniquely owned.
        let fd = unsafe {
            libc::memfd_create(
                c"pggraph-base".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(CopyError::Cache);
        }
        // SAFETY: Successful memfd_create returned a new, uniquely owned fd.
        let file = unsafe { File::from_raw_fd(fd) };
        let mut owner = SealedOwner {
            file,
            _descriptor: descriptor,
        };
        source.seek(SeekFrom::Start(0)).map_err(CopyError::Source)?;
        let mut buffer = [0_u8; 64 * 1024];
        let mut remaining = bytes;
        test_cancel_checkpoint(false);
        while remaining > 0 {
            crate::resource::check_postgres_interrupts();
            let length = remaining.min(buffer.len());
            source
                .read_exact(&mut buffer[..length])
                .map_err(CopyError::Source)?;
            owner
                .file
                .write_all(&buffer[..length])
                .map_err(|_| CopyError::Cache)?;
            remaining -= length;
        }
        // SAFETY: The live memfd has no writable mappings. These inode seals
        // prohibit all later content/size changes through any descriptor.
        if unsafe { libc::fcntl(owner.file.as_raw_fd(), libc::F_ADD_SEALS, REQUIRED_SEALS) } < 0 {
            return Err(CopyError::Cache);
        }
        let hint = Hint::new(key, &owner.file, bytes).map_err(|_| CopyError::Cache)?;
        // A sealed object is useful for this cache only if its descriptor can
        // actually be rediscovered. Failure rewinds into the private fallback.
        drop(reopen(&hint, access).map_err(|_| CopyError::Cache)?);
        SealedSnapshot {
            owner,
            directory: directory.to_owned(),
            hint,
        }
        .map(false)
        .map_err(|_| CopyError::Cache)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux::DescriptorAccess;
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{FileExt, MetadataExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        source: File,
    }

    impl Fixture {
        fn new() -> io::Result<Self> {
            let root = std::env::temp_dir().join(format!(
                "pggraph-snapshot-cache-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root)?;
            let source = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(root.join("source"))?;
            source.write_all_at(b"immutable graph bytes", 0)?;
            Ok(Self { root, source })
        }

        fn cache_dir(&self) -> PathBuf {
            self.root.join("cache")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    struct DenyProvider {
        fd: i32,
        errno: Option<i32>,
    }

    impl DescriptorAccess for DenyProvider {
        fn open(&self, pid: u32, fd: i32) -> io::Result<File> {
            if fd == self.fd {
                return Err(self.errno.map_or_else(
                    || io::Error::other("simulated descriptor budget exhaustion"),
                    io::Error::from_raw_os_error,
                ));
            }
            linux::ProcFs.open(pid, fd)
        }
    }

    struct MissingProc {
        successful_opens: std::cell::Cell<usize>,
    }

    impl DescriptorAccess for MissingProc {
        fn open(&self, pid: u32, fd: i32) -> io::Result<File> {
            let remaining = self.successful_opens.get();
            if remaining == 0 {
                return Err(io::Error::from_raw_os_error(libc::ENOENT));
            }
            self.successful_opens.set(remaining - 1);
            linux::ProcFs.open(pid, fd)
        }
    }

    struct DenyMemfd {
        denied: std::cell::Cell<usize>,
    }

    impl DescriptorAccess for DenyMemfd {
        fn open(&self, pid: u32, fd: i32) -> io::Result<File> {
            let target = fs::read_link(format!("/proc/{pid}/fd/{fd}"))?;
            if target.to_string_lossy().contains("memfd:") {
                self.denied.set(self.denied.get() + 1);
                return Err(io::Error::from_raw_os_error(libc::EACCES));
            }
            linux::ProcFs.open(pid, fd)
        }
    }

    #[test]
    fn unavailable_provider_preserves_hint_and_selects_private_fallback() -> io::Result<()> {
        let mut fixture = Fixture::new()?;
        let cache_dir = fixture.cache_dir();
        let provider = load_snapshot(&mut fixture.source, &cache_dir, "key", 21)?;
        provider.advertise()?;
        let hint = linux::Hint::new("key", provider.sealed_file().unwrap(), 21)?;
        let hint_path = cache_dir.join(hint.file_name());
        let original = fs::read(&hint_path)?;
        for errno in [
            Some(libc::EACCES),
            Some(libc::EPERM),
            Some(libc::EMFILE),
            Some(libc::ENFILE),
            None,
        ] {
            let access = DenyProvider {
                fd: provider.sealed_file().unwrap().as_raw_fd(),
                errno,
            };
            let snapshot = linux::load_snapshot_with_access(
                &mut fixture.source,
                &cache_dir,
                "key",
                21,
                &access,
            )?;
            assert_eq!(snapshot.mode(), SnapshotMode::PrivateFallback);
            assert_eq!(&snapshot.mmap()[..], b"immutable graph bytes");
            assert_eq!(fs::read(&hint_path)?, original);

            let adopter = load_snapshot(&mut fixture.source, &cache_dir, "key", 21)?;
            assert!(adopter.is_shared_hit());
            assert!(adopter.advertise_with_access(&access).is_err());
            assert_eq!(fs::read(&hint_path)?, original);
        }
        Ok(())
    }

    #[test]
    fn missing_proc_does_not_prune_any_provider_or_malformed_hint() -> io::Result<()> {
        let mut fixture = Fixture::new()?;
        let cache_dir = fixture.cache_dir();
        let provider = load_snapshot(&mut fixture.source, &cache_dir, "key", 21)?;
        provider.advertise()?;
        let hint = linux::Hint::new("key", provider.sealed_file().unwrap(), 21)?;
        let hint_path = cache_dir.join(hint.file_name());
        let original = fs::read(&hint_path)?;
        let malformed_path = cache_dir.join("1-2-3.json");
        fs::write(&malformed_path, b"{malformed")?;
        for successful_opens in [0, 1] {
            let access = MissingProc {
                successful_opens: std::cell::Cell::new(successful_opens),
            };
            let snapshot = linux::load_snapshot_with_access(
                &mut fixture.source,
                &cache_dir,
                "key",
                21,
                &access,
            )?;
            assert_eq!(snapshot.mode(), SnapshotMode::PrivateFallback);
            assert_eq!(&snapshot.mmap()[..], b"immutable graph bytes");
            assert_eq!(fs::read(&hint_path)?, original);
            assert_eq!(fs::read(&malformed_path)?, b"{malformed");
        }
        Ok(())
    }

    #[test]
    fn new_memfd_reopen_failure_rewinds_into_private_storage() -> io::Result<()> {
        let mut fixture = Fixture::new()?;
        let cache_dir = fixture.cache_dir();
        let access = DenyMemfd {
            denied: std::cell::Cell::new(0),
        };
        let snapshot =
            linux::load_snapshot_with_access(&mut fixture.source, &cache_dir, "key", 21, &access)?;
        assert_eq!(access.denied.get(), 1);
        assert_eq!(snapshot.mode(), SnapshotMode::PrivateFallback);
        assert_eq!(&snapshot.mmap()[..], b"immutable graph bytes");
        assert!(fs::read_dir(&cache_dir)?
            .all(|entry| entry.unwrap().path().extension() != Some(std::ffi::OsStr::new("json"))));

        fixture.source.set_len(20)?;
        let error =
            linux::load_snapshot_with_access(&mut fixture.source, &cache_dir, "key", 21, &access)
                .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(access.denied.get(), 1);
        Ok(())
    }

    #[test]
    fn advertised_snapshot_shares_sealed_bytes_and_retains_adopter() -> io::Result<()> {
        let mut fixture = Fixture::new()?;
        let cache_dir = fixture.cache_dir();
        let first = load_snapshot(&mut fixture.source, &cache_dir, "key", 21)?;
        assert_eq!(first.mode(), SnapshotMode::SealedCreated);
        first.advertise()?;
        let second = load_snapshot(&mut fixture.source, &cache_dir, "key", 21)?;
        assert!(second.is_shared_hit());
        let first_inode = first.sealed_file().unwrap().metadata()?.ino();
        assert_eq!(second.sealed_file().unwrap().metadata()?.ino(), first_inode);
        second.advertise()?;
        drop(first);
        let third = load_snapshot(&mut fixture.source, &cache_dir, "key", 21)?;
        assert!(third.is_shared_hit());
        assert_eq!(third.sealed_file().unwrap().metadata()?.ino(), first_inode);
        assert_eq!(&third.mmap()[..], b"immutable graph bytes");
        Ok(())
    }

    #[test]
    fn sealed_snapshot_rejects_writes_and_truncation() -> io::Result<()> {
        let mut fixture = Fixture::new()?;
        let cache_dir = fixture.cache_dir();
        let snapshot = load_snapshot(&mut fixture.source, &cache_dir, "key", 21)?;
        let file = snapshot.sealed_file().unwrap();
        assert_eq!(
            file.set_len(0).unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );
        assert_eq!(
            file.set_len(22).unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );
        assert_eq!(
            file.write_all_at(b"x", 0).unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );
        fixture.source.set_len(0)?;
        assert_eq!(&snapshot.mmap()[..], b"immutable graph bytes");
        Ok(())
    }

    #[test]
    fn unsealed_descriptor_hint_is_rejected() -> io::Result<()> {
        let mut fixture = Fixture::new()?;
        let cache_dir = fixture.cache_dir();
        fs::create_dir(&cache_dir)?;
        let hint = linux::Hint::new("key", &fixture.source, 21)?;
        fs::write(cache_dir.join(hint.file_name()), serde_json::to_vec(&hint)?)?;
        let snapshot = load_snapshot(&mut fixture.source, &cache_dir, "key", 21)?;
        assert_eq!(snapshot.mode(), SnapshotMode::SealedCreated);
        assert_ne!(
            snapshot.sealed_file().unwrap().as_raw_fd(),
            fixture.source.as_raw_fd()
        );
        assert_eq!(&snapshot.mmap()[..], b"immutable graph bytes");
        Ok(())
    }

    #[test]
    fn memfd_hint_without_write_seal_is_rejected() -> io::Result<()> {
        use std::os::fd::FromRawFd;
        let mut fixture = Fixture::new()?;
        let cache_dir = fixture.cache_dir();
        fs::create_dir(&cache_dir)?;
        // SAFETY: A terminated static name and valid creation flags are passed.
        let fd =
            unsafe { libc::memfd_create(c"unsealed-fixture".as_ptr(), libc::MFD_ALLOW_SEALING) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: The descriptor was newly created and is uniquely owned here.
        let mut writable = unsafe { File::from_raw_fd(fd) };
        writable.write_all(b"immutable graph bytes")?;
        // SAFETY: The descriptor is live, and no shared writable mapping exists.
        let status = unsafe {
            libc::fcntl(
                fd,
                libc::F_ADD_SEALS,
                libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL,
            )
        };
        assert_eq!(status, 0);
        let hint = linux::Hint::new("key", &writable, 21)?;
        fs::write(cache_dir.join(hint.file_name()), serde_json::to_vec(&hint)?)?;
        let snapshot = load_snapshot(&mut fixture.source, &cache_dir, "key", 21)?;
        assert_eq!(snapshot.mode(), SnapshotMode::SealedCreated);
        writable.write_all_at(b"X", 0)?;
        assert_eq!(&snapshot.mmap()[..], b"immutable graph bytes");
        Ok(())
    }

    #[test]
    fn busy_registry_selects_private_fallback() -> io::Result<()> {
        let mut fixture = Fixture::new()?;
        let cache_dir = fixture.cache_dir();
        fs::create_dir(&cache_dir)?;
        let lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(cache_dir.join("registry.lock"))?;
        lock.lock()?;
        let snapshot = load_snapshot(&mut fixture.source, &cache_dir, "key", 21)?;
        assert_eq!(snapshot.mode(), SnapshotMode::PrivateFallback);
        assert_eq!(&snapshot.mmap()[..], b"immutable graph bytes");
        Ok(())
    }

    #[test]
    fn malformed_and_stale_hints_do_not_authorize_mappings() -> io::Result<()> {
        let mut fixture = Fixture::new()?;
        let cache_dir = fixture.cache_dir();
        fs::create_dir(&cache_dir)?;
        fs::write(cache_dir.join("1-2-3.json"), b"{malformed")?;
        let mut hint = linux::Hint::new("key", &fixture.source, 21)?;
        hint.pid = u32::MAX;
        fs::write(cache_dir.join(hint.file_name()), serde_json::to_vec(&hint)?)?;
        let snapshot = load_snapshot(&mut fixture.source, &cache_dir, "key", 21)?;
        assert_eq!(snapshot.mode(), SnapshotMode::SealedCreated);
        assert_eq!(&snapshot.mmap()[..], b"immutable graph bytes");
        Ok(())
    }

    #[test]
    fn private_fallback_rewinds_source_and_preserves_read_errors() -> io::Result<()> {
        let mut fixture = Fixture::new()?;
        fixture.source.seek(SeekFrom::End(0))?;
        let invalid_cache = fixture.root.join("source");
        let snapshot = load_snapshot(&mut fixture.source, &invalid_cache, "key", 21)?;
        assert_eq!(snapshot.mode(), SnapshotMode::PrivateFallback);
        assert_eq!(&snapshot.mmap()[..], b"immutable graph bytes");
        let error = copy_private_snapshot(&mut fixture.source, 22).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        Ok(())
    }
}
