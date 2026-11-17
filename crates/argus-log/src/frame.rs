//! On-disk framing. Private: nothing outside this crate can depend on the byte layout.
//!
//! ```text
//! file   = MAGIC(8) header_len:u32 header_crc:u32 header[header_len] frame*
//! frame  = payload_len:u32 crc:u32 seq:u64 wall_ns:u64 logical:u32 payload[payload_len]
//!          crc covers seq..payload
//! ```
//! All integers little-endian. A frame whose length runs past EOF or whose CRC fails is
//! the torn tail; everything before it is trusted.

use crate::{Seq, Stamp};

pub(crate) const MAGIC: &[u8; 8] = b"ARGUSLG1";
pub(crate) const FRAME_HEAD: usize = 4 + 4 + 8 + 8 + 4;

pub(crate) struct FrameHead {
    pub payload_len: u32,
    pub crc: u32,
    pub seq: Seq,
    pub stamp: Stamp,
}

#[expect(
    clippy::ptr_arg,
    reason = "sketch: the body appends to the reused buffer"
)]
pub(crate) fn encode(seq: Seq, stamp: Stamp, payload: &[u8], out: &mut Vec<u8>) {
    // TODO: write head fields, compute crc32 over seq..payload, append payload.
    todo!("not implemented")
}

/// `None` when `buf` holds less than a whole head (torn tail candidate).
pub(crate) fn decode_head(buf: &[u8]) -> Option<FrameHead> {
    todo!("not implemented")
}

/// CRC-32 (IEEE), table-driven; std has none.
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    todo!("not implemented")
}
