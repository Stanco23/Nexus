#!/usr/bin/env python3
"""
Download Binance BTCUSDT raw trades from the public historical archive.

URL pattern:
    https://data.binance.vision/data/spot/daily/trades/{symbol}/{symbol}-trades-{YYYY-MM-DD}.zip

Each ZIP contains a CSV:
    trade_id, price, qty, quoteQty, time, isBuyerMaker, isBestMatch

Timestamps are in **microseconds** for dates from Jan 1 2025 onwards.

Usage:
    python3 scripts/fetch_binance_data.py --date 2025-01-02 --output data/
"""

import argparse
import csv
import time
import zipfile
import io
import os
import requests
from datetime import datetime, timezone, timedelta

BASE_URL = "https://data.binance.vision/data/spot/daily/trades"
SYMBOL = "BTCUSDT"


def download_and_convert(date_str: str, output_dir: str) -> dict:
    """
    Download a daily trades ZIP and convert to TVC CSV.
    Returns dict with stats.
    """
    url = f"{BASE_URL}/{SYMBOL}/{SYMBOL}-trades-{date_str}.zip"
    print(f"Downloading: {url}")

    resp = requests.get(url, timeout=60)
    if resp.status_code == 404:
        print(f"  Not found (404) — no data for {date_str}")
        return None
    resp.raise_for_status()

    z = zipfile.ZipFile(io.BytesIO(resp.content))
    if len(z.namelist()) != 1:
        print(f"  Unexpected ZIP contents: {z.namelist()}")
        return None

    csv_name = z.namelist()[0]
    with z.open(csv_name) as f:
        content = f.read().decode('utf-8')

    # Parse trades
    reader = csv.reader(io.StringIO(content))
    header = next(reader)
    print(f"  ZIP header: {header}")

    rows_out = []
    first_ts = None
    last_ts = None
    trade_count = 0

    for row in reader:
        if len(row) < 6:
            continue
        # trade_id, price, qty, quoteQty, time, isBuyerMaker, isBestMatch
        try:
            trade_id = int(row[0])
            price = float(row[1])
            qty = float(row[2])
            time_us = int(row[4])   # microseconds
            is_buyer_maker = row[5].strip().lower() == 'true'
            # Convert microseconds → nanoseconds
            ts_ns = time_us * 1000
            # isBuyerMaker: true → buyer was maker → taker sold → SELL-side
            side = 'SELL' if is_buyer_maker else 'BUY'
            rows_out.append((ts_ns, price, qty, side, trade_id))
            if first_ts is None:
                first_ts = ts_ns
            last_ts = ts_ns
            trade_count += 1
        except (ValueError, IndexError):
            continue

    print(f"  {trade_count} trades, time range: {first_ts} → {last_ts}")

    # Write CSV
    os.makedirs(output_dir, exist_ok=True)
    csv_path = os.path.join(output_dir, f"{SYMBOL}_{date_str}.csv")
    with open(csv_path, 'w', newline='') as f:
        writer = csv.writer(f)
        writer.writerow(['timestamp', 'price', 'quantity', 'side', 'trade_id'])
        writer.writerows(rows_out)

    first_dt = datetime.utcfromtimestamp(first_ts / 1e9) if first_ts else None
    last_dt = datetime.utcfromtimestamp(last_ts / 1e9) if last_ts else None

    print(f"  Wrote: {csv_path} ({os.path.getsize(csv_path) / 1024:.1f} KB)")
    print(f"  Time range: {first_dt} → {last_dt}")

    return {
        'date': date_str,
        'csv_path': csv_path,
        'trade_count': trade_count,
        'first_ts': first_ts,
        'last_ts': last_ts,
    }


def main():
    parser = argparse.ArgumentParser(
        description="Download Binance BTCUSDT daily trades → TVC CSV"
    )
    parser.add_argument(
        "--date", required=True,
        help="Date YYYY-MM-DD (e.g. 2025-01-02)"
    )
    parser.add_argument(
        "--output", "-o", default=".",
        help="Output directory for CSV files"
    )
    args = parser.parse_args()

    print(f"=== Binance Raw Trades Fetcher ===")
    print(f"Symbol:  {SYMBOL}")
    print(f"Date:    {args.date}")
    print(f"Output:  {args.output}")
    print()

    result = download_and_convert(args.date, args.output)

    if result:
        print(f"\n✓ Downloaded {result['trade_count']} trades for {result['date']}")
    else:
        print("\n✗ No data available for this date")


if __name__ == "__main__":
    main()
