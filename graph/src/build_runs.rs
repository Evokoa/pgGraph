//! Governed, checksummed external-sort runs for persisted graph builds.
//!
//! Run files are private derived state. Their manual little-endian format is
//! bound to one graph, build, catalog fingerprint, and data kind. Readers
//! validate the complete payload checksum and sorted order before records can
//! influence a merge output.

use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::resource::{
    ByteCount, ResourceGovernor, ResourceLease, ResourceLimitError, ResourcePhase, RowCount,
    WorkUnits,
};
use crate::safety::{GraphError, GraphResult};

const RUN_MAGIC: [u8; 8] = *b"PGRUN001";
const RUN_VERSION: u16 = 1;
const RUN_HEADER_SIZE: usize = 128;
const RUN_BUFFER_SIZE: usize = 16 * 1024;
const HEADER_CRC_OFFSET: usize = 76;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum RunKind {
    Nodes = 1,
    RelationshipsOutbound = 2,
    RelationshipsInbound = 3,
    Filters = 4,
    FilterDictionary = 5,
    Resolution = 6,
}

impl RunKind {
    fn from_u8(value: u8) -> GraphResult<Self> {
        match value {
            1 => Ok(Self::Nodes),
            2 => Ok(Self::RelationshipsOutbound),
            3 => Ok(Self::RelationshipsInbound),
            4 => Ok(Self::Filters),
            5 => Ok(Self::FilterDictionary),
            6 => Ok(Self::Resolution),
            _ => Err(corrupt_run(format!("unknown run kind {value}"))),
        }
    }

    const fn file_label(self) -> &'static str {
        match self {
            Self::Nodes => "nodes",
            Self::RelationshipsOutbound => "relationships-out",
            Self::RelationshipsInbound => "relationships-in",
            Self::Filters => "filters",
            Self::FilterDictionary => "filter-dictionary",
            Self::Resolution => "resolution",
        }
    }
}

/// One lexicographically ordered external-sort record.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RunRecord {
    pub(crate) key: Vec<u8>,
    pub(crate) value: Vec<u8>,
}

impl RunRecord {
    pub(crate) fn new(key: Vec<u8>, value: Vec<u8>) -> Self {
        Self { key, value }
    }

    fn encoded_len(&self) -> GraphResult<u64> {
        let key = u64::try_from(self.key.len())
            .map_err(|_| GraphError::Internal("run key length exceeds u64".to_string()))?;
        let value = u64::try_from(self.value.len())
            .map_err(|_| GraphError::Internal("run value length exceeds u64".to_string()))?;
        8_u64
            .checked_add(key)
            .and_then(|bytes| bytes.checked_add(value))
            .ok_or_else(|| GraphError::Internal("run record length overflowed".to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunHeader {
    kind: RunKind,
    graph_id: [u8; 16],
    build_id: [u8; 16],
    catalog_fingerprint: u64,
    record_count: u64,
    payload_len: u64,
    payload_crc: u32,
}

struct RunWriteSpec {
    kind: RunKind,
    record_count: u64,
    payload_len: u64,
    max_record_bytes: usize,
    phase: ResourcePhase,
}

impl RunHeader {
    fn encode(self) -> [u8; RUN_HEADER_SIZE] {
        let mut bytes = [0_u8; RUN_HEADER_SIZE];
        bytes[0..8].copy_from_slice(&RUN_MAGIC);
        bytes[8..10].copy_from_slice(&RUN_VERSION.to_le_bytes());
        bytes[10] = self.kind as u8;
        bytes[12..16].copy_from_slice(&(RUN_HEADER_SIZE as u32).to_le_bytes());
        bytes[16..32].copy_from_slice(&self.graph_id);
        bytes[32..48].copy_from_slice(&self.build_id);
        bytes[48..56].copy_from_slice(&self.catalog_fingerprint.to_le_bytes());
        bytes[56..64].copy_from_slice(&self.record_count.to_le_bytes());
        bytes[64..72].copy_from_slice(&self.payload_len.to_le_bytes());
        bytes[72..76].copy_from_slice(&self.payload_crc.to_le_bytes());
        let crc = crc32fast::hash(&bytes);
        bytes[HEADER_CRC_OFFSET..HEADER_CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        bytes
    }

    fn decode(mut bytes: [u8; RUN_HEADER_SIZE]) -> GraphResult<Self> {
        if bytes[0..8] != RUN_MAGIC {
            return Err(corrupt_run("invalid run magic"));
        }
        if read_u16(&bytes, 8)? != RUN_VERSION {
            return Err(corrupt_run("unsupported run version"));
        }
        if read_u32(&bytes, 12)? as usize != RUN_HEADER_SIZE {
            return Err(corrupt_run("invalid run header length"));
        }
        if bytes[11] != 0 || bytes[80..].iter().any(|byte| *byte != 0) {
            return Err(corrupt_run("nonzero reserved run header bytes"));
        }
        let expected_crc = read_u32(&bytes, HEADER_CRC_OFFSET)?;
        bytes[HEADER_CRC_OFFSET..HEADER_CRC_OFFSET + 4].fill(0);
        if crc32fast::hash(&bytes) != expected_crc {
            return Err(corrupt_run("run header checksum mismatch"));
        }
        let mut graph_id = [0_u8; 16];
        graph_id.copy_from_slice(&bytes[16..32]);
        let mut build_id = [0_u8; 16];
        build_id.copy_from_slice(&bytes[32..48]);
        Ok(Self {
            kind: RunKind::from_u8(bytes[10])?,
            graph_id,
            build_id,
            catalog_fingerprint: read_u64(&bytes, 48)?,
            record_count: read_u64(&bytes, 56)?,
            payload_len: read_u64(&bytes, 64)?,
            payload_crc: read_u32(&bytes, 72)?,
        })
    }

    fn same_build_as(self, other: Self) -> bool {
        self.kind == other.kind
            && self.graph_id == other.graph_id
            && self.build_id == other.build_id
            && self.catalog_fingerprint == other.catalog_fingerprint
    }
}

struct FileSlot {
    active: Rc<Cell<usize>>,
}

impl Drop for FileSlot {
    fn drop(&mut self) {
        self.active.set(self.active.get().saturating_sub(1));
    }
}

/// Private build workspace with a hard live-file limit.
pub(crate) struct RunWorkspace {
    path: PathBuf,
    graph_id: [u8; 16],
    build_id: [u8; 16],
    catalog_fingerprint: u64,
    max_files: usize,
    active_files: Rc<Cell<usize>>,
    next_file: Cell<u64>,
}

impl RunWorkspace {
    pub(crate) fn create(
        root: &Path,
        graph_id: [u8; 16],
        build_id: [u8; 16],
        catalog_fingerprint: u64,
        max_files: usize,
    ) -> GraphResult<Self> {
        if max_files == 0 {
            return Err(GraphError::Internal(
                "build run file limit must be positive".to_string(),
            ));
        }
        let path = root.join(format!("pggraph-build-{}", hex_id(build_id)));
        fs::create_dir(&path).map_err(|error| {
            GraphError::Internal(format!("could not create build run workspace: {error}"))
        })?;
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            GraphError::Internal(format!("could not secure build run workspace: {error}"))
        })?;
        Ok(Self {
            path,
            graph_id,
            build_id,
            catalog_fingerprint,
            max_files,
            active_files: Rc::new(Cell::new(0)),
            next_file: Cell::new(0),
        })
    }

    fn allocate_file(&self, kind: RunKind) -> GraphResult<(PathBuf, FileSlot)> {
        let active = self.active_files.get();
        if active >= self.max_files {
            return Err(resource_limit(
                "files",
                ResourcePhase::BuildRunWrite,
                active as u64,
                1,
                self.max_files as u64,
            ));
        }
        self.active_files.set(active + 1);
        let id = self.next_file.get();
        self.next_file.set(id.checked_add(1).ok_or_else(|| {
            GraphError::Internal("build run file sequence overflowed".to_string())
        })?);
        Ok((
            self.path.join(format!("{}-{id:08}.run", kind.file_label())),
            FileSlot {
                active: Rc::clone(&self.active_files),
            },
        ))
    }

    fn write_run<'a, I>(
        &self,
        governor: &'a ResourceGovernor,
        spec: RunWriteSpec,
        records: I,
    ) -> GraphResult<StagedRun<'a>>
    where
        I: IntoIterator<Item = GraphResult<RunRecord>>,
    {
        let total_len = (RUN_HEADER_SIZE as u64)
            .checked_add(spec.payload_len)
            .ok_or_else(|| GraphError::Internal("run file length overflowed".to_string()))?;
        let disk = governor
            .reserve_disk(spec.phase, ByteCount::from_bytes(total_len))
            .map_err(resource_error)?;
        let (path, slot) = self.allocate_file(spec.kind)?;
        let result = write_run_file(
            &path,
            RunHeader {
                kind: spec.kind,
                graph_id: self.graph_id,
                build_id: self.build_id,
                catalog_fingerprint: self.catalog_fingerprint,
                record_count: spec.record_count,
                payload_len: spec.payload_len,
                payload_crc: 0,
            },
            records,
            spec.max_record_bytes,
            governor,
            spec.phase,
        );
        match result {
            Ok(header) => Ok(StagedRun {
                path,
                header,
                disk,
                _slot: slot,
            }),
            Err(error) => {
                let _ = fs::remove_file(&path);
                Err(error)
            }
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Remove well-formed abandoned workspaces while the graph build lock is held.
    ///
    /// The caller must hold the graph-scoped build lock. The current build and
    /// unrelated directories are never removed.
    pub(crate) fn cleanup_abandoned_while_build_locked(
        root: &Path,
        current_build_id: [u8; 16],
    ) -> GraphResult<usize> {
        let current = format!("pggraph-build-{}", hex_id(current_build_id));
        let mut removed = 0_usize;
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => {
                return Err(GraphError::Internal(format!(
                    "could not inspect build run workspaces: {error}"
                )))
            }
        };
        for entry in entries {
            let entry = entry.map_err(|error| {
                GraphError::Internal(format!("could not inspect build run workspace: {error}"))
            })?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == current || !is_build_workspace_name(&name) {
                continue;
            }
            let file_type = entry.file_type().map_err(|error| {
                GraphError::Internal(format!("could not inspect build run path: {error}"))
            })?;
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            fs::remove_dir_all(entry.path()).map_err(|error| {
                GraphError::Internal(format!("could not remove abandoned build run: {error}"))
            })?;
            removed = removed.checked_add(1).ok_or_else(|| {
                GraphError::Internal("abandoned workspace count overflowed".to_string())
            })?;
        }
        Ok(removed)
    }
}

impl Drop for RunWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Validated run file that retains its disk and file-count reservations.
pub(crate) struct StagedRun<'a> {
    path: PathBuf,
    header: RunHeader,
    disk: crate::resource::DiskLease<'a>,
    _slot: FileSlot,
}

impl StagedRun<'_> {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn record_count(&self) -> u64 {
        self.header.record_count
    }
}

impl Drop for StagedRun<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        self.disk.release_all();
    }
}

/// Byte-bounded run collector used by source scanners.
pub(crate) struct RunCollector<'workspace, 'governor> {
    workspace: &'workspace RunWorkspace,
    governor: &'governor ResourceGovernor,
    kind: RunKind,
    target_bytes: u64,
    max_record_bytes: usize,
    records: Vec<RunRecord>,
    payload_len: u64,
    memory: ResourceLease<'governor>,
    runs: Vec<StagedRun<'governor>>,
}

impl<'workspace, 'governor> RunCollector<'workspace, 'governor> {
    pub(crate) fn new(
        workspace: &'workspace RunWorkspace,
        governor: &'governor ResourceGovernor,
        kind: RunKind,
        target_bytes: ByteCount,
        max_record_bytes: usize,
    ) -> GraphResult<Self> {
        if target_bytes == ByteCount::ZERO || max_record_bytes < 8 {
            return Err(GraphError::Internal(
                "run collector limits must be positive".to_string(),
            ));
        }
        Ok(Self {
            workspace,
            governor,
            kind,
            target_bytes: target_bytes.as_u64(),
            max_record_bytes,
            records: Vec::new(),
            payload_len: 0,
            memory: governor
                .reserve_memory(ResourcePhase::BuildRunWrite, ByteCount::ZERO)
                .map_err(resource_error)?,
            runs: Vec::new(),
        })
    }

    pub(crate) fn push(&mut self, record: RunRecord) -> GraphResult<()> {
        let encoded = record.encoded_len()?;
        if encoded > self.max_record_bytes as u64 {
            return Err(corrupt_run("run record exceeds configured maximum"));
        }
        self.governor
            .consume_rows(ResourcePhase::BuildRunWrite, RowCount::new(1))
            .map_err(resource_error)?;
        if self.payload_len != 0
            && self
                .payload_len
                .checked_add(encoded)
                .is_none_or(|next| next > self.target_bytes)
        {
            self.flush()?;
        }
        let memory = encoded
            .checked_add(std::mem::size_of::<RunRecord>() as u64)
            .ok_or_else(|| GraphError::Internal("run collector accounting overflow".to_string()))?;
        self.memory
            .try_grow(ByteCount::from_bytes(memory))
            .map_err(resource_error)?;
        self.records.try_reserve(1).map_err(|error| {
            GraphError::Internal(format!("run collector allocation failed: {error}"))
        })?;
        self.records.push(record);
        self.payload_len = self
            .payload_len
            .checked_add(encoded)
            .ok_or_else(|| GraphError::Internal("run payload length overflowed".to_string()))?;
        Ok(())
    }

    pub(crate) fn flush(&mut self) -> GraphResult<()> {
        if self.records.is_empty() {
            return Ok(());
        }
        self.records.sort_unstable();
        let records = std::mem::take(&mut self.records);
        let count = records.len() as u64;
        let payload_len = std::mem::take(&mut self.payload_len);
        let run = self.workspace.write_run(
            self.governor,
            RunWriteSpec {
                kind: self.kind,
                record_count: count,
                payload_len,
                max_record_bytes: self.max_record_bytes,
                phase: ResourcePhase::BuildRunWrite,
            },
            records.into_iter().map(Ok),
        )?;
        self.memory.release_all();
        self.runs.push(run);
        Ok(())
    }

    pub(crate) fn finish(mut self) -> GraphResult<Vec<StagedRun<'governor>>> {
        self.flush()?;
        Ok(self.runs)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MergeStats {
    pub(crate) passes: usize,
    pub(crate) runs_written: usize,
}

pub(crate) fn merge_runs<'a>(
    workspace: &RunWorkspace,
    governor: &'a ResourceGovernor,
    mut runs: Vec<StagedRun<'a>>,
    fanout: usize,
    max_record_bytes: usize,
) -> GraphResult<(StagedRun<'a>, MergeStats)> {
    if runs.is_empty() {
        return Err(GraphError::Internal(
            "cannot merge an empty run set".to_string(),
        ));
    }
    if fanout < 2 {
        return Err(GraphError::Internal(
            "run merge fanout must be at least two".to_string(),
        ));
    }
    let mut stats = MergeStats::default();
    while runs.len() > 1 {
        governor
            .check_elapsed(ResourcePhase::BuildRunMerge)
            .map_err(resource_error)?;
        crate::resource::check_postgres_interrupts();
        stats.passes += 1;
        let mut pending = runs.into_iter();
        let mut next = Vec::new();
        loop {
            let group = pending.by_ref().take(fanout).collect::<Vec<_>>();
            if group.is_empty() {
                break;
            }
            if group.len() == 1 {
                next.extend(group);
            } else {
                next.push(merge_group(workspace, governor, group, max_record_bytes)?);
                stats.runs_written += 1;
            }
        }
        runs = next;
    }
    let run = runs
        .pop()
        .ok_or_else(|| GraphError::Internal("merged run disappeared".to_string()))?;
    Ok((run, stats))
}

fn merge_group<'a>(
    workspace: &RunWorkspace,
    governor: &'a ResourceGovernor,
    inputs: Vec<StagedRun<'a>>,
    max_record_bytes: usize,
) -> GraphResult<StagedRun<'a>> {
    let first = inputs[0].header;
    if inputs.iter().any(|run| !run.header.same_build_as(first)) {
        return Err(corrupt_run("merge input belongs to another build or kind"));
    }
    let record_count = inputs.iter().try_fold(0_u64, |count, run| {
        count
            .checked_add(run.header.record_count)
            .ok_or_else(|| GraphError::Internal("merged record count overflowed".to_string()))
    })?;
    let payload_len = inputs.iter().try_fold(0_u64, |bytes, run| {
        bytes
            .checked_add(run.header.payload_len)
            .ok_or_else(|| GraphError::Internal("merged payload length overflowed".to_string()))
    })?;
    let per_reader = (RUN_BUFFER_SIZE as u64)
        .checked_add((max_record_bytes as u64).saturating_mul(2))
        .ok_or_else(|| GraphError::Internal("merge memory estimate overflowed".to_string()))?;
    let merge_bytes = per_reader
        .checked_mul(inputs.len() as u64)
        .ok_or_else(|| GraphError::Internal("merge memory estimate overflowed".to_string()))?;
    let _memory = governor
        .reserve_memory(
            ResourcePhase::BuildRunMerge,
            ByteCount::from_bytes(merge_bytes),
        )
        .map_err(resource_error)?;
    governor
        .consume_work(ResourcePhase::BuildRunMerge, WorkUnits::new(record_count))
        .map_err(resource_error)?;
    let iterator = MergeIterator::open(&inputs, max_record_bytes)?;
    let output = workspace.write_run(
        governor,
        RunWriteSpec {
            kind: first.kind,
            record_count,
            payload_len,
            max_record_bytes,
            phase: ResourcePhase::BuildRunMerge,
        },
        iterator,
    )?;
    drop(inputs);
    Ok(output)
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct HeapRecord {
    record: RunRecord,
    source: usize,
}

struct MergeIterator {
    readers: Vec<RunReader>,
    heap: BinaryHeap<Reverse<HeapRecord>>,
}

impl MergeIterator {
    fn open(inputs: &[StagedRun<'_>], max_record_bytes: usize) -> GraphResult<Self> {
        let mut readers = inputs
            .iter()
            .map(|run| RunReader::open(&run.path, Some(run.header), max_record_bytes))
            .collect::<GraphResult<Vec<_>>>()?;
        let mut heap = BinaryHeap::new();
        for (source, reader) in readers.iter_mut().enumerate() {
            if let Some(record) = reader.next_record()? {
                heap.push(Reverse(HeapRecord { record, source }));
            }
        }
        Ok(Self { readers, heap })
    }
}

impl Iterator for MergeIterator {
    type Item = GraphResult<RunRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        let Reverse(entry) = self.heap.pop()?;
        match self.readers[entry.source].next_record() {
            Ok(Some(record)) => self.heap.push(Reverse(HeapRecord {
                record,
                source: entry.source,
            })),
            Ok(None) => {}
            Err(error) => return Some(Err(error)),
        }
        Some(Ok(entry.record))
    }
}

struct RunReader {
    reader: BufReader<File>,
    header: RunHeader,
    remaining: u64,
    payload_read: u64,
    previous: Option<RunRecord>,
    max_record_bytes: usize,
}

impl RunReader {
    fn open(
        path: &Path,
        expected: Option<RunHeader>,
        max_record_bytes: usize,
    ) -> GraphResult<Self> {
        let mut file = File::open(path)
            .map_err(|error| GraphError::Internal(format!("could not open build run: {error}")))?;
        let mut header_bytes = [0_u8; RUN_HEADER_SIZE];
        file.read_exact(&mut header_bytes)
            .map_err(|_| corrupt_run("truncated run header"))?;
        let header = RunHeader::decode(header_bytes)?;
        if expected.is_some_and(|expected| expected != header) {
            return Err(corrupt_run("run header changed after staging"));
        }
        let expected_len = (RUN_HEADER_SIZE as u64)
            .checked_add(header.payload_len)
            .ok_or_else(|| corrupt_run("run file length overflowed"))?;
        if file.metadata().map(|metadata| metadata.len()).unwrap_or(0) != expected_len {
            return Err(corrupt_run("run file length does not match header"));
        }
        let mut hasher = crc32fast::Hasher::new();
        let mut remaining = header.payload_len;
        let mut buffer = [0_u8; RUN_BUFFER_SIZE];
        while remaining != 0 {
            let take = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| corrupt_run("run checksum length overflowed"))?;
            file.read_exact(&mut buffer[..take])
                .map_err(|_| corrupt_run("truncated run payload"))?;
            hasher.update(&buffer[..take]);
            remaining -= take as u64;
        }
        if hasher.finalize() != header.payload_crc {
            return Err(corrupt_run("run payload checksum mismatch"));
        }
        file.seek(SeekFrom::Start(RUN_HEADER_SIZE as u64))
            .map_err(|error| {
                GraphError::Internal(format!("could not rewind build run: {error}"))
            })?;
        Ok(Self {
            reader: BufReader::with_capacity(RUN_BUFFER_SIZE, file),
            header,
            remaining: header.record_count,
            payload_read: 0,
            previous: None,
            max_record_bytes,
        })
    }

    fn next_record(&mut self) -> GraphResult<Option<RunRecord>> {
        if self.remaining == 0 {
            if self.payload_read != self.header.payload_len {
                return Err(corrupt_run("run record count does not consume payload"));
            }
            return Ok(None);
        }
        let mut lengths = [0_u8; 8];
        self.reader
            .read_exact(&mut lengths)
            .map_err(|_| corrupt_run("truncated run record lengths"))?;
        let key_len = read_u32(&lengths, 0)? as usize;
        let value_len = read_u32(&lengths, 4)? as usize;
        let encoded = 8_usize
            .checked_add(key_len)
            .and_then(|bytes| bytes.checked_add(value_len))
            .ok_or_else(|| corrupt_run("run record length overflowed"))?;
        if encoded > self.max_record_bytes {
            return Err(corrupt_run("run record exceeds configured maximum"));
        }
        let next_payload = self
            .payload_read
            .checked_add(encoded as u64)
            .ok_or_else(|| corrupt_run("run payload position overflowed"))?;
        if next_payload > self.header.payload_len {
            return Err(corrupt_run("run record exceeds payload boundary"));
        }
        let mut key = vec![0_u8; key_len];
        let mut value = vec![0_u8; value_len];
        self.reader
            .read_exact(&mut key)
            .and_then(|_| self.reader.read_exact(&mut value))
            .map_err(|_| corrupt_run("truncated run record"))?;
        let record = RunRecord { key, value };
        if self.previous.as_ref().is_some_and(|prior| prior > &record) {
            return Err(corrupt_run("run records are not sorted"));
        }
        self.previous = Some(record.clone());
        self.remaining -= 1;
        self.payload_read = next_payload;
        Ok(Some(record))
    }
}

fn write_run_file<I>(
    path: &Path,
    mut header: RunHeader,
    records: I,
    max_record_bytes: usize,
    governor: &ResourceGovernor,
    phase: ResourcePhase,
) -> GraphResult<RunHeader>
where
    I: IntoIterator<Item = GraphResult<RunRecord>>,
{
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path).map_err(|error| {
        GraphError::Internal(format!("could not create build run file: {error}"))
    })?;
    let mut writer = BufWriter::with_capacity(RUN_BUFFER_SIZE, file);
    writer
        .write_all(&[0_u8; RUN_HEADER_SIZE])
        .map_err(|error| GraphError::Internal(format!("could not stage run header: {error}")))?;
    let mut hasher = crc32fast::Hasher::new();
    let mut written_records = 0_u64;
    let mut written_payload = 0_u64;
    let mut previous: Option<RunRecord> = None;
    for record in records {
        if written_records.is_multiple_of(1024) {
            governor.check_elapsed(phase).map_err(resource_error)?;
            crate::resource::check_postgres_interrupts();
        }
        let record = record?;
        if previous.as_ref().is_some_and(|prior| prior > &record) {
            return Err(corrupt_run("run writer received unsorted records"));
        }
        let encoded = record.encoded_len()?;
        if encoded > max_record_bytes as u64 {
            return Err(corrupt_run("run record exceeds configured maximum"));
        }
        let key_len =
            u32::try_from(record.key.len()).map_err(|_| corrupt_run("run key exceeds u32"))?;
        let value_len =
            u32::try_from(record.value.len()).map_err(|_| corrupt_run("run value exceeds u32"))?;
        let lengths = [key_len.to_le_bytes(), value_len.to_le_bytes()].concat();
        for bytes in [&lengths[..], &record.key, &record.value] {
            writer.write_all(bytes).map_err(|error| {
                GraphError::Internal(format!("could not write build run: {error}"))
            })?;
            hasher.update(bytes);
        }
        written_records = written_records
            .checked_add(1)
            .ok_or_else(|| GraphError::Internal("run record count overflowed".to_string()))?;
        written_payload = written_payload
            .checked_add(encoded)
            .ok_or_else(|| GraphError::Internal("run payload length overflowed".to_string()))?;
        previous = Some(record);
    }
    if written_records != header.record_count || written_payload != header.payload_len {
        return Err(corrupt_run("run summary does not match written records"));
    }
    header.payload_crc = hasher.finalize();
    writer.flush().map_err(|error| {
        GraphError::Internal(format!("could not flush build run payload: {error}"))
    })?;
    let mut file = writer.into_inner().map_err(|error| {
        GraphError::Internal(format!("could not finalize build run buffer: {error}"))
    })?;
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(&header.encode()))
        .and_then(|_| file.sync_all())
        .map_err(|error| GraphError::Internal(format!("could not fsync build run: {error}")))?;
    RunReader::open(path, Some(header), max_record_bytes)?;
    Ok(header)
}

fn read_u16(bytes: &[u8], offset: usize) -> GraphResult<u16> {
    bytes
        .get(offset..offset + 2)
        .and_then(|slice| slice.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| corrupt_run("truncated u16 field"))
}

fn read_u32(bytes: &[u8], offset: usize) -> GraphResult<u32> {
    bytes
        .get(offset..offset + 4)
        .and_then(|slice| slice.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| corrupt_run("truncated u32 field"))
}

fn read_u64(bytes: &[u8], offset: usize) -> GraphResult<u64> {
    bytes
        .get(offset..offset + 8)
        .and_then(|slice| slice.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| corrupt_run("truncated u64 field"))
}

fn hex_id(id: [u8; 16]) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_build_workspace_name(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("pggraph-build-") else {
        return false;
    };
    suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn corrupt_run(reason: impl Into<String>) -> GraphError {
    GraphError::CorruptFile {
        reason: format!("build run: {}", reason.into()),
    }
}

fn resource_error(error: ResourceLimitError) -> GraphError {
    resource_limit(
        error.kind().as_str(),
        error.phase(),
        error.used(),
        error.requested(),
        error.limit(),
    )
}

fn resource_limit(
    resource: &str,
    phase: ResourcePhase,
    used: u64,
    requested: u64,
    limit: u64,
) -> GraphError {
    GraphError::ResourceLimit {
        resource: resource.to_string(),
        phase: phase.as_str().to_string(),
        used,
        requested,
        limit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{
        DiskBudget, ElapsedBudget, MemoryBudget, ResourceLimits, RowCount, WorkUnits,
    };
    use std::time::Duration;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "pggraph-runs-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn governor(memory: u64, disk: u64) -> ResourceGovernor {
        ResourceGovernor::new(ResourceLimits::bounded(
            MemoryBudget::new(ByteCount::from_bytes(memory)),
            DiskBudget::new(ByteCount::from_bytes(disk)),
            RowCount::UNLIMITED,
            WorkUnits::UNLIMITED,
            ElapsedBudget::new(Duration::MAX),
        ))
    }

    fn workspace(root: &Path, max_files: usize) -> RunWorkspace {
        RunWorkspace::create(root, [1; 16], [2; 16], 42, max_files).unwrap()
    }

    #[test]
    fn tiny_runs_merge_multiple_passes_and_preserve_duplicates() {
        let root = temp_root("multi-pass");
        let governor = governor(1_000_000, 1_000_000);
        let workspace = workspace(&root, 32);
        let mut collector = RunCollector::new(
            &workspace,
            &governor,
            RunKind::Nodes,
            ByteCount::from_bytes(10),
            64,
        )
        .unwrap();
        for key in [b"d", b"a", b"c", b"a", b"f", b"e", b"b", b"h", b"g"] {
            collector
                .push(RunRecord::new(key.to_vec(), vec![key[0]]))
                .unwrap();
        }
        let runs = collector.finish().unwrap();
        assert_eq!(runs.len(), 9);
        let (merged, stats) = merge_runs(&workspace, &governor, runs, 2, 64).unwrap();
        assert!(stats.passes >= 4);
        assert_eq!(merged.record_count(), 9);
        let mut reader = RunReader::open(merged.path(), Some(merged.header), 64).unwrap();
        let mut keys = Vec::new();
        while let Some(record) = reader.next_record().unwrap() {
            keys.push(record.key);
        }
        assert_eq!(keys, [b"a", b"a", b"b", b"c", b"d", b"e", b"f", b"g", b"h"]);
        drop(merged);
        drop(workspace);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn payload_corruption_is_rejected_before_records_are_read() {
        let root = temp_root("corrupt");
        let governor = governor(100_000, 100_000);
        let workspace = workspace(&root, 4);
        let mut collector = RunCollector::new(
            &workspace,
            &governor,
            RunKind::Resolution,
            ByteCount::from_bytes(1024),
            128,
        )
        .unwrap();
        collector
            .push(RunRecord::new(b"key".to_vec(), b"value".to_vec()))
            .unwrap();
        let run = collector.finish().unwrap().pop().unwrap();
        let mut file = OpenOptions::new().write(true).open(run.path()).unwrap();
        file.seek(SeekFrom::End(-1)).unwrap();
        file.write_all(&[0xff]).unwrap();
        file.sync_all().unwrap();
        assert!(matches!(
            RunReader::open(run.path(), Some(run.header), 128),
            Err(GraphError::CorruptFile { .. })
        ));
    }

    #[test]
    fn collector_rejects_oversized_records_before_disk_write() {
        let root = temp_root("oversized");
        let governor = governor(100_000, 100_000);
        let workspace = workspace(&root, 4);
        let mut collector = RunCollector::new(
            &workspace,
            &governor,
            RunKind::Filters,
            ByteCount::from_bytes(1024),
            16,
        )
        .unwrap();
        assert!(matches!(
            collector.push(RunRecord::new(vec![0; 9], vec![0; 1])),
            Err(GraphError::CorruptFile { .. })
        ));
        assert_eq!(workspace.active_files.get(), 0);
    }

    #[test]
    fn disk_and_file_limits_fail_before_creating_unaccounted_runs() {
        let root = temp_root("limits");
        let disk_limited = governor(100_000, 100);
        let workspace = workspace(&root, 1);
        let mut collector = RunCollector::new(
            &workspace,
            &disk_limited,
            RunKind::Nodes,
            ByteCount::from_bytes(1024),
            64,
        )
        .unwrap();
        collector
            .push(RunRecord::new(b"a".to_vec(), b"b".to_vec()))
            .unwrap();
        assert!(matches!(
            collector.finish(),
            Err(GraphError::ResourceLimit { .. })
        ));
        assert_eq!(workspace.active_files.get(), 0);

        let file_limited = governor(100_000, 100_000);
        let mut first = RunCollector::new(
            &workspace,
            &file_limited,
            RunKind::Nodes,
            ByteCount::from_bytes(9),
            64,
        )
        .unwrap();
        first
            .push(RunRecord::new(b"a".to_vec(), Vec::new()))
            .unwrap();
        first
            .push(RunRecord::new(b"b".to_vec(), Vec::new()))
            .unwrap();
        assert!(matches!(
            first.finish(),
            Err(GraphError::ResourceLimit { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn workspace_and_run_permissions_are_private() {
        let root = temp_root("permissions");
        let governor = governor(100_000, 100_000);
        let workspace = workspace(&root, 2);
        assert_eq!(
            fs::metadata(workspace.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let mut collector = RunCollector::new(
            &workspace,
            &governor,
            RunKind::FilterDictionary,
            ByteCount::from_bytes(1024),
            64,
        )
        .unwrap();
        collector
            .push(RunRecord::new(b"a".to_vec(), b"b".to_vec()))
            .unwrap();
        let run = collector.finish().unwrap().pop().unwrap();
        assert_eq!(
            fs::metadata(run.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn every_run_kind_roundtrips_its_header_binding() {
        let root = temp_root("kinds");
        let governor = governor(1_000_000, 1_000_000);
        let workspace = workspace(&root, 12);
        let kinds = [
            RunKind::Nodes,
            RunKind::RelationshipsOutbound,
            RunKind::RelationshipsInbound,
            RunKind::Filters,
            RunKind::FilterDictionary,
            RunKind::Resolution,
        ];
        let mut runs = Vec::new();
        for kind in kinds {
            let mut collector =
                RunCollector::new(&workspace, &governor, kind, ByteCount::from_bytes(1024), 64)
                    .unwrap();
            collector
                .push(RunRecord::new(vec![kind as u8], b"value".to_vec()))
                .unwrap();
            let run = collector.finish().unwrap().pop().unwrap();
            let mut reader = RunReader::open(run.path(), Some(run.header), 64).unwrap();
            assert_eq!(reader.header.kind, kind);
            assert_eq!(reader.next_record().unwrap().unwrap().key, vec![kind as u8]);
            runs.push(run);
        }
    }

    #[test]
    fn abandoned_cleanup_removes_only_other_well_formed_workspaces() {
        let root = temp_root("cleanup");
        let current_id = [2; 16];
        let current = RunWorkspace::create(&root, [1; 16], current_id, 42, 4).unwrap();
        let abandoned = root.join(format!("pggraph-build-{}", hex_id([3; 16])));
        fs::create_dir(&abandoned).unwrap();
        let unrelated = root.join("pggraph-build-not-a-valid-id");
        fs::create_dir(&unrelated).unwrap();
        assert_eq!(
            RunWorkspace::cleanup_abandoned_while_build_locked(&root, current_id).unwrap(),
            1
        );
        assert!(current.path().is_dir());
        assert!(!abandoned.exists());
        assert!(unrelated.is_dir());
    }
}
