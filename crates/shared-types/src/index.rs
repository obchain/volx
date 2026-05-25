//! Published index value — the engine's output to `ClickHouse` / API consumers.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::OffsetDateTime;

use crate::ids::IndexId;

/// 32-byte content hash of the snapshot's strip set (blake3 / sha256). Wire
/// format is lowercase hex (no `0x` prefix); equality is on the raw bytes.
///
/// The hash algorithm is decided by the engine crate when it builds the
/// snapshot — `shared-types` only owns the carrier type so API + storage
/// agree on the on-wire encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StripHash(pub [u8; 32]);

impl Serialize for StripHash {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&hex::encode(self.0))
    }
}

impl<'de> Deserialize<'de> for StripHash {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        // `String`, not `&str`: non-self-describing formats (postcard, msgpack,
        // bincode) can't hand out a borrowed slice, and we want this type to
        // round-trip through any serde backend the engine ever uses.
        let s = String::deserialize(de)?;
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        let arr: [u8; 32] = bytes.try_into().map_err(|v: Vec<u8>| {
            serde::de::Error::custom(format!("expected 32 bytes, got {}", v.len()))
        })?;
        Ok(Self(arr))
    }
}

/// One published index row (one per snapshot per index, §5).
///
/// `value` is in volatility points (`BVOL = 100·√σ²_30d`, §4.7).
/// `confidence` is bounded to `[0.0, 1.0]`; the engine assigns it from the
/// snapshot's data-quality signals (strike count, IV-fit residual, etc.).
/// `ts` is the snapshot timestamp (bar open), not the engine wall-clock (§5).
///
/// Invariants enforced at deserialize time:
/// - `confidence` finite and in `[0.0, 1.0]`,
/// - `value` finite and non-negative (`100·√σ²_30d` is never negative).
///
/// A future bump may promote `confidence` to a `Confidence(f64)` newtype with
/// `TryFrom<f64>`; doing it inline keeps the published-row schema flat for the
/// `ClickHouse` column mapping landing in #15.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexValue {
    pub index_id: IndexId,
    #[serde(deserialize_with = "de_finite_non_negative")]
    pub value: f64,
    #[serde(deserialize_with = "de_unit_interval")]
    pub confidence: f64,
    pub strip_hash: StripHash,
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
}

fn de_finite_non_negative<'de, D: Deserializer<'de>>(de: D) -> Result<f64, D::Error> {
    let v = f64::deserialize(de)?;
    if !v.is_finite() || v < 0.0 {
        return Err(serde::de::Error::custom(format!(
            "IndexValue.value must be finite and >= 0, got {v}"
        )));
    }
    Ok(v)
}

fn de_unit_interval<'de, D: Deserializer<'de>>(de: D) -> Result<f64, D::Error> {
    let v = f64::deserialize(de)?;
    if !v.is_finite() || !(0.0..=1.0).contains(&v) {
        return Err(serde::de::Error::custom(format!(
            "IndexValue.confidence must be in [0.0, 1.0], got {v}"
        )));
    }
    Ok(v)
}
