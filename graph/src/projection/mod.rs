//! Projection read helpers shared by graph algorithms.

#[allow(
    dead_code,
    reason = "durable projection manifest metadata is introduced before readers consume it"
)]
pub(crate) mod manifest;
pub(crate) mod neighbors;
#[cfg(any(test, feature = "fuzzing", feature = "development"))]
pub(crate) mod segment;
#[cfg(test)]
mod test_contracts;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub(crate) mod tx_delta;
