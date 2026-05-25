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
        let s = <&str>::deserialize(de)?;
        let bytes = hex::decode(s).map_err(serde::de::Error::custom)?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|v: Vec<u8>| serde::de::Error::custom(format!("expected 32 bytes, got {}", v.len())))?;
        Ok(Self(arr))
    }
}

/// One published index row (one per snapshot per index, §5).
///
/// `value` is in volatility points (`BVOL = 100·√σ²_30d`, §4.7).
/// `confidence` is bounded to `[0.0, 1.0]`; the engine assigns it from the
/// snapshot's data-quality signals (strike count, IV-fit residual, etc.).
/// `ts` is the snapshot timestamp (bar open), not the engine wall-clock (§5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexValue {
    pub index_id: IndexId,
    pub value: f64,
    pub confidence: f64,
    pub strip_hash: StripHash,
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
}
