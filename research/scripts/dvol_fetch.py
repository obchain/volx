#!/usr/bin/env python3
"""
Fetch Deribit DVOL (Deribit Volatility Index) historical OHLC time series.

DVOL is Deribit's published crypto volatility index, computed at 1-min granularity
24/7 from their option chains. We pull it as hourly candles to use as the ground-
truth target for the offline VIX-style backtest (#5).

The Deribit `resolution` parameter is in SECONDS (their docs say "minutes" but the
API behavior is seconds — verified empirically). Pass 3600 for hourly candles.

Usage:
    python research/scripts/dvol_fetch.py --currencies BTC,ETH \\
        --start-date 2024-05-20 --end-date 2026-05-20 \\
        --resolution-sec 3600 --out-dir research/data/dvol
"""
from __future__ import annotations

import argparse
import datetime as dt
import sys
import time
from pathlib import Path

import pandas as pd
import requests

ENDPOINT = "https://www.deribit.com/api/v2/public/get_volatility_index_data"
PAGE_LIMIT = 1000  # API caps each response at 1000 candles


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument(
        "--currencies",
        default="BTC,ETH",
        help="Comma-separated currencies (default: BTC,ETH)",
    )
    p.add_argument(
        "--start-date",
        required=True,
        help="YYYY-MM-DD (inclusive, UTC)",
    )
    p.add_argument(
        "--end-date",
        required=True,
        help="YYYY-MM-DD (exclusive, UTC)",
    )
    p.add_argument(
        "--resolution-sec",
        type=int,
        default=3600,
        help="Candle resolution in seconds (default: 3600 = hourly)",
    )
    p.add_argument(
        "--out-dir",
        default="research/data/dvol",
        help="Output dir (default: research/data/dvol)",
    )
    p.add_argument(
        "--sleep-ms",
        type=int,
        default=100,
        help="Sleep between paginated calls in milliseconds (default: 100)",
    )
    return p.parse_args()


def to_ms(d: dt.date) -> int:
    return int(dt.datetime(d.year, d.month, d.day, tzinfo=dt.timezone.utc).timestamp() * 1000)


def fetch_currency(
    currency: str,
    start_ms: int,
    end_ms: int,
    resolution_sec: int,
    sleep_ms: int,
) -> pd.DataFrame:
    rows: list[list[float]] = []
    cursor_end = end_ms
    prev_cursor_end: int | None = None
    page = 0
    while True:
        page += 1
        params = {
            "currency": currency,
            "start_timestamp": start_ms,
            "end_timestamp": cursor_end,
            "resolution": resolution_sec,
        }
        resp = requests.get(ENDPOINT, params=params, timeout=(15, 60))
        resp.raise_for_status()
        body = resp.json()
        result = body.get("result", {})
        data = result.get("data", [])
        rows.extend(data)
        continuation = result.get("continuation")
        print(
            f"  {currency} page {page:>3}: +{len(data):>5} rows "
            f"(cursor_end={cursor_end}, continuation={continuation})",
            file=sys.stderr,
        )
        if not continuation or continuation <= start_ms or len(data) < PAGE_LIMIT:
            break
        # Defensive guard against a stale-cursor infinite loop: if the API ever
        # echoes back the same continuation as the cursor we just used, advance
        # would never happen and we'd request the same page forever.
        if continuation == cursor_end or continuation == prev_cursor_end:
            print(
                f"  {currency}: WARNING pagination cursor did not advance "
                f"(continuation={continuation}), breaking",
                file=sys.stderr,
            )
            break
        prev_cursor_end = cursor_end
        cursor_end = continuation
        time.sleep(sleep_ms / 1000)

    if not rows:
        return pd.DataFrame(columns=["timestamp", "open", "high", "low", "close"])

    df = pd.DataFrame(rows, columns=["timestamp", "open", "high", "low", "close"])
    df["timestamp"] = pd.to_datetime(df["timestamp"], unit="ms", utc=True)
    df = df.drop_duplicates(subset=["timestamp"]).sort_values("timestamp").reset_index(drop=True)
    df = df[(df["timestamp"] >= pd.Timestamp(start_ms, unit="ms", tz="UTC")) &
            (df["timestamp"] <  pd.Timestamp(end_ms,   unit="ms", tz="UTC"))]
    return df


def sanity_check(df: pd.DataFrame, currency: str, resolution_sec: int) -> None:
    if df.empty:
        print(f"  {currency}: WARNING empty dataframe", file=sys.stderr)
        return
    expected_step = pd.Timedelta(seconds=resolution_sec)
    diffs = df["timestamp"].diff().dropna()
    max_gap = diffs.max()
    n_gaps = (diffs > expected_step * 1.5).sum()
    closes = df["close"]
    print(
        f"  {currency}: rows={len(df):>6,}  "
        f"close min={closes.min():.2f} max={closes.max():.2f} mean={closes.mean():.2f}  "
        f"max_gap={max_gap}  n_gaps_>1.5x={n_gaps}",
        file=sys.stderr,
    )
    # Warnings are informational only; exit code stays 0 so callers decide.
    # For pipeline use, pipe stderr to a log and grep for 'WARNING'.
    if closes.min() < 1 or closes.max() > 500:
        print(f"  {currency}: WARNING close values outside [1, 500] sanity range", file=sys.stderr)
    if max_gap > expected_step * 24:
        print(f"  {currency}: WARNING max gap >24x expected step ({max_gap})", file=sys.stderr)


def main() -> None:
    args = parse_args()
    start_date = dt.date.fromisoformat(args.start_date)
    end_date = dt.date.fromisoformat(args.end_date)
    if end_date <= start_date:
        print(f"ERROR: --end-date must be after --start-date", file=sys.stderr)
        sys.exit(2)
    start_ms = to_ms(start_date)
    end_ms = to_ms(end_date)
    currencies = [c.strip().upper() for c in args.currencies.split(",") if c.strip()]
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    print(
        f"window: {start_date} → {end_date}  "
        f"resolution={args.resolution_sec}s  currencies={currencies}",
        file=sys.stderr,
    )

    for currency in currencies:
        t0 = time.monotonic()
        df = fetch_currency(currency, start_ms, end_ms, args.resolution_sec, args.sleep_ms)
        path = out_dir / f"{currency.lower()}.parquet"
        df.to_parquet(path, compression="snappy", index=False)
        elapsed = time.monotonic() - t0
        size_kb = path.stat().st_size / 1024
        print(
            f"  {currency}: wrote {len(df):>6,} rows  {size_kb:>7.1f} KB  "
            f"in {elapsed:.1f}s  → {path}",
            file=sys.stderr,
        )
        sanity_check(df, currency, args.resolution_sec)


if __name__ == "__main__":
    main()
