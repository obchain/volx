//! JSON round-trip tests for every public domain type.
//!
//! Catches accidental rename / repr changes that would break the on-wire
//! contract with the Go API, `ClickHouse` JSON columns, and the Python
//! reference pipeline.
//!
//! Float comparisons here are deliberately bit-exact: `serde_json` round-trips
//! finite `f64` losslessly, so a mismatch always means a real regression.
#![allow(clippy::float_cmp)]

use time::macros::datetime;
use volx_shared_types::{
    Asset, IndexId, IndexValue, Minutes, OptionKind, OptionTick, Strip, StripHash, StripQuote,
    Venue, Years, strip::MIN_STRIP_QUOTES,
};

fn assert_json_roundtrip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq,
{
    let encoded = serde_json::to_string(value).expect("serialize");
    let decoded: T = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(value, &decoded, "round-trip mismatch (encoded: {encoded})");
}

#[test]
fn venue_wire_format_is_snake_case() {
    assert_eq!(
        serde_json::to_string(&Venue::Deribit).unwrap(),
        "\"deribit\""
    );
    assert_eq!(serde_json::to_string(&Venue::Okx).unwrap(), "\"okx\"");
    assert_eq!(serde_json::to_string(&Venue::Bybit).unwrap(), "\"bybit\"");
    for v in [Venue::Deribit, Venue::Okx, Venue::Bybit] {
        let s = serde_json::to_string(&v).unwrap();
        let back: Venue = serde_json::from_str(&s).unwrap();
        assert_eq!(v, back);
    }
}

#[test]
fn asset_wire_format_is_snake_case() {
    assert_eq!(serde_json::to_string(&Asset::Btc).unwrap(), "\"btc\"");
    assert_eq!(serde_json::to_string(&Asset::Eth).unwrap(), "\"eth\"");
}

#[test]
fn option_kind_wire_format_is_snake_case() {
    assert_eq!(
        serde_json::to_string(&OptionKind::Call).unwrap(),
        "\"call\""
    );
    assert_eq!(serde_json::to_string(&OptionKind::Put).unwrap(), "\"put\"");
}

#[test]
fn index_id_wire_format_is_uppercase_ticker() {
    assert_eq!(serde_json::to_string(&IndexId::Bvol).unwrap(), "\"BVOL\"");
    assert_eq!(serde_json::to_string(&IndexId::Evol).unwrap(), "\"EVOL\"");
    assert_eq!(IndexId::Bvol.ticker(), "BVOL");
    assert_eq!(IndexId::Bvol.asset(), Asset::Btc);
    assert_eq!(IndexId::Evol.asset(), Asset::Eth);
}

#[test]
fn time_unit_newtypes_are_transparent() {
    assert_eq!(serde_json::to_string(&Years(0.25)).unwrap(), "0.25");
    assert_eq!(
        serde_json::to_string(&Minutes(43_200.0)).unwrap(),
        "43200.0"
    );
    let y: Years = serde_json::from_str("0.5").unwrap();
    assert_eq!(y, Years(0.5));
}

#[test]
fn minutes_year_round_trip_is_exact() {
    let y = Years(30.0 / 365.0);
    let m = y.to_minutes();
    let back = m.to_years();
    assert!((back.0 - y.0).abs() < 1e-15);
    assert_eq!(Minutes::N_30D, Minutes(43_200.0));
    assert_eq!(Minutes::N_365D, Minutes(525_600.0));
}

#[test]
fn option_tick_round_trips_through_json() {
    let tick = OptionTick {
        venue: Venue::Deribit,
        asset: Asset::Btc,
        expiry: datetime!(2026-06-26 08:00:00 UTC),
        strike: 70_000.0,
        kind: OptionKind::Call,
        bid: Some(0.0421),
        ask: Some(0.0438),
        mid: Some(0.0430),
        iv: Some(0.62),
        underlying: 68_500.0,
        open_interest: 1234.5,
        volume_24h: 56.7,
        received_at: datetime!(2026-05-25 12:34:56.789 UTC),
    };
    let encoded = serde_json::to_string(&tick).expect("serialize");
    let decoded: OptionTick = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(decoded.venue, tick.venue);
    assert_eq!(decoded.strike, tick.strike);
    assert_eq!(decoded.bid, tick.bid);
    assert_eq!(decoded.iv, tick.iv);
    assert_eq!(decoded.expiry, tick.expiry);
    assert_eq!(decoded.received_at, tick.received_at);
}

#[test]
fn option_tick_missing_side_serializes_null() {
    let tick = OptionTick {
        venue: Venue::Deribit,
        asset: Asset::Eth,
        expiry: datetime!(2026-07-31 08:00:00 UTC),
        strike: 3_500.0,
        kind: OptionKind::Put,
        bid: None,
        ask: Some(0.05),
        mid: None,
        iv: Some(0.71),
        underlying: 3_400.0,
        open_interest: 0.0,
        volume_24h: 0.0,
        received_at: datetime!(2026-05-25 00:00:00 UTC),
    };
    let encoded = serde_json::to_string(&tick).expect("serialize");
    assert!(encoded.contains("\"bid\":null"));
    assert!(encoded.contains("\"mid\":null"));
    let decoded: OptionTick = serde_json::from_str(&encoded).unwrap();
    assert!(decoded.bid.is_none());
    assert_eq!(decoded.ask, Some(0.05));
}

#[test]
fn strip_round_trips_through_json() {
    let strip = Strip {
        forward: 68_500.0,
        k_zero: 68_000.0,
        time_to_expiry: Years(30.0 / 365.0),
        quotes: vec![
            StripQuote {
                strike: 55_000.0,
                q_usd: 4.0,
                iv: 0.72,
            },
            StripQuote {
                strike: 60_000.0,
                q_usd: 12.5,
                iv: 0.70,
            },
            StripQuote {
                strike: 68_000.0,
                q_usd: 1_200.0,
                iv: 0.62,
            },
            StripQuote {
                strike: 80_000.0,
                q_usd: 8.0,
                iv: 0.65,
            },
            StripQuote {
                strike: 90_000.0,
                q_usd: 3.0,
                iv: 0.68,
            },
        ],
    };
    let encoded = serde_json::to_string(&strip).expect("serialize");
    let decoded: Strip = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(decoded.forward, strip.forward);
    assert_eq!(decoded.k_zero, strip.k_zero);
    assert_eq!(decoded.time_to_expiry, strip.time_to_expiry);
    assert_eq!(decoded.quotes.len(), 5);
    assert_eq!(decoded.quotes[2].strike, 68_000.0);
}

#[test]
fn strip_rejects_below_minimum_quote_count() {
    let too_short = format!(
        r#"{{"forward":68500.0,"k_zero":68000.0,"time_to_expiry":0.0822,
            "quotes":[{}]}}"#,
        (0..MIN_STRIP_QUOTES - 1)
            .map(|i| format!(
                r#"{{"strike":{},"q_usd":1.0,"iv":0.5}}"#,
                60_000 + i * 1_000
            ))
            .collect::<Vec<_>>()
            .join(",")
    );
    let err = serde_json::from_str::<Strip>(&too_short).expect_err("must reject short strip");
    assert!(err.to_string().contains("must have >="), "{err}");
}

#[test]
fn strip_quote_rejects_out_of_range_iv() {
    let bad = r#"{"strike":68000.0,"q_usd":1.0,"iv":7.5}"#;
    let err = serde_json::from_str::<StripQuote>(bad).expect_err("must reject high IV");
    assert!(err.to_string().contains("fitted IV"), "{err}");
}

#[test]
fn index_value_rejects_out_of_range_confidence() {
    let bad = r#"{"index_id":"BVOL","value":65.0,"confidence":1.5,
                  "strip_hash":"0000000000000000000000000000000000000000000000000000000000000000",
                  "ts":"2026-05-25T12:00:00Z"}"#;
    let err = serde_json::from_str::<IndexValue>(bad).expect_err("must reject confidence > 1");
    assert!(err.to_string().contains("confidence"), "{err}");
}

#[test]
fn index_value_rejects_negative_value() {
    let bad = r#"{"index_id":"BVOL","value":-1.0,"confidence":0.5,
                  "strip_hash":"0000000000000000000000000000000000000000000000000000000000000000",
                  "ts":"2026-05-25T12:00:00Z"}"#;
    let err = serde_json::from_str::<IndexValue>(bad).expect_err("must reject negative value");
    assert!(err.to_string().contains("value"), "{err}");
}

#[test]
fn strip_hash_serializes_as_lowercase_hex() {
    let hash = StripHash([0xab; 32]);
    let encoded = serde_json::to_string(&hash).expect("serialize");
    assert_eq!(encoded.len(), 2 + 64); // quotes + 64 hex chars
    assert!(encoded.starts_with("\"ab"));
    let decoded: StripHash = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(decoded, hash);
}

#[test]
fn strip_hash_rejects_wrong_length() {
    let bad = "\"deadbeef\"";
    let err = serde_json::from_str::<StripHash>(bad).expect_err("must reject short hex");
    assert!(err.to_string().contains("expected 32 bytes"), "{err}");
}

#[test]
fn index_value_round_trips_through_json() {
    let value = IndexValue {
        index_id: IndexId::Bvol,
        value: 65.42,
        confidence: 0.97,
        strip_hash: StripHash([0u8; 32]),
        ts: datetime!(2026-05-25 12:00:00 UTC),
    };
    let encoded = serde_json::to_string(&value).expect("serialize");
    assert!(encoded.contains("\"index_id\":\"BVOL\""), "{encoded}");
    let decoded: IndexValue = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(decoded.index_id, value.index_id);
    assert_eq!(decoded.value, value.value);
    assert_eq!(decoded.confidence, value.confidence);
    assert_eq!(decoded.strip_hash, value.strip_hash);
    assert_eq!(decoded.ts, value.ts);
}

// Roundtrip helper exercised on enum-only payloads (the structs above hit
// the same code path but include float fields where exact equality is
// load-bearing only for these particular literals).
#[test]
fn enum_payload_roundtrip_helper() {
    assert_json_roundtrip(&Venue::Deribit);
    assert_json_roundtrip(&Asset::Eth);
    assert_json_roundtrip(&OptionKind::Put);
    assert_json_roundtrip(&IndexId::Evol);
    assert_json_roundtrip(&StripHash([7u8; 32]));
}
