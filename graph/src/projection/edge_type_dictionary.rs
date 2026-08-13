//! Durable cumulative relationship-type dictionary artifacts.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::edge_type_registry::EdgeTypeRegistry;
use crate::safety::{GraphError, GraphResult};
use crate::types::EdgeTypeId;

const MAGIC: &[u8; 8] = b"PGGTYP01";
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 32;
const CHECKSUM_OFFSET: usize = 24;

/// Validated cumulative relationship-type dictionary in logical-ID order.
#[derive(Debug, Clone)]
pub(crate) struct EdgeTypeDictionary {
    registry: EdgeTypeRegistry,
}

impl EdgeTypeDictionary {
    /// Validate exact source spellings, policy limits, and uniqueness.
    pub(crate) fn try_from_labels(labels: Vec<String>) -> GraphResult<Self> {
        Ok(Self {
            registry: EdgeTypeRegistry::try_from_labels(labels)?,
        })
    }

    /// Borrow labels in logical-ID order, including reserved slot zero.
    pub(crate) fn labels(&self) -> &[String] {
        self.registry.as_slice()
    }

    /// Return the logical ID assigned to an exact source spelling.
    pub(crate) fn id(&self, label: &str) -> Option<EdgeTypeId> {
        self.registry.id(label)
    }

    /// Append one previously unseen source spelling under the public policy
    /// limits. Existing labels retain their current logical IDs.
    pub(crate) fn intern(&mut self, label: &str) -> GraphResult<EdgeTypeId> {
        self.registry.register(label)
    }

    /// Consume the artifact wrapper and return its validated runtime registry.
    pub(crate) fn into_registry(self) -> EdgeTypeRegistry {
        self.registry
    }
}

/// Write one checksummed dictionary through an atomic candidate rename.
pub(crate) fn write_edge_type_dictionary_artifact(
    root: &Path,
    path: &Path,
    dictionary: &EdgeTypeDictionary,
    governor: &crate::resource::ResourceGovernor,
) -> GraphResult<(String, u64)> {
    write_edge_type_dictionary_artifact_inner(root, path, dictionary, Some(governor))
}

/// Write an artifact whose exact memory and disk bytes are already retained by
/// the caller's larger atomic publication plan.
pub(crate) fn write_edge_type_dictionary_artifact_precharged(
    root: &Path,
    path: &Path,
    dictionary: &EdgeTypeDictionary,
) -> GraphResult<(String, u64)> {
    write_edge_type_dictionary_artifact_inner(root, path, dictionary, None)
}

fn write_edge_type_dictionary_artifact_inner(
    root: &Path,
    path: &Path,
    dictionary: &EdgeTypeDictionary,
    governor: Option<&crate::resource::ResourceGovernor>,
) -> GraphResult<(String, u64)> {
    fs::create_dir_all(root).map_err(|error| dictionary_io("create directory", root, error))?;
    let labels = dictionary.labels();
    let count = u32::try_from(labels.len())
        .map_err(|_| dictionary_corrupt("relationship type count exceeds u32"))?;
    let payload_len = labels.iter().try_fold(0_usize, |total, label| {
        total
            .checked_add(label.len())
            .ok_or_else(|| dictionary_corrupt("relationship type payload length overflows"))
    })?;
    let offset_bytes = labels
        .len()
        .checked_add(1)
        .and_then(|count| count.checked_mul(8))
        .ok_or_else(|| dictionary_corrupt("relationship type offset table overflows"))?;
    let total_len = HEADER_SIZE
        .checked_add(offset_bytes)
        .and_then(|bytes| bytes.checked_add(payload_len))
        .ok_or_else(|| dictionary_corrupt("relationship type artifact length overflows"))?;
    let resource_bytes = crate::resource::ByteCount::from_usize(total_len)
        .ok_or_else(|| dictionary_corrupt("relationship type artifact length exceeds u64"))?;
    let _memory = governor
        .map(|governor| {
            governor
                .reserve_memory(crate::resource::ResourcePhase::Persistence, resource_bytes)
                .map_err(crate::safety::resource_limit_error)
        })
        .transpose()?;
    let _disk = governor
        .map(|governor| {
            governor
                .reserve_disk(crate::resource::ResourcePhase::Persistence, resource_bytes)
                .map_err(crate::safety::resource_limit_error)
        })
        .transpose()?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(total_len).map_err(|error| {
        GraphError::Internal(format!(
            "relationship type artifact allocation failed: {error}"
        ))
    })?;
    bytes.resize(HEADER_SIZE, 0);
    bytes[0..8].copy_from_slice(MAGIC);
    bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
    bytes[12..16].copy_from_slice(&count.to_le_bytes());
    bytes[16..24].copy_from_slice(
        &u64::try_from(payload_len)
            .map_err(|_| dictionary_corrupt("relationship type payload exceeds u64"))?
            .to_le_bytes(),
    );
    let mut offset = 0_u64;
    bytes.extend_from_slice(&offset.to_le_bytes());
    for label in labels {
        offset =
            offset
                .checked_add(u64::try_from(label.len()).map_err(|_| {
                    dictionary_corrupt("relationship type label length exceeds u64")
                })?)
                .ok_or_else(|| dictionary_corrupt("relationship type offset overflows"))?;
        bytes.extend_from_slice(&offset.to_le_bytes());
    }
    for label in labels {
        bytes.extend_from_slice(label.as_bytes());
    }
    debug_assert_eq!(bytes.len(), total_len);
    let checksum = checksum_bytes(&bytes);
    bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());

    let (temp_path, mut file) = create_temp_file(root, path)?;
    let mut published = false;
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|error| dictionary_io("write temp artifact", &temp_path, error))?;
        file.sync_all()
            .map_err(|error| dictionary_io("sync temp artifact", &temp_path, error))?;
        fs::hard_link(&temp_path, path)
            .map_err(|error| dictionary_io("publish artifact without overwrite", path, error))?;
        published = true;
        fs::remove_file(&temp_path)
            .map_err(|error| dictionary_io("remove temp artifact", &temp_path, error))?;
        fs::File::open(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| dictionary_io("sync directory", root, error))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
        if published {
            let _ = fs::remove_file(path);
            let _ = fs::File::open(root).and_then(|directory| directory.sync_all());
        }
    }
    result?;
    Ok((format!("crc32:{checksum:08x}"), bytes.len() as u64))
}

/// Read a manifest-bounded dictionary after validating allocation limits.
pub(crate) fn read_manifest_edge_type_dictionary_artifact(
    path: &Path,
    expected_checksum: &str,
    expected_bytes: u64,
    expected_count: u32,
) -> GraphResult<EdgeTypeDictionary> {
    read_edge_type_dictionary_artifact_inner(
        path,
        expected_checksum,
        Some((expected_bytes, expected_count)),
    )
}

/// Conservative peak heap needed to decode and index a manifest dictionary.
pub(crate) fn edge_type_dictionary_decode_upper_bound(
    artifact_bytes: u64,
    entry_count: u32,
) -> GraphResult<u64> {
    let artifact = usize::try_from(artifact_bytes)
        .map_err(|_| dictionary_corrupt("relationship type artifact exceeds usize"))?;
    let count = entry_count as usize;
    let offset_bytes = count
        .checked_add(1)
        .and_then(|value| value.checked_mul(8))
        .ok_or_else(|| dictionary_corrupt("relationship type offsets overflow"))?;
    let payload = artifact
        .checked_sub(HEADER_SIZE)
        .and_then(|bytes| bytes.checked_sub(offset_bytes))
        .ok_or_else(|| dictionary_corrupt("relationship type artifact length is invalid"))?;
    let registry = EdgeTypeRegistry::decoded_heap_upper_bound(count, payload)?;
    artifact
        .checked_add(offset_bytes)
        .and_then(|bytes| {
            bytes.checked_add(count.checked_mul(std::mem::size_of::<std::ops::Range<usize>>())?)
        })
        .and_then(|bytes| bytes.checked_add(registry))
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| dictionary_corrupt("relationship type decode bound overflowed"))
}

/// Read a dictionary when its checksum is known independently of a manifest.
#[cfg(test)]
pub(crate) fn read_edge_type_dictionary_artifact(
    path: &Path,
    expected_checksum: &str,
) -> GraphResult<EdgeTypeDictionary> {
    read_edge_type_dictionary_artifact_inner(path, expected_checksum, None)
}

fn read_edge_type_dictionary_artifact_inner(
    path: &Path,
    expected_checksum: &str,
    manifest_bounds: Option<(u64, u32)>,
) -> GraphResult<EdgeTypeDictionary> {
    let mut file = fs::File::open(path).map_err(|error| dictionary_io("open", path, error))?;
    let file_len = file
        .metadata()
        .map_err(|error| dictionary_io("stat", path, error))?
        .len();
    if manifest_bounds.is_some_and(|(bytes, _)| bytes != file_len) {
        return Err(dictionary_corrupt(
            "relationship type manifest byte count mismatch",
        ));
    }
    if file_len < HEADER_SIZE as u64 {
        return Err(dictionary_corrupt(
            "relationship type artifact is shorter than its header",
        ));
    }
    let mut header = [0_u8; HEADER_SIZE];
    file.read_exact(&mut header)
        .map_err(|error| dictionary_io("read header", path, error))?;
    if &header[0..8] != MAGIC {
        return Err(dictionary_corrupt(
            "invalid relationship type artifact magic",
        ));
    }
    let version = read_u32(&header, 8)?;
    if version != VERSION {
        return Err(GraphError::IncompatibleVersion(format!(
            "relationship type artifact version {version} is unsupported; expected {VERSION}"
        )));
    }
    let count = usize::try_from(read_u32(&header, 12)?)
        .map_err(|_| dictionary_corrupt("relationship type count exceeds usize"))?;
    if count == 0 || count > EdgeTypeRegistry::MAX_USER_EDGE_TYPES + 1 {
        return Err(dictionary_corrupt("relationship type count exceeds policy"));
    }
    if manifest_bounds.is_some_and(|(_, expected)| expected as usize != count) {
        return Err(dictionary_corrupt(
            "relationship type manifest count mismatch",
        ));
    }
    let payload_len = usize::try_from(read_u64(&header, 16)?)
        .map_err(|_| dictionary_corrupt("relationship type payload exceeds usize"))?;
    if header[28..32] != [0; 4] {
        return Err(dictionary_corrupt(
            "relationship type reserved header bytes must be zero",
        ));
    }
    if payload_len > EdgeTypeRegistry::MAX_EDGE_TYPE_DICTIONARY_BYTES {
        return Err(dictionary_corrupt(
            "relationship type payload exceeds policy",
        ));
    }
    let offset_bytes = count
        .checked_add(1)
        .and_then(|value| value.checked_mul(8))
        .ok_or_else(|| dictionary_corrupt("relationship type offsets overflow"))?;
    let expected_len = HEADER_SIZE
        .checked_add(offset_bytes)
        .and_then(|value| value.checked_add(payload_len))
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| dictionary_corrupt("relationship type artifact length overflows"))?;
    if expected_len != file_len {
        return Err(dictionary_corrupt(
            "relationship type artifact length mismatch",
        ));
    }

    let mut offsets = Vec::new();
    offsets.try_reserve_exact(offset_bytes).map_err(|error| {
        GraphError::Internal(format!(
            "relationship type offset allocation failed: {error}"
        ))
    })?;
    offsets.resize(offset_bytes, 0);
    file.read_exact(&mut offsets)
        .map_err(|error| dictionary_io("read offsets", path, error))?;
    let mut previous = 0_usize;
    let mut ranges = Vec::new();
    ranges.try_reserve_exact(count).map_err(|error| {
        GraphError::Internal(format!(
            "relationship type offset allocation failed: {error}"
        ))
    })?;
    for index in 0..=count {
        let offset = usize::try_from(read_u64(&offsets, index * 8)?)
            .map_err(|_| dictionary_corrupt("relationship type offset exceeds usize"))?;
        if offset < previous || offset > payload_len {
            return Err(dictionary_corrupt("relationship type offsets are invalid"));
        }
        if index > 0 {
            let label_len = offset - previous;
            if index > 1 && label_len > EdgeTypeRegistry::MAX_EDGE_TYPE_LABEL_BYTES {
                return Err(dictionary_corrupt("relationship type label exceeds policy"));
            }
            ranges.push(previous..offset);
        }
        previous = offset;
    }
    if previous != payload_len || ranges.first().is_none_or(|range| !range.is_empty()) {
        return Err(dictionary_corrupt(
            "relationship type offsets do not reserve slot zero",
        ));
    }
    let file_len_usize = usize::try_from(file_len)
        .map_err(|_| dictionary_corrupt("relationship type artifact exceeds usize"))?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(file_len_usize).map_err(|error| {
        GraphError::Internal(format!(
            "relationship type decode allocation failed: {error}"
        ))
    })?;
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(&offsets);
    bytes.resize(file_len_usize, 0);
    let payload_start = HEADER_SIZE + offset_bytes;
    file.read_exact(&mut bytes[payload_start..])
        .map_err(|error| dictionary_io("read payload", path, error))?;
    let mut trailing = [0_u8; 1];
    if file
        .read(&mut trailing)
        .map_err(|error| dictionary_io("check length", path, error))?
        != 0
    {
        return Err(dictionary_corrupt(
            "relationship type artifact grew while reading",
        ));
    }
    let stored_checksum = read_u32(&bytes, CHECKSUM_OFFSET)?;
    let checksum = checksum_bytes(&bytes);
    if stored_checksum != checksum || expected_checksum != format!("crc32:{checksum:08x}") {
        return Err(dictionary_corrupt(
            "relationship type artifact checksum mismatch",
        ));
    }
    let payload = &bytes[payload_start..];
    let mut labels = Vec::new();
    labels.try_reserve_exact(count).map_err(|error| {
        GraphError::Internal(format!(
            "relationship type label allocation failed: {error}"
        ))
    })?;
    for range in ranges {
        let label = std::str::from_utf8(&payload[range])
            .map_err(|_| dictionary_corrupt("relationship type label is not UTF-8"))?;
        let mut owned = String::new();
        owned.try_reserve_exact(label.len()).map_err(|error| {
            GraphError::Internal(format!(
                "relationship type string allocation failed: {error}"
            ))
        })?;
        owned.push_str(label);
        labels.push(owned);
    }
    EdgeTypeDictionary::try_from_labels(labels)
}

fn checksum_bytes(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&bytes[..CHECKSUM_OFFSET]);
    hasher.update(&[0; 4]);
    hasher.update(&bytes[CHECKSUM_OFFSET + 4..]);
    hasher.finalize()
}

fn read_u32(bytes: &[u8], offset: usize) -> GraphResult<u32> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| dictionary_corrupt("relationship type u32 is out of bounds"))?;
    Ok(u32::from_le_bytes(raw.try_into().map_err(|_| {
        dictionary_corrupt("relationship type u32 is malformed")
    })?))
}

fn read_u64(bytes: &[u8], offset: usize) -> GraphResult<u64> {
    let raw = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| dictionary_corrupt("relationship type u64 is out of bounds"))?;
    Ok(u64::from_le_bytes(raw.try_into().map_err(|_| {
        dictionary_corrupt("relationship type u64 is malformed")
    })?))
}

fn create_temp_file(root: &Path, path: &Path) -> GraphResult<(PathBuf, fs::File)> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| GraphError::Internal(format!("system clock before Unix epoch: {error}")))?
        .as_nanos();
    for attempt in 0..128 {
        let temp_path = root.join(format!(
            ".{}.tmp-{}-{stamp}-{attempt}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("edge-types"),
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(dictionary_io("create temp artifact", &temp_path, error)),
        }
    }
    Err(GraphError::Internal(
        "relationship type temp path kept colliding".to_string(),
    ))
}

fn dictionary_corrupt(reason: impl Into<String>) -> GraphError {
    GraphError::CorruptFile {
        reason: reason.into(),
    }
}

fn dictionary_io(operation: &str, path: &Path, error: std::io::Error) -> GraphError {
    GraphError::Internal(format!(
        "relationship type {operation} failed for {}: {error}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn governor(memory: u64, disk: u64) -> crate::resource::ResourceGovernor {
        crate::resource::ResourceGovernor::new(crate::resource::ResourceLimits::bounded(
            crate::resource::MemoryBudget::new(crate::resource::ByteCount::from_bytes(memory)),
            crate::resource::DiskBudget::new(crate::resource::ByteCount::from_bytes(disk)),
            crate::resource::RowCount::UNLIMITED,
            crate::resource::WorkUnits::UNLIMITED,
            crate::resource::ElapsedBudget::new(Duration::MAX),
        ))
    }

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pggraph-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    #[test]
    fn edge_type_dictionary_roundtrips_boundaries_and_preserves_source_spelling() {
        let dictionary = EdgeTypeDictionary::try_from_labels(vec![
            String::new(),
            "Works_At".to_string(),
            "关系".to_string(),
        ])
        .expect("dictionary validates");
        let root = temp_root("edge-type-roundtrip");
        let path = root.join("edge-types.bin");
        let (checksum, bytes) = write_edge_type_dictionary_artifact(
            &root,
            &path,
            &dictionary,
            &governor(u64::MAX, u64::MAX),
        )
        .expect("dictionary writes");
        let decoded = read_manifest_edge_type_dictionary_artifact(
            &path,
            &checksum,
            bytes,
            dictionary.labels().len() as u32,
        )
        .expect("dictionary reads");
        assert_eq!(decoded.labels(), dictionary.labels());
        assert_eq!(decoded.id("Works_At").map(EdgeTypeId::get), Some(1));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn edge_type_dictionary_rejects_count_offset_utf8_duplicate_and_checksum_corruption() {
        assert!(EdgeTypeDictionary::try_from_labels(vec![
            String::new(),
            "same".into(),
            "same".into()
        ])
        .is_err());
        let dictionary = EdgeTypeDictionary::try_from_labels(vec![String::new(), "one".into()])
            .expect("dictionary validates");
        let root = temp_root("edge-type-corrupt");
        let path = root.join("edge-types.bin");
        let (checksum, _) = write_edge_type_dictionary_artifact(
            &root,
            &path,
            &dictionary,
            &governor(u64::MAX, u64::MAX),
        )
        .expect("dictionary writes");
        let original = fs::read(&path).expect("artifact reads");
        let mut checksum_corrupt = original.clone();
        checksum_corrupt[CHECKSUM_OFFSET] ^= 0xff;
        fs::write(&path, checksum_corrupt).expect("checksum corruption writes");
        assert!(read_edge_type_dictionary_artifact(&path, &checksum).is_err());

        let mut invalid_offset = original.clone();
        invalid_offset[HEADER_SIZE + 16..HEADER_SIZE + 24].copy_from_slice(&2_u64.to_le_bytes());
        repair_checksum(&mut invalid_offset);
        let offset_checksum = format!("crc32:{:08x}", checksum_bytes(&invalid_offset));
        fs::write(&path, invalid_offset).expect("offset corruption writes");
        assert!(read_edge_type_dictionary_artifact(&path, &offset_checksum).is_err());

        let mut invalid_utf8 = original;
        *invalid_utf8.last_mut().expect("payload exists") = 0xff;
        repair_checksum(&mut invalid_utf8);
        let utf8_checksum = format!("crc32:{:08x}", checksum_bytes(&invalid_utf8));
        fs::write(&path, invalid_utf8).expect("UTF-8 corruption writes");
        assert!(read_edge_type_dictionary_artifact(&path, &utf8_checksum).is_err());

        let mut invalid_reserved = fs::read(&path).expect("artifact reads");
        invalid_reserved[28] = 1;
        repair_checksum(&mut invalid_reserved);
        let reserved_checksum = format!("crc32:{:08x}", checksum_bytes(&invalid_reserved));
        fs::write(&path, invalid_reserved).expect("reserved corruption writes");
        assert!(read_edge_type_dictionary_artifact(&path, &reserved_checksum).is_err());
        let _ = fs::remove_dir_all(root);
    }

    fn repair_checksum(bytes: &mut [u8]) {
        let checksum = checksum_bytes(bytes);
        bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());
    }

    #[test]
    fn edge_type_dictionary_decode_is_resource_governed_before_allocation() {
        let count = 1_000_001_u32;
        let artifact_bytes = HEADER_SIZE as u64 + u64::from(count + 1) * 8;
        let decode_bound = edge_type_dictionary_decode_upper_bound(artifact_bytes, count)
            .expect("valid boundary metadata has a decode bound");
        assert!(decode_bound > artifact_bytes.saturating_mul(4));

        let root = temp_root("edge-type-bound");
        fs::create_dir_all(&root).expect("root creates");
        let path = root.join("edge-types.bin");
        let mut header = [0_u8; HEADER_SIZE];
        header[0..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&VERSION.to_le_bytes());
        header[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        header[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&path, header).expect("header writes");
        assert!(read_edge_type_dictionary_artifact(&path, "crc32:00000000").is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn edge_type_dictionary_writer_is_governed_and_never_overwrites() {
        let dictionary = EdgeTypeDictionary::try_from_labels(vec![String::new(), "one".into()])
            .expect("dictionary validates");
        let root = temp_root("edge-type-writer-governor");
        let path = root.join("edge-types.bin");
        assert!(write_edge_type_dictionary_artifact(
            &root,
            &path,
            &dictionary,
            &governor(1, u64::MAX),
        )
        .is_err());
        assert!(!path.exists());

        fs::create_dir_all(&root).expect("root creates");
        fs::write(&path, b"last-good").expect("last-good writes");
        assert!(write_edge_type_dictionary_artifact(
            &root,
            &path,
            &dictionary,
            &governor(u64::MAX, u64::MAX),
        )
        .is_err());
        assert_eq!(fs::read(&path).expect("last-good reads"), b"last-good");
        let _ = fs::remove_dir_all(root);
    }
}
