//! Candidate edge-type artifact width benchmark.
//!
//! This target models only the encoded edge-type section. It does not change
//! `EdgeTypeId`, `EdgeStore`, or the production artifact format. The retained
//! measurements can therefore compare one-, two-, and four-byte candidates
//! before a storage migration is selected.

#![allow(missing_docs)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;
use std::time::Duration;

const EDGE_COUNT: usize = 1_000_000;

#[derive(Clone, Copy)]
enum CandidateWidth {
    One,
    Two,
    Four,
}

impl CandidateWidth {
    const fn bytes(self) -> usize {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Four => 4,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::One => "u8",
            Self::Two => "u16",
            Self::Four => "u32",
        }
    }
}

struct EdgeTypeArtifactFixture {
    encoded: Vec<u8>,
    width: CandidateWidth,
    label_count: u32,
}

impl EdgeTypeArtifactFixture {
    fn new(edge_count: usize, label_count: u32, width: CandidateWidth) -> Self {
        let maximum_user_id = match width {
            CandidateWidth::One => u32::from(u8::MAX - 1),
            CandidateWidth::Two => u32::from(u16::MAX - 1),
            CandidateWidth::Four => u32::MAX - 1,
        };
        assert!(
            (1..=maximum_user_id).contains(&label_count),
            "fixture label count must fit without using the all-ones sentinel"
        );
        let capacity = edge_count
            .checked_mul(width.bytes())
            .expect("benchmark fixture byte count must fit usize");
        let mut encoded = Vec::with_capacity(capacity);
        for edge_index in 0..edge_count {
            let type_id = 1 + u32::try_from(edge_index)
                .expect("benchmark edge count must fit the logical u32 ID domain")
                % label_count;
            match width {
                CandidateWidth::One => encoded.push(
                    u8::try_from(type_id).expect("one-byte fixture validates its label count"),
                ),
                CandidateWidth::Two => encoded.extend_from_slice(
                    &u16::try_from(type_id)
                        .expect("two-byte fixture validates its label count")
                        .to_le_bytes(),
                ),
                CandidateWidth::Four => encoded.extend_from_slice(&type_id.to_le_bytes()),
            }
        }
        assert_eq!(encoded.len(), capacity);
        Self {
            encoded,
            width,
            label_count,
        }
    }

    fn decode_checksum(&self) -> u64 {
        match self.width {
            CandidateWidth::One => self.encoded.iter().map(|value| u64::from(*value)).sum(),
            CandidateWidth::Two => self
                .encoded
                .chunks_exact(2)
                .map(|bytes| u64::from(u16::from_le_bytes([bytes[0], bytes[1]])))
                .sum(),
            CandidateWidth::Four => self
                .encoded
                .chunks_exact(4)
                .map(|bytes| {
                    u64::from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                })
                .sum(),
        }
    }

    fn benchmark_id(&self) -> String {
        format!(
            "{}_{}_labels_{}_artifact_bytes",
            self.width.label(),
            self.label_count,
            self.encoded.len()
        )
    }
}

fn bench_edge_type_width_candidates(c: &mut Criterion) {
    let fixtures = [
        EdgeTypeArtifactFixture::new(EDGE_COUNT, 254, CandidateWidth::One),
        EdgeTypeArtifactFixture::new(EDGE_COUNT, 255, CandidateWidth::Two),
        EdgeTypeArtifactFixture::new(EDGE_COUNT, 65_534, CandidateWidth::Two),
        EdgeTypeArtifactFixture::new(EDGE_COUNT, 65_535, CandidateWidth::Four),
        EdgeTypeArtifactFixture::new(EDGE_COUNT, 254, CandidateWidth::Four),
    ];

    let mut scan = c.benchmark_group("edge_type_width_decode");
    scan.measurement_time(Duration::from_secs(8));
    scan.sample_size(30);
    for fixture in &fixtures {
        scan.throughput(Throughput::Elements(EDGE_COUNT as u64));
        scan.bench_with_input(
            BenchmarkId::from_parameter(fixture.benchmark_id()),
            fixture,
            |b, fixture| b.iter(|| black_box(fixture).decode_checksum()),
        );
    }
    scan.finish();

    let mut materialization = c.benchmark_group("edge_type_width_artifact_copy");
    materialization.measurement_time(Duration::from_secs(8));
    materialization.sample_size(30);
    for fixture in &fixtures {
        materialization.throughput(Throughput::Bytes(fixture.encoded.len() as u64));
        materialization.bench_with_input(
            BenchmarkId::from_parameter(fixture.benchmark_id()),
            fixture,
            |b, fixture| b.iter(|| black_box(fixture.encoded.clone())),
        );
    }
    materialization.finish();
}

criterion_group!(benches, bench_edge_type_width_candidates);
criterion_main!(benches);
