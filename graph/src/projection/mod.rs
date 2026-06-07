//! Projection read helpers shared by graph algorithms.

#[allow(
    dead_code,
    reason = "Microphase 9 adds base chunk rewrite helpers before compaction and SQL repair scheduling consume them"
)]
pub(crate) mod chunk;
#[allow(
    dead_code,
    reason = "Microphase 10 adds compaction helpers before scheduled maintenance wires compaction policy"
)]
pub(crate) mod compact;
pub(crate) mod ingest;
#[allow(
    dead_code,
    reason = "Microphase 7 adds the layered runtime before Microphase 8 routes Engine reads through it"
)]
pub(crate) mod layered;
#[allow(
    dead_code,
    reason = "durable projection manifest metadata is introduced before readers consume it"
)]
pub(crate) mod manifest;
pub(crate) mod neighbors;
pub(crate) mod normalize;
pub(crate) mod segment;
#[cfg(test)]
mod test_contracts;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub(crate) mod tx_delta;
