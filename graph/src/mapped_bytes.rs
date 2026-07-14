//! Ownership wrapper for immutable mapped graph bytes.
//!
//! Production graph views retain an `Arc<Mmap>`. Unit tests use an owned byte
//! allocation so the same validation and access code can run under Miri,
//! which does not emulate operating-system file mappings.

use memmap2::Mmap;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct MappedBytes {
    backing: MappedBytesBacking,
}

#[derive(Clone, Debug)]
enum MappedBytesBacking {
    Mmap(Arc<Mmap>),
    #[cfg(test)]
    Test {
        words: Arc<[u64]>,
        len: usize,
    },
}

impl MappedBytes {
    pub(crate) fn from_mmap(mmap: Arc<Mmap>) -> Self {
        Self {
            backing: MappedBytesBacking::Mmap(mmap),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_test_bytes(bytes: Vec<u8>) -> Self {
        let mut words = Vec::with_capacity(bytes.len().div_ceil(std::mem::size_of::<u64>()));
        for chunk in bytes.chunks(std::mem::size_of::<u64>()) {
            let mut word = [0u8; std::mem::size_of::<u64>()];
            word[..chunk.len()].copy_from_slice(chunk);
            words.push(u64::from_ne_bytes(word));
        }
        Self {
            backing: MappedBytesBacking::Test {
                words: words.into(),
                len: bytes.len(),
            },
        }
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        match &self.backing {
            MappedBytesBacking::Mmap(mmap) => mmap,
            #[cfg(test)]
            MappedBytesBacking::Test { words, len } => {
                // SAFETY: Every u64 is initialized, u8 accepts every bit
                // pattern, and `len` never exceeds the word allocation's byte
                // extent. The Arc retains the allocation for the slice.
                unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), *len) }
            }
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.as_slice().len()
    }
}
