//! Durable relationship-identity dictionary artifacts.
//!
//! Mutable projection segments store compact numeric relationship IDs. This
//! module owns the cumulative, checksummed mapping from those IDs to canonical
//! PostgreSQL source-row identities for each published manifest generation.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::edge_store::{RelationshipId, RelationshipIdentity};
use crate::safety::{GraphError, GraphResult};

const MAGIC: &[u8; 8] = b"PGGID001";
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 32;
const CHECKSUM_OFFSET: usize = 24;

/// Validated cumulative relationship identity dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelationshipIdentityDictionary {
    identities: Vec<Option<RelationshipIdentity>>,
    ids_by_identity: HashMap<RelationshipIdentity, RelationshipId>,
}

impl RelationshipIdentityDictionary {
    /// Validate and index a cumulative identity vector.
    pub(crate) fn try_from_identities(
        identities: Vec<Option<RelationshipIdentity>>,
    ) -> GraphResult<Self> {
        if identities.first().is_none_or(Option::is_some) {
            return Err(identity_corrupt(
                "relationship identity dictionary must reserve empty slot 0",
            ));
        }
        let mut ids_by_identity = HashMap::with_capacity(identities.len().saturating_sub(1));
        for (idx, identity) in identities.iter().enumerate().skip(1) {
            let Some(identity) = identity else {
                return Err(identity_corrupt(format!(
                    "relationship identity dictionary slot {idx} is empty"
                )));
            };
            if identity.mapping_id == 0 {
                return Err(identity_corrupt(format!(
                    "relationship identity dictionary slot {idx} is invalid"
                )));
            }
            let id = RelationshipId::try_from(idx).map_err(|_| GraphError::OverlayLimit {
                kind: "relationship_identities".to_string(),
                requested: idx,
                limit: RelationshipId::MAX as usize,
            })?;
            if ids_by_identity.insert(identity.clone(), id).is_some() {
                return Err(identity_corrupt(format!(
                    "relationship identity dictionary contains duplicate mapping {} source key {}",
                    identity.mapping_id, identity.source_key
                )));
            }
        }
        Ok(Self {
            identities,
            ids_by_identity,
        })
    }

    /// Append unseen identities in deterministic canonical order.
    pub(crate) fn intern_all(
        &mut self,
        identities: impl IntoIterator<Item = RelationshipIdentity>,
    ) -> GraphResult<()> {
        let mut unseen = identities
            .into_iter()
            .filter(|identity| !self.ids_by_identity.contains_key(identity))
            .collect::<Vec<_>>();
        unseen.sort_by(|left, right| {
            (left.mapping_id, &left.source_key).cmp(&(right.mapping_id, &right.source_key))
        });
        unseen.dedup();
        for identity in unseen {
            let idx = self.identities.len();
            let id = RelationshipId::try_from(idx).map_err(|_| GraphError::OverlayLimit {
                kind: "relationship_identities".to_string(),
                requested: idx,
                limit: RelationshipId::MAX as usize,
            })?;
            self.ids_by_identity.insert(identity.clone(), id);
            self.identities.push(Some(identity));
        }
        Ok(())
    }

    /// Resolve one canonical identity to its durable numeric ID.
    pub(crate) fn id_for(&self, identity: &RelationshipIdentity) -> GraphResult<RelationshipId> {
        self.ids_by_identity.get(identity).copied().ok_or_else(|| {
            GraphError::Internal(format!(
                "relationship identity mapping {} source key {} was not interned",
                identity.mapping_id, identity.source_key
            ))
        })
    }

    /// Return the cumulative identity vector in ID order.
    pub(crate) fn identities(&self) -> &[Option<RelationshipIdentity>] {
        &self.identities
    }
}

/// Write a dictionary artifact atomically and return checksum and byte count.
pub(crate) fn write_identity_artifact(
    root: &Path,
    path: &Path,
    dictionary: &RelationshipIdentityDictionary,
) -> GraphResult<(String, u64)> {
    fs::create_dir_all(root).map_err(|err| identity_io("create directory", root, err))?;
    let payload =
        bincode::serde::encode_to_vec(dictionary.identities(), bincode::config::standard())
            .map_err(|err| {
                GraphError::Internal(format!("relationship identity encoding failed: {err}"))
            })?;
    let count =
        u32::try_from(dictionary.identities().len()).map_err(|_| GraphError::OverlayLimit {
            kind: "relationship_identities".to_string(),
            requested: dictionary.identities().len(),
            limit: u32::MAX as usize,
        })?;
    let payload_len = u64::try_from(payload.len())
        .map_err(|_| GraphError::Internal("identity payload length exceeds u64".to_string()))?;
    let mut bytes = vec![0_u8; HEADER_SIZE];
    bytes[0..8].copy_from_slice(MAGIC);
    bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
    bytes[12..16].copy_from_slice(&count.to_le_bytes());
    bytes[16..24].copy_from_slice(&payload_len.to_le_bytes());
    bytes.extend_from_slice(&payload);
    let checksum = checksum_bytes(&bytes);
    bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());

    let (temp_path, mut file) = create_temp_file(root, path)?;
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|err| identity_io("write temp artifact", &temp_path, err))?;
        file.sync_all()
            .map_err(|err| identity_io("sync temp artifact", &temp_path, err))?;
        fs::rename(&temp_path, path).map_err(|err| identity_io("publish artifact", path, err))?;
        sync_directory(root)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result?;
    Ok((format!("crc32:{checksum:08x}"), bytes.len() as u64))
}

/// Read and validate a dictionary artifact and its expected checksum.
pub(crate) fn read_identity_artifact(
    path: &Path,
    expected_checksum: &str,
) -> GraphResult<RelationshipIdentityDictionary> {
    read_identity_artifact_inner(path, expected_checksum, None)
}

/// Read an artifact referenced by a manifest with pre-allocation bounds.
pub(crate) fn read_manifest_identity_artifact(
    path: &Path,
    expected_checksum: &str,
    expected_bytes: u64,
    expected_count: u32,
) -> GraphResult<RelationshipIdentityDictionary> {
    read_identity_artifact_inner(
        path,
        expected_checksum,
        Some((expected_bytes, expected_count)),
    )
}

fn read_identity_artifact_inner(
    path: &Path,
    expected_checksum: &str,
    manifest_bounds: Option<(u64, u32)>,
) -> GraphResult<RelationshipIdentityDictionary> {
    let mut file = fs::File::open(path).map_err(|err| identity_io("open artifact", path, err))?;
    let file_len = file
        .metadata()
        .map_err(|err| identity_io("stat artifact", path, err))?
        .len();
    if manifest_bounds.is_some_and(|(expected_bytes, _)| expected_bytes != file_len) {
        return Err(identity_corrupt(
            "relationship identity manifest byte count mismatch",
        ));
    }
    if file_len < HEADER_SIZE as u64 {
        return Err(identity_corrupt(
            "relationship identity artifact is shorter than its header",
        ));
    }
    let mut header = [0_u8; HEADER_SIZE];
    file.read_exact(&mut header)
        .map_err(|err| identity_io("read artifact header", path, err))?;
    if &header[0..8] != MAGIC {
        return Err(identity_corrupt(
            "invalid relationship identity artifact magic",
        ));
    }
    let version = read_u32(&header, 8)?;
    if version != VERSION {
        return Err(GraphError::IncompatibleVersion(format!(
            "relationship identity artifact version {version} is unsupported; expected {VERSION}"
        )));
    }
    let count = read_u32(&header, 12)? as usize;
    if manifest_bounds.is_some_and(|(_, expected_count)| expected_count as usize != count) {
        return Err(identity_corrupt(
            "relationship identity manifest entry count mismatch",
        ));
    }
    let payload_len = usize::try_from(read_u64(&header, 16)?)
        .map_err(|_| identity_corrupt("identity payload length exceeds usize"))?;
    let expected_file_len = HEADER_SIZE
        .checked_add(payload_len)
        .and_then(|len| u64::try_from(len).ok())
        .ok_or_else(|| identity_corrupt("identity artifact length overflows"))?;
    if expected_file_len != file_len {
        return Err(identity_corrupt(
            "relationship identity artifact payload length mismatch",
        ));
    }
    let mut bytes = Vec::with_capacity(expected_file_len as usize);
    bytes.extend_from_slice(&header);
    bytes.resize(expected_file_len as usize, 0);
    file.read_exact(&mut bytes[HEADER_SIZE..])
        .map_err(|err| identity_io("read artifact payload", path, err))?;
    let mut trailing = [0_u8; 1];
    if file
        .read(&mut trailing)
        .map_err(|err| identity_io("check artifact length", path, err))?
        != 0
    {
        return Err(identity_corrupt(
            "relationship identity artifact grew while being read",
        ));
    }
    let stored_checksum = read_u32(&bytes, CHECKSUM_OFFSET)?;
    let checksum = checksum_bytes(&bytes);
    if stored_checksum != checksum || expected_checksum != format!("crc32:{checksum:08x}") {
        return Err(identity_corrupt(
            "relationship identity artifact checksum mismatch",
        ));
    }
    let (identities, consumed): (Vec<Option<RelationshipIdentity>>, usize) =
        bincode::serde::decode_from_slice(&bytes[HEADER_SIZE..], bincode::config::standard())
            .map_err(|err| identity_corrupt(format!("identity payload decode failed: {err}")))?;
    if consumed != payload_len || identities.len() != count {
        return Err(identity_corrupt(
            "relationship identity artifact entry count mismatch",
        ));
    }
    RelationshipIdentityDictionary::try_from_identities(identities)
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
        .ok_or_else(|| identity_corrupt("identity artifact u32 read out of bounds"))?;
    Ok(u32::from_le_bytes(raw.try_into().map_err(|_| {
        identity_corrupt("invalid identity artifact u32")
    })?))
}

fn read_u64(bytes: &[u8], offset: usize) -> GraphResult<u64> {
    let raw = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| identity_corrupt("identity artifact u64 read out of bounds"))?;
    Ok(u64::from_le_bytes(raw.try_into().map_err(|_| {
        identity_corrupt("invalid identity artifact u64")
    })?))
}

fn create_temp_file(root: &Path, path: &Path) -> GraphResult<(PathBuf, fs::File)> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| GraphError::Internal(format!("system clock before Unix epoch: {err}")))?
        .as_nanos();
    for attempt in 0..128 {
        let temp_path = root.join(format!(
            ".{}.tmp-{}-{stamp}-{attempt}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("identity"),
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(identity_io("create temp artifact", &temp_path, err)),
        }
    }
    Err(GraphError::Internal(
        "relationship identity temp path kept colliding".to_string(),
    ))
}

fn sync_directory(path: &Path) -> GraphResult<()> {
    let dir = fs::File::open(path).map_err(|err| identity_io("open directory", path, err))?;
    dir.sync_all()
        .map_err(|err| identity_io("sync directory", path, err))
}

fn identity_corrupt(reason: impl Into<String>) -> GraphError {
    GraphError::CorruptFile {
        reason: reason.into(),
    }
}

fn identity_io(operation: &str, path: &Path, err: std::io::Error) -> GraphError {
    GraphError::Internal(format!(
        "relationship identity {operation} failed for {}: {err}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_interning_is_deterministic_and_artifact_roundtrips() {
        let mut dictionary = RelationshipIdentityDictionary::try_from_identities(vec![None])
            .expect("empty dictionary validates");
        let first = RelationshipIdentity {
            mapping_id: 2,
            source_key: "b".to_string(),
        };
        let second = RelationshipIdentity {
            mapping_id: 1,
            source_key: "a".to_string(),
        };
        dictionary
            .intern_all([first.clone(), second.clone(), first.clone()])
            .expect("identities intern");
        assert_eq!(dictionary.id_for(&second).expect("second id"), 1);
        assert_eq!(dictionary.id_for(&first).expect("first id"), 2);

        let root = std::env::temp_dir().join(format!(
            "pggraph-identity-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let path = root.join("identities.bin");
        let (checksum, bytes) =
            write_identity_artifact(&root, &path, &dictionary).expect("identity artifact writes");
        let decoded = read_identity_artifact(&path, &checksum).expect("identity artifact reads");
        assert_eq!(decoded, dictionary);
        let bounded = read_manifest_identity_artifact(
            &path,
            &checksum,
            bytes,
            dictionary.identities().len() as u32,
        )
        .expect("manifest-bounded identity artifact reads");
        assert_eq!(bounded, dictionary);
        assert!(read_manifest_identity_artifact(
            &path,
            &checksum,
            bytes.saturating_add(1),
            dictionary.identities().len() as u32,
        )
        .is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dictionary_rejects_empty_duplicate_and_invalid_slots() {
        assert!(RelationshipIdentityDictionary::try_from_identities(Vec::new()).is_err());
        assert!(RelationshipIdentityDictionary::try_from_identities(vec![None, None]).is_err());
        let identity = RelationshipIdentity {
            mapping_id: 1,
            source_key: "edge".to_string(),
        };
        assert!(RelationshipIdentityDictionary::try_from_identities(vec![
            None,
            Some(identity.clone()),
            Some(identity),
        ])
        .is_err());
        assert!(RelationshipIdentityDictionary::try_from_identities(vec![
            None,
            Some(RelationshipIdentity {
                mapping_id: 1,
                source_key: String::new(),
            }),
        ])
        .is_ok());
    }

    #[test]
    fn identity_artifact_rejects_checksum_corruption() {
        let root = std::env::temp_dir().join(format!(
            "pggraph-identity-corrupt-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let path = root.join("identities.bin");
        let dictionary = RelationshipIdentityDictionary::try_from_identities(vec![
            None,
            Some(RelationshipIdentity {
                mapping_id: 1,
                source_key: "edge-1".to_string(),
            }),
        ])
        .expect("dictionary validates");
        let (checksum, _) =
            write_identity_artifact(&root, &path, &dictionary).expect("artifact writes");
        let mut bytes = fs::read(&path).expect("artifact reads");
        let last = bytes.last_mut().expect("artifact has payload");
        *last ^= 0xff;
        fs::write(&path, bytes).expect("corrupt artifact writes");

        let err = read_identity_artifact(&path, &checksum)
            .expect_err("corrupt identity artifact must fail closed");
        assert!(matches!(err, GraphError::CorruptFile { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn identity_artifact_rejects_declared_payload_before_allocating_it() {
        let root = std::env::temp_dir().join(format!(
            "pggraph-identity-length-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("test directory creates");
        let path = root.join("identities.bin");
        let mut header = [0_u8; HEADER_SIZE];
        header[0..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&VERSION.to_le_bytes());
        header[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        header[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&path, header).expect("malformed header writes");

        let err = read_identity_artifact(&path, "crc32:00000000")
            .expect_err("oversized declared payload rejects before allocation");
        assert!(matches!(err, GraphError::CorruptFile { .. }));
        let _ = fs::remove_dir_all(root);
    }
}
