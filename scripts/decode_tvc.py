#!/usr/bin/env python3
"""Decode TVC v3 and verify against CSV."""
import struct
import csv
import os

def read_tvc(path):
    with open(path, 'rb') as f:
        data = f.read()
    
    # Header (128 bytes)
    magic = data[0:4]
    version = data[4]
    decimal_precision = data[5]
    anchor_interval = struct.unpack_from('<I', data, 6)[0]
    instrument_id = struct.unpack_from('<I', data, 10)[0]
    start_time_ns = struct.unpack_from('<Q', data, 14)[0]
    end_time_ns = struct.unpack_from('<Q', data, 22)[0]
    num_ticks = struct.unpack_from('<Q', data, 30)[0]
    num_anchors = struct.unpack_from('<I', data, 38)[0]
    index_offset = struct.unpack_from('<Q', data, 42)[0]
    
    print(f"Version: {version}")
    print(f"Decimal precision: {decimal_precision}")
    print(f"Anchor interval: {anchor_interval}")
    print(f"Num ticks: {num_ticks}, Num anchors: {num_anchors}")
    
    # Decode anchor index (16 bytes per entry: 8B tick_index + 8B byte_offset)
    pos = index_offset
    num_anchors_index = struct.unpack_from('<I', data, pos)[0]
    pos += 4
    
    anchor_positions = []
    anchor_tick_indices = []
    for i in range(num_anchors_index):
        tick_index = struct.unpack_from('<Q', data, pos)[0]
        byte_offset = struct.unpack_from('<Q', data, pos + 8)[0]
        pos += 16  # 16 bytes per entry
        anchor_positions.append(byte_offset)
        anchor_tick_indices.append(tick_index)
    
    print(f"\nAnchor positions (absolute file offsets):")
    for i, (tick_idx, byte_off) in enumerate(zip(anchor_tick_indices, anchor_positions)):
        print(f"  Anchor {i}: tick_index={tick_idx}, byte_offset={byte_off}")
    
    # Read anchors
    anchors = []
    for byte_off in anchor_positions:
        ts = struct.unpack_from('<Q', data, byte_off)[0]
        price = struct.unpack_from('<q', data, byte_off + 8)[0]
        size = struct.unpack_from('<q', data, byte_off + 16)[0]
        side = data[byte_off + 24]
        flags = data[byte_off + 25]
        sequence = struct.unpack_from('<I', data, byte_off + 26)[0]
        anchors.append({'ts': ts, 'price': price, 'size': size, 'side': side, 'flags': flags, 'seq': sequence})
    
    print(f"\nAnchor 0: ts={anchors[0]['ts']}, price={anchors[0]['price']}, size={anchors[0]['size']}, side={anchors[0]['side']}")
    print(f"Anchors: {len(anchors)}")
    
    # Decode deltas between anchors
    decoded_ticks = []
    
    for anchor_idx in range(len(anchors) - 1):
        start_anchor = anchors[anchor_idx]
        end_anchor_tick_idx = anchor_tick_indices[anchor_idx + 1]
        next_anchor_pos = anchor_positions[anchor_idx + 1]
        
        # Start from anchor
        prev_tick = start_anchor.copy()
        tick_seq = start_anchor['seq']
        
        # Decode deltas until next anchor
        pos = anchor_positions[anchor_idx] + 30  # after anchor
        end_pos = next_anchor_pos
        
        while pos < end_pos:
            if data[pos] == 0xFF:
                # 12-byte overflow
                ts_extra_raw = struct.unpack_from('<H', data, pos + 1)[0]
                price_extra = struct.unpack_from('<q', data, pos + 3)[0]
                size_byte = data[pos + 11]
                
                if ts_extra_raw & 0x8000 == 0:
                    ts_extra = ts_extra_raw >> 1
                else:
                    ts_extra = (ts_extra_raw & 0x7FFF) << 21
                
                ts = prev_tick['ts'] + ts_extra
                price = prev_tick['price'] + price_extra
                side = (size_byte >> 7) & 1
                size_sign = -1 if (size_byte & 0x40) else 1
                size_mag = size_byte & 0x3F
                size = prev_tick['size'] + size_sign * size_mag
                
                pos += 12
            else:
                # 4-byte base
                packed = struct.unpack_from('<I', data, pos)[0]
                ts_delta = packed & 0xFFFFF
                price_zigzag = (packed >> 20) & 0x7FFFF
                
                if price_zigzag & 1:
                    price_delta = -(price_zigzag >> 1)
                else:
                    price_delta = price_zigzag >> 1
                
                ts = prev_tick['ts'] + ts_delta
                price = prev_tick['price'] + price_delta
                side = prev_tick['side']
                size = prev_tick['size']
                
                pos += 4
            
            tick_seq += 1
            decoded_ticks.append({'ts': ts, 'price': price, 'size': size, 'side': side, 'seq': tick_seq})
            prev_tick = decoded_ticks[-1].copy()
    
    # Decode remaining ticks after last anchor
    last_anchor = anchors[-1]
    prev_tick = last_anchor.copy()
    tick_seq = last_anchor['seq']
    
    pos = anchor_positions[-1] + 30
    while pos < index_offset:
        if data[pos] == 0xFF:
            ts_extra_raw = struct.unpack_from('<H', data, pos + 1)[0]
            price_extra = struct.unpack_from('<q', data, pos + 3)[0]
            size_byte = data[pos + 11]
            
            if ts_extra_raw & 0x8000 == 0:
                ts_extra = ts_extra_raw >> 1
            else:
                ts_extra = (ts_extra_raw & 0x7FFF) << 21
            
            ts = prev_tick['ts'] + ts_extra
            price = prev_tick['price'] + price_extra
            side = (size_byte >> 7) & 1
            size_sign = -1 if (size_byte & 0x40) else 1
            size_mag = size_byte & 0x3F
            size = prev_tick['size'] + size_sign * size_mag
            
            pos += 12
        else:
            packed = struct.unpack_from('<I', data, pos)[0]
            ts_delta = packed & 0xFFFFF
            price_zigzag = (packed >> 20) & 0x7FFFF
            
            if price_zigzag & 1:
                price_delta = -(price_zigzag >> 1)
            else:
                price_delta = price_zigzag >> 1
            
            ts = prev_tick['ts'] + ts_delta
            price = prev_tick['price'] + price_delta
            side = prev_tick['side']
            size = prev_tick['size']
            
            pos += 4
        
        tick_seq += 1
        decoded_ticks.append({'ts': ts, 'price': price, 'size': size, 'side': side, 'seq': tick_seq})
        prev_tick = decoded_ticks[-1].copy()
    
    return decoded_ticks, decimal_precision, anchors

# Test
decoded, precision, anchors = read_tvc('tvc_data/BTCUSDT_2025-01-02_v3.tvc')
print(f"\nDecoded {len(decoded)} ticks")
print(f"First decoded: {decoded[0]}")
print(f"Last decoded: {decoded[-1]}")

# Compare with CSV at precision=1e6
csv_rows = list(csv.DictReader(open('tvc_data/BTCUSDT_2025-01-02.csv')))
csv_first_price = int(float(csv_rows[0]['price']) * 1_000_000)
csv_first_size = int(float(csv_rows[0]['quantity']) * 1_000_000)

print(f"\nCSV first: price_int={csv_first_price}, size_int={csv_first_size}")
print(f"Decoded first: price={decoded[0]['price']}, size={decoded[0]['size']}")
print(f"Match: {decoded[0]['price'] == csv_first_price}")

# Check first 10
print("\nFirst 10 ticks comparison:")
for i in range(min(10, len(decoded))):
    csv_price = int(float(csv_rows[i]['price']) * 1_000_000)
    csv_size = int(float(csv_rows[i]['quantity']) * 1_000_000)
    diff_price = decoded[i]['price'] - csv_price
    diff_size = decoded[i]['size'] - csv_size
    status = "✓" if diff_price == 0 and diff_size == 0 else "✗"
    print(f"  Tick {i}: decoded={decoded[i]['price']}, csv={csv_price}, diff={diff_price} {status}")

# Average file size
file_size = os.path.getsize('tvc_data/BTCUSDT_2025-01-02_v3.tvc')
avg_bytes = file_size / len(decoded)
print(f"\nFile size: {file_size}, Ticks: {len(decoded)}")
print(f"Avg bytes/tick: {avg_bytes:.2f}")