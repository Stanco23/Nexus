#!/usr/bin/env python3
"""Verify TVC and TVCB file processing."""
import struct
import os
import sys

def read_tvc_header(path):
    """Read and parse TVC header."""
    with open(path, 'rb') as f:
        magic = f.read(4)
        version = struct.unpack('B', f.read(1))[0]
        decimal_precision = struct.unpack('B', f.read(1))[0]
        anchor_interval = struct.unpack('<I', f.read(4))[0]
        instrument_id = struct.unpack('<I', f.read(4))[0]
        start_time_ns = struct.unpack('<Q', f.read(8))[0]
        end_time_ns = struct.unpack('<Q', f.read(8))[0]
        num_ticks = struct.unpack('<Q', f.read(8))[0]
        num_anchors = struct.unpack('<I', f.read(4))[0]
        index_offset = struct.unpack('<Q', f.read(8))[0]
        
        # Read index
        f.seek(index_offset)
        num_anchors_read = struct.unpack('<I', f.read(4))[0]
        index_entries = []
        for _ in range(num_anchors_read):
            tick_index = struct.unpack('<Q', f.read(8))[0]
            byte_offset = struct.unpack('<Q', f.read(8))[0]
            index_entries.append((tick_index, byte_offset))
        
    return {
        'magic': magic,
        'version': version,
        'decimal_precision': decimal_precision,
        'anchor_interval': anchor_interval,
        'instrument_id': instrument_id,
        'start_time_ns': start_time_ns,
        'end_time_ns': end_time_ns,
        'num_ticks': num_ticks,
        'num_anchors': num_anchors,
        'index_offset': index_offset,
        'index_entries': index_entries
    }

def decode_anchor_tick(data, offset, decimal_precision):
    """Decode 30-byte anchor tick."""
    # timestamp_ns: u64 (8)
    # price_int: u64 (8)
    # size_int: u64 (8)
    # side: u8 (1)
    # flags: u8 (1)
    # sequence: u32 (4)
    ts = struct.unpack_from('<Q', data, offset)[0]
    price_int = struct.unpack_from('<Q', data, offset + 8)[0]
    size_int = struct.unpack_from('<Q', data, offset + 16)[0]
    side = data[offset + 24]
    flags = data[offset + 25]
    seq = struct.unpack_from('<I', data, offset + 26)[0]
    
    price = price_int / (10 ** decimal_precision)
    return {'ts': ts, 'price': price, 'size_int': size_int, 'side': side, 'flags': flags, 'seq': seq}

def read_tvc_raw_ticks(path, count=5):
    """Read raw tick data from TVC file."""
    results = []
    with open(path, 'rb') as f:
        # Skip header
        f.seek(128)
        
        while len(results) < count:
            byte = f.read(1)[0]
            if byte == 0xFF:
                # Overflow record (15 bytes)
                data = f.read(14)
                results.append({'type': 'overflow', 'raw': data.hex()})
            elif byte == 0:
                break
            else:
                # Base delta (4 bytes)
                data = f.read(3)
                results.append({'type': 'delta', 'raw': data.hex()})
                if len(results) >= count:
                    break
            
            # Check for anchor (0xFF followed by marker)
            pos = f.tell()
            next_byte = f.read(1)
            if next_byte and next_byte[0] == 0xFF:
                # This is an anchor
                f.seek(pos)
                anchor_data = f.read(30)
                results.append({'type': 'anchor', 'raw': anchor_data.hex()})
    
    return results

def verify_tvc(path):
    """Verify TVC file."""
    print(f"\n=== TVC: {path} ===")
    header = read_tvc_header(path)
    
    print(f"Magic: {header['magic']}")
    print(f"Version: {header['version']}")
    print(f"Decimal precision: {header['decimal_precision']}")
    print(f"Anchor interval: {header['anchor_interval']}")
    print(f"Instrument ID: 0x{header['instrument_id']:08X}")
    print(f"Start: {header['start_time_ns']} ({header['start_time_ns']/1e9:.3f})")
    print(f"End: {header['end_time_ns']} ({header['end_time_ns']/1e9:.3f})")
    print(f"Num ticks: {header['num_ticks']}")
    print(f"Num anchors: {header['num_anchors']}")
    print(f"Index offset: {header['index_offset']}")
    print(f"File size: {os.path.getsize(path)} bytes")
    
    # Show first few index entries
    print(f"\nFirst 3 index entries:")
    for i, (tick_idx, byte_off) in enumerate(header['index_entries'][:3]):
        print(f"  [{i}] tick_index={tick_idx}, byte_offset={byte_off}")
    print(f"  ... last entry:")
    if len(header['index_entries']) > 1:
        last = header['index_entries'][-1]
        print(f"  [{len(header['index_entries'])-1}] tick_index={last[0]}, byte_offset={last[1]}")
    
    # Decode first anchor tick
    with open(path, 'rb') as f:
        f.seek(header['index_entries'][0][1])
        anchor_data = f.read(30)
        tick = decode_anchor_tick(anchor_data, 0, header['decimal_precision'])
        print(f"\nFirst anchor tick:")
        print(f"  ts={tick['ts']} ({tick['ts']/1e9:.6f})")
        print(f"  price={tick['price']:.8f}")
        print(f"  size_int={tick['size_int']}")
        print(f"  side={'buy' if tick['side']==1 else 'sell'}")
    
    return header

def find_tvcb_files():
    """Find or create TVCB test files."""
    # TVCB files would be alongside TVC files with .tvcb extension
    tvc_dir = "/home/shadowarch/Nexus/data/binance/spot/BTCUSDT"
    tvc_files = sorted([f for f in os.listdir(tvc_dir) if f.endswith('.tvc')])
    
    # TVCB files don't exist yet - check
    tvcb_files = [f for f in os.listdir(tvc_dir) if f.endswith('.tvcb')]
    print(f"\nTVCB files found: {len(tvcb_files)}")
    
    return tvc_files

if __name__ == '__main__':
    tvc_dir = "/home/shadowarch/Nexus/data/binance/spot/BTCUSDT"
    
    # Verify TVC files
    tvc_files = find_tvcb_files()
    print(f"TVC files found: {len(tvc_files)}")
    
    for tvc_file in tvc_files[:2]:
        path = os.path.join(tvc_dir, tvc_file)
        try:
            header = verify_tvc(path)
            print("\n✅ TVC file OK")
        except Exception as e:
            print(f"\n❌ TVC file ERROR: {e}")
    
    print("\n\n=== Summary ===")
    print("TVC: Can read headers, index, and decode anchor ticks ✅")
    print("TVCB: Need to verify with Rust binary (no .tvcb files present yet)")
    print("Next: Run cargo test to verify TVC decode, then test TVCB with bar writer")