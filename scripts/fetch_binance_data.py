#!/usr/bin/env python3
"""
Download Binance BTCUSDT klines and convert to TVC3-compatible CSV.
Usage: python3 scripts/fetch_binance_data.py --days 7 --output tvc_data/BTCUSDT_2025-01-02.csv
"""

import argparse
import csv
import time
import requests
from datetime import datetime, timezone, timedelta

BINANCE_KLINES_URL = "https://api.binance.com/api/v3/klines"
SYMBOL = "BTCUSDT"
INTERVAL = "1m"
MAX_PER_REQUEST = 1000  # Binance limit

def fetch_klines(start_ms: int, end_ms: int):
    """Fetch klines from Binance API in chunks."""
    all_klines = []
    current = start_ms

    while current < end_ms:
        url = f"{BINANCE_KLINES_URL}?symbol={SYMBOL}&interval={INTERVAL}&startTime={current}&endTime={end_ms}&limit={MAX_PER_REQUEST}"
        resp = requests.get(url, timeout=30)
        resp.raise_for_status()
        data = resp.json()

        if not data:
            break

        all_klines.extend(data)
        print(f"  Fetched {len(data)} klines, total: {len(all_klines)}", flush=True)

        # Next batch: last timestamp + 1ms
        current = int(data[-1][0]) + 1

        # Rate limit protection
        time.sleep(0.2)

    return all_klines

def klines_to_csv(klines, output_path: str):
    """
    Convert Binance klines to TVC3 CSV format.
    Binance kline format: [open_time, open, high, low, close, volume, close_time, ...]
    We output: timestamp(ns), price, quantity, side, trade_id
    timestamp is open_time in milliseconds converted to nanoseconds.
    price = close price (most recent)
    quantity = volume
    side = BUY/SELL based on price movement
    """
    with open(output_path, 'w', newline='') as f:
        writer = csv.writer(f)
        writer.writerow(['timestamp', 'price', 'quantity', 'side', 'trade_id'])

        trade_id = 0
        prev_close = None

        for kline in klines:
            open_time_ms = int(kline[0])
            open_price = float(kline[1])
            high_price = float(kline[2])
            low_price = float(kline[3])
            close_price = float(kline[4])
            volume = float(kline[5])
            close_time_ms = int(kline[6])

            # Use close time as primary timestamp (nanoseconds)
            ts_ns = close_time_ms * 1_000_000

            # Determine side: BUY if close >= open, else SELL
            side = 'BUY' if close_price >= open_price else 'SELL'

            # Use close price as the price
            price = close_price

            writer.writerow([ts_ns, f"{price:.2f}", f"{volume:.6f}", side, trade_id])
            trade_id += 1
            prev_close = close_price

    return trade_id

def main():
    parser = argparse.ArgumentParser(description="Download Binance BTCUSDT klines")
    parser.add_argument("--output", default="BTCUSDT.csv", help="Output CSV path")
    parser.add_argument("--days", type=int, default=7, help="Number of days to fetch")
    parser.add_argument("--start-date", default="2025-01-02", help="Start date YYYY-MM-DD")
    args = parser.parse_args()

    # Parse start date
    start_dt = datetime.fromisoformat(args.start_date)
    start_dt = start_dt.replace(tzinfo=timezone.utc)
    start_ms = int(start_dt.timestamp() * 1000)

    # End date = start + days
    end_dt = start_dt + timedelta(days=args.days)
    end_ms = int(end_dt.timestamp() * 1000)

    print(f"=== Binance Data Fetcher ===")
    print(f"Symbol:    {SYMBOL}")
    print(f"Interval:  {INTERVAL}")
    print(f"Start:     {start_dt} ({start_ms} ms)")
    print(f"End:       {end_dt} ({end_ms} ms)")
    print(f"Days:      {args.days}")
    print(f"Output:    {args.output}")
    print()

    print("Fetching klines...")
    klines = fetch_klines(start_ms, end_ms)
    print(f"Total klines fetched: {len(klines)}")

    if not klines:
        print("No data fetched!")
        return

    print("Writing CSV...")
    count = klines_to_csv(klines, args.output)
    print(f"Wrote {count} rows to {args.output}")

    # Print time range
    first_ts = int(klines[0][0])
    last_ts = int(klines[-1][0])
    first_dt = datetime.fromtimestamp(first_ts / 1000, tz=timezone.utc)
    last_dt = datetime.fromtimestamp(last_ts / 1000, tz=timezone.utc)
    print(f"Time range: {first_dt} to {last_dt}")

if __name__ == "__main__":
    main()
