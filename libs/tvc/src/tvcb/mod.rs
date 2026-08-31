//! TVCB — Bar Storage Format
//!
//! Binary delta-compressed format for OHLCV bar storage.
//!
//! ## Format Layout
//! ```text
//! [128-byte header]
//! [anchor bar 72B][delta bar ...][anchor bar 72B][delta bar...]...
//!                                                           ^ index at EOF
//! [index: 16B * num_anchors]
//! [32B SHA256 digest]
//! ```
//!
//! ## Key Types
//! - [`TvcbHeader`] — 128-byte file header
//! - [`AnchorBar`] — 72-byte full bar stored every anchor_interval
//! - [`IndexEntry`] — 16-byte random-access index entry
//! - [`Bar`] — in-memory bar representation
//!
//! ## Usage
//! ```ignore
//! // Writing
//! let mut writer = TvcbWriter::new("data.tvcb", instrument_id, 100, 9, 2024, 900_000_000_000)?;
//! for bar in bars {
//!     writer.write_bar(&bar)?;
//! }
//! writer.finalize()?;
//!
//! // Reading
//! let reader = TvcbReader::open("data.tvcb")?;
//! let bar = reader.read_bar_at_index(42)?;
//!
//! // Cross-file iteration
//! let files = vec!["2022.tvcb".into(), "2023.tvcb".into()];
//! for bar in BarIter::new(files, start_ts, end_ts)? {
//!     // process bar
//! }
//! ```

pub mod encoding;
pub mod types;
pub mod writer;
pub mod reader;

pub use encoding::{encode_delta_bar, decode_delta_bar};
pub use types::{TvcbHeader, AnchorBar, IndexEntry, Bar, TvcbError, HEADER_SIZE, ANCHOR_BAR_SIZE, INDEX_ENTRY_SIZE};
pub use writer::TvcbWriter;
pub use reader::{TvcbReader, BarIter};