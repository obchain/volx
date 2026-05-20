#!/usr/bin/env python3
"""
Fetch Deribit options_chain snapshots from Tardis.dev free tier (first-of-month).

Streams the daily compressed CSV (~2.3 GB), filters BTC/ETH coin-margined options,
resamples to per-minute snapshots (last tick per symbol per minute bucket),
writes one Parquet file per (currency, date).

Free tier covers first-of-month dates only; other dates require a Tardis API key
(pass --allow-paid-tier to bypass the date check if you have one configured).

Usage:
    python research/scripts/tardis_fetch.py --date 2024-06-01 \\
        --currencies BTC,ETH --cadence-sec 60 \\
        --out-dir research/data/tardis
"""
from __future__ import annotations

import argparse
import csv
import datetime as dt
import gzip
import io
import sys
import time
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import requests

URL_TEMPLATE = (
    "https://datasets.tardis.dev/v1/deribit/options_chain/"
    "{year:04d}/{month:02d}/{day:02d}/OPTIONS.csv.gz"
)

# bid_iv / ask_iv intentionally omitted — the VIX-style integral uses mark mid-price
# directly, so only mark_iv is retained for sanity comparison against Deribit's pricer.
SCHEMA = pa.schema([
    ("timestamp", pa.timestamp("us", tz="UTC")),
    ("symbol", pa.string()),
    ("currency", pa.string()),
    ("type", pa.string()),
    ("strike_price", pa.float64()),
    ("expiration", pa.timestamp("us", tz="UTC")),
    ("bid_price", pa.float64()),
    ("bid_amount", pa.float64()),
    ("ask_price", pa.float64()),
    ("ask_amount", pa.float64()),
    ("mark_price", pa.float64()),
    ("mark_iv", pa.float64()),
    ("underlying_price", pa.float64()),
])


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument(
        "--date",
        required=True,
        help="YYYY-MM-DD (must be first-of-month unless --allow-paid-tier is set)",
    )
    p.add_argument(
        "--currencies",
        default="BTC,ETH",
        help="Comma-separated coin-margined currencies (default: BTC,ETH)",
    )
    p.add_argument(
        "--cadence-sec",
        type=int,
        default=60,
        help="Snapshot cadence in seconds (default: 60 = 1-minute)",
    )
    p.add_argument(
        "--out-dir",
        default="research/data/tardis",
        help="Output dir (default: research/data/tardis)",
    )
    p.add_argument(
        "--debug-max-rows",
        type=int,
        default=None,
        help=(
            "Stop after N raw rows (smoke-test only). When set, output filename "
            "becomes snapshot.partial.parquet so downstream globs do not mistake "
            "a debug run for a full-day fetch."
        ),
    )
    p.add_argument(
        "--allow-paid-tier",
        action="store_true",
        help="Skip the first-of-month date check (requires Tardis API key configured)",
    )
    return p.parse_args()


def safe_float(s: str | None) -> float | None:
    if s is None or s == "":
        return None
    try:
        return float(s)
    except ValueError:
        return None


def safe_int(s: str | None) -> int | None:
    if s is None or s == "":
        return None
    try:
        return int(s)
    except (ValueError, TypeError):
        return None


def to_utc(us: int | None) -> dt.datetime | None:
    if us is None:
        return None
    return dt.datetime.fromtimestamp(us / 1_000_000, tz=dt.timezone.utc)


def fetch_and_resample(
    date: dt.date,
    currencies: list[str],
    cadence_sec: int,
    out_dir: Path,
    debug_max_rows: int | None,
) -> dict[str, int]:
    url = URL_TEMPLATE.format(year=date.year, month=date.month, day=date.day)
    print(f"GET {url}", file=sys.stderr, flush=True)

    bucket_us = cadence_sec * 1_000_000
    writers: dict[str, pq.ParquetWriter] = {}
    paths: dict[str, Path] = {}
    counts: dict[str, int] = {c: 0 for c in currencies}
    # active_bucket only advances on rows for that currency. An illiquid currency
    # with a long tick gap will hold its pending dict open until the next tick
    # arrives (or the finally block runs at end-of-stream). BTC/ETH on Deribit
    # tick every second so this is a non-issue for the supported scope; revisit
    # if --currencies is extended to thinly-traded altcoin options.
    active_bucket: dict[str, int | None] = {c: None for c in currencies}
    pending: dict[str, dict[str, dict[str, str]]] = {c: {} for c in currencies}

    partial_suffix = ".partial" if debug_max_rows is not None else ""
    for currency in currencies:
        outdir = out_dir / currency.lower() / date.isoformat()
        outdir.mkdir(parents=True, exist_ok=True)
        path = outdir / f"snapshot{partial_suffix}.parquet"
        paths[currency] = path
        writers[currency] = pq.ParquetWriter(path, SCHEMA, compression="snappy")

    def flush(currency: str, bucket_start_us: int) -> None:
        rows = list(pending[currency].values())
        if not rows:
            return
        ts_dt = to_utc(bucket_start_us)
        cols: dict[str, list] = {f.name: [] for f in SCHEMA}
        for r in rows:
            cols["timestamp"].append(ts_dt)
            cols["symbol"].append(r["symbol"])
            cols["currency"].append(currency)
            cols["type"].append(r["type"])
            cols["strike_price"].append(safe_float(r["strike_price"]))
            cols["expiration"].append(to_utc(safe_int(r["expiration"])))
            cols["bid_price"].append(safe_float(r["bid_price"]))
            cols["bid_amount"].append(safe_float(r["bid_amount"]))
            cols["ask_price"].append(safe_float(r["ask_price"]))
            cols["ask_amount"].append(safe_float(r["ask_amount"]))
            cols["mark_price"].append(safe_float(r["mark_price"]))
            cols["mark_iv"].append(safe_float(r["mark_iv"]))
            cols["underlying_price"].append(safe_float(r["underlying_price"]))
        table = pa.Table.from_pydict(cols, schema=SCHEMA)
        writers[currency].write_table(table)
        counts[currency] += len(rows)
        pending[currency].clear()

    prefixes = tuple(f"{c}-" for c in currencies)
    t_start = time.monotonic()
    raw_rows = 0
    last_log = t_start

    try:
        # timeout=(connect, read): read=None disables the per-socket-read timer so
        # a transient stall on the long-running stream does not abort the download.
        with requests.get(url, stream=True, timeout=(30, None)) as resp:
            resp.raise_for_status()
            # Belt-and-suspenders: make sure urllib3 does not transparently decompress.
            # Tardis serves the gzip at the file level, so we decompress ourselves
            # via GzipFile. Setting decode_content=False on resp.raw guards against
            # accidental gzip-over-gzip if the server ever adds Content-Encoding: gzip.
            resp.raw.decode_content = False
            gz = gzip.GzipFile(fileobj=resp.raw)
            text = io.TextIOWrapper(gz, encoding="utf-8", newline="")
            reader = csv.DictReader(text)
            for row in reader:
                raw_rows += 1
                sym = row["symbol"]
                if not sym.startswith(prefixes):
                    continue
                # match exact currency prefix (e.g. BTC- not BTC_USDC-)
                currency = sym.split("-", 1)[0]
                if currency not in counts:
                    continue
                ts_us = int(row["timestamp"])
                bucket_start = (ts_us // bucket_us) * bucket_us
                prev = active_bucket[currency]
                if prev is None:
                    active_bucket[currency] = bucket_start
                elif bucket_start > prev:
                    # forward advance: emit the closed bucket, open the new one
                    flush(currency, prev)
                    active_bucket[currency] = bucket_start
                elif bucket_start < prev:
                    # late arrival (Tardis is sorted by local_timestamp, not
                    # exchange timestamp). Absorb into the still-open bucket
                    # rather than re-opening a flushed one — the per-symbol dict
                    # below will overwrite or insert accordingly.
                    pass
                pending[currency][sym] = row

                if debug_max_rows is not None and raw_rows >= debug_max_rows:
                    print(
                        f"hit --debug-max-rows={debug_max_rows}, stopping early",
                        file=sys.stderr,
                    )
                    break

                now = time.monotonic()
                if now - last_log >= 10.0:
                    last_log = now
                    elapsed = now - t_start
                    rps = raw_rows / elapsed if elapsed > 0 else 0
                    kept = " ".join(f"{c}={counts[c]:,}" for c in currencies)
                    print(
                        f"  ... {raw_rows:>12,} raw rows  "
                        f"{elapsed:>6.0f}s  "
                        f"{rps:>10,.0f} rows/s  "
                        f"kept: {kept}",
                        file=sys.stderr,
                        flush=True,
                    )
    finally:
        for c in currencies:
            b = active_bucket[c]
            if b is not None:
                flush(c, b)
            writers[c].close()

    elapsed = time.monotonic() - t_start
    print(
        f"done: {raw_rows:,} raw rows in {elapsed:.1f}s "
        f"({raw_rows / max(elapsed, 1e-9):,.0f} rows/s)",
        file=sys.stderr,
    )
    for c in currencies:
        size_mb = paths[c].stat().st_size / 1024 / 1024
        print(f"  {c}: {counts[c]:>8,} rows  {size_mb:>7.2f} MB  → {paths[c]}", file=sys.stderr)
    return counts


def main() -> None:
    args = parse_args()
    date = dt.date.fromisoformat(args.date)
    if date.day != 1 and not args.allow_paid_tier:
        print(
            f"ERROR: {date} is not first-of-month. The Tardis free tier covers "
            f"only first-of-month dates; other dates return 404 without an API "
            f"key. Pass --allow-paid-tier to override (requires API key).",
            file=sys.stderr,
        )
        sys.exit(2)
    currencies = [c.strip().upper() for c in args.currencies.split(",") if c.strip()]
    fetch_and_resample(
        date, currencies, args.cadence_sec, Path(args.out_dir), args.debug_max_rows
    )


if __name__ == "__main__":
    main()
