#!/usr/bin/env python3
"""Debug decode from anchor 0 to anchor 1."""
import struct

with open('tvc_data/BTCUSDT_2025-01-02_v3.tvc', 'rb') as f:
    data = f.read()

# Anchor 0 at pos 128, Anchor 1 at pos 12434
# But let me trace through all positions to verify

# Use binary search on anchor index
idx_offset = struct.unpack_from('<Q', data, 42)[0]
num_anchors = struct.unpack_from('<I', data, idx_offset)[0]
print(f'Index offset: {idx_offset}, num_anchors: {num_anchors}')

for i in range(num_anchors):
    tick_index = struct.unpack_from('<Q', data, idx_offset + 4 + i*16)[0]
    byte_offset = struct.unpack_from('<Q', data, idx_offset + 4 + i*16 + 8)[0]
    print(f'Anchor {i}: tick_index={tick_index}, byte_offset={byte_offset}')

# Decode from anchor 0 through to anchor 1
anchor0_pos = 128
prev_ts = struct.unpack_from('<Q', data, anchor0_pos)[0]
prev_price = struct.unpack_from('<q', data, anchor0_pos + 8)[0]
prev_size = struct.unpack_from('<q', data, anchor0_pos + 16)[0]
prev_side = data[anchor0_pos + 24]

print(f'\nAnchor 0 at {anchor0_pos}: ts={prev_ts}, price={prev_price}')

# Trace through first 5 deltas
pos = anchor0_pos + 30  # start of data after anchor 0
for tick in range(1, 6):
    if data[pos] == 0xFF:
        ts_raw = struct.unpack_from('<H', data, pos + 1)[0]
        price_extra = struct.unpack_from('<q', data, pos + 3)[0]
        size_byte = data[pos + 11]

        marker = (ts_raw >> 15) & 1
        if marker == 0:
            ts_extra = ts_raw >> 1
        else:
            ts_extra = (ts_raw & 0x7FFF) << 21

        ts = prev_ts + ts_extra
        price = prev_price + price_extra
        side = (size_byte >> 7) & 1

        print(f'Tick {tick} at pos {pos}: ts_extra={ts_extra}, price_extra={price_extra}, ts={ts}, price={price}')

        prev_ts = ts
        prev_price = price
        pos += 12
    else:
        print(f'Tick {tick} at pos {pos}: BASE delta (should not happen with 60s intervals)')
        pos += 4

# Now find tick 1022 and 1023
print(f'\nTracing to tick 1023...')
pos = anchor0_pos + 30
prev_ts = struct.unpack_from('<Q', data, anchor0_pos)[0]
prev_price = struct.unpack_from('<q', data, anchor0_pos + 8)[0]
prev_size = struct.unpack_from('<q', data, anchor0_pos + 16)[0]
prev_side = data[anchor0_pos + 24]

tick = 1
while tick <= 1025:
    if data[pos] == 0xFF:
        ts_raw = struct.unpack_from('<H', data, pos + 1)[0]
        price_extra = struct.unpack_from('<q', data, pos + 3)[0]
        size_byte = data[pos + 11]

        marker = (ts_raw >> 15) & 1
        if marker == 0:
            ts_extra = ts_raw >> 1
        else:
            ts_extra = (ts_raw & 0x7FFF) << 21

        ts = prev_ts + ts_extra
        price = prev_price + price_extra

        if tick in [1022, 1023, 1024]:
            print(f'Tick {tick} at pos {pos}: ts_raw=0x{ts_raw:04x}, marker={marker}, ts_extra={ts_extra}, price_extra={price_extra}')
            print(f'  prev_ts={prev_ts}, ts={ts}')
            print(f'  prev_price={prev_price}, price={price}')

        prev_ts = ts
        prev_price = price
        pos += 12
    else:
        packed = struct.unpack_from('<I', data, pos)[0]
        ts_delta = packed & 0xFFFFF
        price_zigzag = (packed >> 20) & 0x7FFFF

        if price_zigzag & 1:
            price_delta = -(price_zigzag >> 1)
        else:
            price_delta = price_zigzag >> 1

        ts = prev_ts + ts_delta
        price = prev_price + price_delta

        if tick in [1022, 1023, 1024]:
            print(f'Tick {tick} at pos {pos}: BASE ts_delta={ts_delta}, price_delta={price_delta}')

        prev_ts = ts
        prev_price = price
        pos += 4

    tick += 1

print(f'\nFinal pos after tick 1025: {pos}')
print(f'Anchor 1 should be at: 12434')
print(f'Index offset: {idx_offset}')
print(f'Data end: {idx_offset}')

# Check what's at pos 12434
print(f'\nAt pos 12434: {data[12434:12444].hex()}')
ts_at_12434 = struct.unpack_from('<Q', data, 12434)[0]
print(f'Ts at 12434: {ts_at_12434}')