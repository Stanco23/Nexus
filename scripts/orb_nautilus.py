#!/usr/bin/env python3
"""
ORB Backtest — Nautilus Python (Standalone)
============================================
Standalone Python implementation of the Opening Range Breakout strategy.
Mirrors the Rust orb_backtest.rs logic exactly.

Usage:
    python3 scripts/orb_nautilus.py --csv ./tvc_data/BTCUSDT_2025-01-01.csv --output results_orb_nautilus.csv
"""

import argparse
import csv
import sys
import time
from collections import defaultdict


# ─────────────────────────────────────────────────────────────────────────────
# ORB Config (must match orb_backtest.rs OrbConfig)
# ─────────────────────────────────────────────────────────────────────────────

class OrbConfig:
    def __init__(self):
        self.instrument_id = "BTCUSDT.BINANCE"
        self.opening_window_minutes = 5.0    # 5-minute opening range
        self.opening_start_hour = 9.5         # 9:30 AM EST
        self.session_end_hour = 16.0          # 4:00 PM EST
        self.atr_multiplier = 1.5
        self.position_size = 1.0              # 1 BTC — matches Rust
        self.tick_size = 0.01
        self.initial_equity = 100_000.0
        self.commission = 0.0004             # 0.04% per side (same as Rust)


# ─────────────────────────────────────────────────────────────────────────────
# ORB Strategy (mirrors Rust orb_backtest.rs OrbStrategy)
# ─────────────────────────────────────────────────────────────────────────────

class OrbStrategy:
    def __init__(self, config: OrbConfig):
        self.c = config
        self.orb_high = None
        self.orb_low = None
        self.orb_armed = False
        self.position_open = False
        self.entry_price = None
        self.stop_price = None
        self.take_profit = None
        self.position_side = "flat"  # long / short / flat

    def opening_end_hour(self) -> float:
        return self.c.opening_start_hour + self.c.opening_window_minutes / 60.0

    @staticmethod
    def unix_ns_to_est_hour(unix_ns: int) -> float:
        """Convert Unix nanoseconds to hour-of-day in EST (UTC-5, no DST)."""
        unix_sec = unix_ns / 1_000_000_000.0
        est_sec = (unix_sec + 5.0 * 3600.0) % 86400.0
        return est_sec / 3600.0

    def round_to_tick(self, price: float) -> float:
        return round(price / self.c.tick_size) * self.c.tick_size

    def on_trade(self, instrument_id: str, timestamp_ns: int, price: float, size: float) -> str:
        """
        Returns: "buy", "sell", or "close"
        Mirrors the Rust PortfolioStrategy::on_trade logic.
        """
        if instrument_id != self.c.instrument_id:
            return "close"

        hour = self.unix_ns_to_est_hour(timestamp_ns)
        opening_end = self.opening_end_hour()

        # Phase 1: Build opening range
        if not self.orb_armed and hour < opening_end:
            if self.orb_high is None or price > self.orb_high:
                self.orb_high = price
            if self.orb_low is None or price < self.orb_low:
                self.orb_low = price
            return "close"

        # Phase 2: Arm ORB when window closes
        if not self.orb_armed and hour >= opening_end:
            if self.orb_high is not None and self.orb_low is not None:
                self.orb_armed = True
            return "close"

        # Phase 3: Only trade during regular session
        if hour < opening_end or hour >= self.c.session_end_hour:
            return "close"

        # Phase 4: Entry
        if not self.position_open:
            range_width = self.orb_high - self.orb_low
            tick = self.c.tick_size

            if price > self.orb_high:
                self.entry_price = price
                self.stop_price = self.orb_low - tick
                self.take_profit = self.round_to_tick(price + range_width * self.c.atr_multiplier)
                self.position_open = True
                self.position_side = "long"
                return "buy"

            if price < self.orb_low:
                self.entry_price = price
                self.stop_price = self.orb_high + tick
                self.take_profit = self.round_to_tick(price - range_width * self.c.atr_multiplier)
                self.position_open = True
                self.position_side = "short"
                return "sell"

            return "close"

        # Phase 5: Exit
        if self.position_open:
            if self.entry_price is None:
                self.position_open = False
                self.position_side = "flat"
                return "close"

            if self.position_side == "long":
                pnl_ticks = (price - self.entry_price) / self.c.tick_size
                if pnl_ticks <= -1.0:
                    # Stop loss hit
                    self.position_open = False
                    self.position_side = "flat"
                    self.entry_price = None
                    return "close"
                tp_ticks = (self.take_profit - self.entry_price) / self.c.tick_size
                if pnl_ticks >= tp_ticks:
                    # Take profit hit
                    self.position_open = False
                    self.position_side = "flat"
                    self.entry_price = None
                    return "close"

            elif self.position_side == "short":
                pnl_ticks = (self.entry_price - price) / self.c.tick_size
                if pnl_ticks <= -1.0:
                    # Stop loss hit
                    self.position_open = False
                    self.position_side = "flat"
                    self.entry_price = None
                    return "close"
                tp_ticks = (self.entry_price - self.take_profit) / self.c.tick_size
                if pnl_ticks >= tp_ticks:
                    # Take profit hit
                    self.position_open = False
                    self.position_side = "flat"
                    self.entry_price = None
                    return "close"

        return "close"


# ─────────────────────────────────────────────────────────────────────────────
# Simple Portfolio (mirrors Rust Portfolio behavior)
# ─────────────────────────────────────────────────────────────────────────────

class SimplePortfolio:
    """
    Mirrors the Rust Portfolio + CommissionConfig behavior.
    - Commission charged once on open, once on close (same as Rust)
    - Equity tracks realized PnL only (no unrealized — positions held to close)
    """
    def __init__(self, initial_equity: float, commission: float):
        self.initial_equity = initial_equity
        self.equity = initial_equity
        self.commission = commission
        self.max_dd = 0.0
        self.peak = initial_equity
        self.total_trades = 0
        self.position = 0.0        # positive=long, negative=short
        self.position_side = "flat"
        self.entry_price = 0.0

    def open_position(self, price: float, size: float, side: str):
        """Open a position. Charges commission once (mirrors Rust open_position)."""
        comm = price * size * self.commission
        self.equity -= comm
        self.position = size if side == "long" else -size
        self.position_side = side
        self.entry_price = price
        self.total_trades += 1

    def close_position(self, price: float):
        """Close current position. Charges commission on exit (mirrors Rust close_position)."""
        if self.position_side == "flat":
            return
        comm = price * abs(self.position) * self.commission
        if self.position > 0.0:
            pnl = (price - self.entry_price) * abs(self.position)
        else:
            pnl = (self.entry_price - price) * abs(self.position)
        self.equity += pnl
        self.equity -= comm
        self.position = 0.0
        self.position_side = "flat"
        self.entry_price = 0.0
        # Update peak and drawdown
        if self.equity > self.peak:
            self.peak = self.equity
        dd = self.peak - self.equity
        if dd > self.max_dd:
            self.max_dd = dd


# ─────────────────────────────────────────────────────────────────────────────
# CSV Reader (Binance Data Archive — same source as Rust TVC3 ingestion)
# ─────────────────────────────────────────────────────────────────────────────

def read_binance_trades(csv_path: str):
    """
    Reads Binance Data Archive CSV.
    Format: id,price,qty,quote_qty,time,is_buyer_maker,is_self_trade
    time is in MICROSECONDS (Binance Data Archive standard).
    Convert to nanoseconds to match Rust BinanceFileIngestor.
    """
    with open(csv_path, 'r') as f:
        reader = csv.reader(f)
        for row in reader:
            if len(row) < 6:
                continue
            try:
                price = float(row[1])
                qty = float(row[2])
                time_us = int(row[4])   # microseconds
                # Convert µs → ns (same conversion as Rust BinanceFileIngestor)
                timestamp_ns = time_us * 1000
                yield timestamp_ns, price, qty
            except (ValueError, IndexError):
                continue


# ─────────────────────────────────────────────────────────────────────────────
# Main
# ─────────────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="ORB Backtest — Nautilus Python")
    parser.add_argument("--csv", required=True, help="Binance trades CSV")
    parser.add_argument("--output", default="results_orb_nautilus.csv", help="Output CSV")
    parser.add_argument("--equity", type=float, default=100_000.0, help="Initial equity")
    args = parser.parse_args()

    config = OrbConfig()
    config.initial_equity = args.equity
    strategy = OrbStrategy(config)
    portfolio = SimplePortfolio(initial_equity=args.equity, commission=config.commission)

    print(f"=== ORB Backtest — Nautilus Python ===")
    print(f"CSV:     {args.csv}")
    print(f"Equity:  ${args.equity:,.2f}")
    print(f"Pos size: {config.position_size} BTC")

    start = time.time()
    tick_count = 0
    prev_signal = "close"

    for ts_ns, price, size in read_binance_trades(args.csv):
        tick_count += 1
        signal = strategy.on_trade(config.instrument_id, ts_ns, price, size)

        # Only act on signal changes (same as Rust run_portfolio final_signal != last_sig)
        if signal == prev_signal:
            continue

        prev_signal = signal

        if signal == "buy" and portfolio.position_side != "long":
            if portfolio.position_side == "short":
                portfolio.close_position(price)
            portfolio.open_position(price, config.position_size, "long")

        elif signal == "sell" and portfolio.position_side != "short":
            if portfolio.position_side == "long":
                portfolio.close_position(price)
            portfolio.open_position(price, config.position_size, "short")

        elif signal == "close" and portfolio.position_side != "flat":
            portfolio.close_position(price)

    elapsed = time.time() - start
    pnl = portfolio.equity - portfolio.initial_equity

    print(f"\n=== Results ===")
    print(f"Total ticks:    {tick_count:,}")
    print(f"Total PnL:      ${pnl:,.2f}")
    print(f"Max Drawdown:   ${portfolio.max_dd:,.2f}")
    print(f"Total Trades:   {portfolio.total_trades}")
    print(f"Runtime:        {elapsed:.4f}s")
    print(f"Throughput:     {tick_count / elapsed:,.0f} ticks/sec")

    with open(args.output, 'w', newline='') as f:
        w = csv.writer(f)
        w.writerow(["metric", "value"])
        w.writerow(["instrument", config.instrument_id])
        w.writerow(["total_ticks", tick_count])
        w.writerow(["num_trades", portfolio.total_trades])
        w.writerow(["pnl", f"{pnl:.2f}"])
        w.writerow(["max_drawdown", f"{portfolio.max_dd:.2f}"])
        w.writerow(["runtime_sec", f"{elapsed:.4f}"])

    print(f"\nResults written to {args.output}")


if __name__ == "__main__":
    main()
