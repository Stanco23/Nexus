#!/usr/bin/env python3
"""
Convert a TVC-format CSV (timestamp_ns, price, quantity, side, trade_id)
into a binary TVC3 file.

TVC3 File Layout (byte-level):
    [0..127]           Header (128 bytes)
    [128..]            Tick data (N × 30 bytes each)
    [index_offset..]   Anchor index: num_anchors (4B u32) + M × AnchorIndexEntry (16B)
    [EOF-32..EOF]      SHA256 digest (32 bytes) — covers header + tick data + index

Header (128 bytes):
    0-3:     magic b"TVC3"
    4:       version u8 = 2
    5:       decimal_precision u8 = 9
    6-9:     anchor_interval u32
    10-13:   instrument_id u32 (FNV-1a of "BTCUSDT.BINANCE")
    14-21:   start_time_ns u64
    22-29:   end_time_ns u64
    30-37:   num_ticks u64
    38-41:   num_anchors u32
    42-49:   index_offset u64 (byte offset where anchor index starts)
    50-127:  reserved zeros (78 bytes)

Tick (30 bytes):
    0-7:   timestamp_ns u64
    8-15:  price_int i64 (price × 1e9, little-endian)
    16-23: size_int i64 (qty × 1e9)
    24:    side u8 (0=BUY, 1=SELL)
    25:    flags u8 (1=trade)
    26-29: sequence u32

AnchorIndexEntry (16 bytes):
    0-7:   tick_index u64 (cumulative tick number)
    8-15:  byte_offset u64 (byte position of this anchor in file)

Usage:
    python3 scripts/csv_to_tvc.py data/BTCUSDT_2025-01-02.csv
    python3 scripts/csv_to_tvc.py data/BTCUSDT_2025-01-02.csv -o out.tvc --anchor-interval 1000
"""

import argparse
import csv
import struct
import os
import hashlib
from pathlib import Path

HEADER_SIZE = 128
TICK_SIZE = 30
ANCHOR_INDEX_ENTRY_SIZE = 16
ANCHOR_INTERVAL_DEFAULT = 1000
DECIMAL_PRECISION = 9  # 9 decimal places (nano-integer)


def fnv1a_hash(data: bytes) -> int:
    """32-bit FNV-1a hash of a byte string."""
    h = 0x811c9dc5
    for byte in data:
        h ^= byte
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h


def write_tvc(csv_path: str, output_path: str, anchor_interval: int):
    """Convert TVC CSV → binary TVC3 file."""
    rows = []
    with open(csv_path, 'r', newline='') as f:
        reader = csv.reader(f)
        header_row = next(reader)
        print(f"CSV header: {header_row}")

        for row in reader:
            if len(row) < 5:
                continue
            ts_ns = int(row[0])
            price = float(row[1])
            qty = float(row[2])
            side = 0 if row[3].strip().upper() == 'BUY' else 1
            rows.append((ts_ns, price, qty, side))

    print(f"Loaded {len(rows):,} trades")

    num_ticks = len(rows)
    num_anchors = (num_ticks + anchor_interval - 1) // anchor_interval
    tick_data_size = num_ticks * TICK_SIZE
    index_size = 4 + num_anchors * ANCHOR_INDEX_ENTRY_SIZE  # num_anchors u32 + entries
    index_offset = HEADER_SIZE + tick_data_size
    file_size_no_digest = index_offset + index_size
    file_size = file_size_no_digest + 32  # + SHA256

    instrument_str = "BTCUSDT.BINANCE"
    id_hash = fnv1a_hash(instrument_str.encode('utf-8'))
    start_time_ns = rows[0][0]
    end_time_ns = rows[-1][0]

    print(f"TVC3: {num_ticks:,} ticks, {num_anchors:,} anchors, "
          f"anchor_interval={anchor_interval}")
    print(f"  tick_data: {tick_data_size:,} bytes")
    print(f"  index_offset: {index_offset:,}")
    print(f"  index: {index_size:,} bytes (4 + {num_anchors} × 16)")
    print(f"  file_size: {file_size:,} bytes ({file_size / 1024 / 1024:.2f} MB)")

    os.makedirs(os.path.dirname(output_path) or '.', exist_ok=True)

    # ── Build header bytes ───────────────────────────────────────────────────
    header = struct.pack(
        '<4s B B I I Q Q Q I Q 78x',
        b'TVC3',
        2,                 # version
        DECIMAL_PRECISION,  # decimal_precision
        anchor_interval,
        id_hash,
        start_time_ns,
        end_time_ns,
        num_ticks,
        num_anchors,
        index_offset,
    )
    assert len(header) == HEADER_SIZE, f"Header {len(header)} != {HEADER_SIZE}"

    # ── Build tick data ───────────────────────────────────────────────────────
    tick_data = bytearray(tick_data_size)
    for i, (ts_ns, price, qty, side) in enumerate(rows):
        price_int = round(price * 1e9)
        size_int = round(qty * 1e9)
        pos = i * TICK_SIZE
        tick_data[pos:pos+8] = struct.pack('<Q', ts_ns)
        tick_data[pos+8:pos+16] = struct.pack('<q', price_int)
        tick_data[pos+16:pos+24] = struct.pack('<q', size_int)
        tick_data[pos+24] = side
        tick_data[pos+25] = 1  # flags = trade
        tick_data[pos+26:pos+30] = struct.pack('<I', i)  # sequence

    # ── Build anchor index ────────────────────────────────────────────────────
    anchor_index_data = bytearray(index_size)
    struct.pack_into('<I', anchor_index_data, 0, num_anchors)  # num_anchors at offset 0
    for m in range(num_anchors):
        tick_index = m * anchor_interval
        byte_offset = HEADER_SIZE + tick_index * TICK_SIZE
        pos = 4 + m * ANCHOR_INDEX_ENTRY_SIZE
        struct.pack_into('<Q', anchor_index_data, pos, tick_index)
        struct.pack_into('<Q', anchor_index_data, pos + 8, byte_offset)

    # ── Compute SHA256 over header + tick_data + anchor_index_data ─────────────
    sha256 = hashlib.sha256()
    sha256.update(header)
    sha256.update(tick_data)
    sha256.update(anchor_index_data)
    digest = sha256.digest()
    assert len(digest) == 32

    # ── Write file ────────────────────────────────────────────────────────────
    with open(output_path, 'wb') as f:
        f.write(header)
        f.write(tick_data)
        f.write(anchor_index_data)
        f.write(digest)

    actual_size = os.path.getsize(output_path)
    print(f"Wrote: {output_path} ({actual_size:,} bytes = {actual_size / 1024 / 1024:.2f} MB)")
    print(f"  time range: {start_time_ns} → {end_time_ns}")
    print(f"  instrument_id: 0x{id_hash:08x} (FNV-1a of '{instrument_str}')")

    if actual_size != file_size:
        print(f"  WARNING: expected {file_size:,} bytes, got {actual_size:,}")


def main():
    parser = argparse.ArgumentParser(description="CSV → TVC3 binary converter")
    parser.add_argument("csv", help="Input TVC-format CSV")
    parser.add_argument("--output", "-o",
                        help="Output .tvc path (default: same CSV stem + .tvc)")
    parser.add_argument("--anchor-interval", type=int, default=ANCHOR_INTERVAL_DEFAULT,
                        help=f"Ticks between anchors (default {ANCHOR_INTERVAL_DEFAULT})")
    args = parser.parse_args()

    csv_path = Path(args.csv)
    output_path = args.output or str(csv_path.with_suffix('.tvc'))

    print(f"CSV:    {csv_path}")
    print(f"Output: {output_path}")
    print(f"Anchors every {args.anchor_interval} ticks\n")
    write_tvc(str(csv_path), output_path, args.anchor_interval)


if __name__ == "__main__":
    main()
