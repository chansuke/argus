//! On-disk framing. Private: nothing outside this crate can depend on the byte layout.
//!
//! ```text
//! file   = MAGIC(8) header_len:u32 header_crc:u32 header[header_len] frame*
//! frame  = payload_len:u32 crc:u32 seq:u64 wall_ns:u64 logical:u32 payload[payload_len]
//!          crc covers seq..payload
//! ```
//! All integers little-endian. A frame whose length runs past EOF or whose CRC fails is
//! the torn tail; everything before it is trusted.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::{LogError, Seq, Stamp};

pub(crate) const MAGIC: &[u8; 8] = b"ARGUSLG1";
pub(crate) const FILE_HEAD: usize = 8 + 4 + 4;
pub(crate) const FRAME_HEAD: usize = 4 + 4 + 8 + 8 + 4;
/// A header is a short description of the log's source; anything longer is garbage.
const MAX_HEADER: u32 = 1 << 16;

#[derive(Debug)]
pub(crate) struct FrameHead {
    pub payload_len: u32,
    pub crc: u32,
    pub seq: Seq,
    pub stamp: Stamp,
}

pub(crate) fn encode_file_head(header: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FILE_HEAD + header.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32(header).to_le_bytes());
    out.extend_from_slice(header);
    out
}

pub(crate) fn encode(seq: Seq, stamp: Stamp, payload: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    let crc_at = out.len();
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&seq.get().to_le_bytes());
    out.extend_from_slice(&stamp.wall_ns.to_le_bytes());
    out.extend_from_slice(&stamp.logical.to_le_bytes());
    out.extend_from_slice(payload);
    let crc = crc32(&out[crc_at + 4..]);
    out[crc_at..crc_at + 4].copy_from_slice(&crc.to_le_bytes());
}

/// `None` when `buf` holds less than a whole head (torn tail candidate).
pub(crate) fn decode_head(buf: &[u8]) -> Option<FrameHead> {
    let buf = buf.get(..FRAME_HEAD)?;
    let u32_at = |i: usize| u32::from_le_bytes(buf[i..i + 4].try_into().unwrap());
    let u64_at = |i: usize| u64::from_le_bytes(buf[i..i + 8].try_into().unwrap());
    Some(FrameHead {
        payload_len: u32_at(0),
        crc: u32_at(4),
        seq: Seq::from_persisted(u64_at(8)),
        stamp: Stamp {
            wall_ns: u64_at(16),
            logical: u32_at(24),
        },
    })
}

/// Whether the head and payload are the bytes that were written.
pub(crate) fn verify(head_bytes: &[u8], payload: &[u8], head: &FrameHead) -> bool {
    let mut crc = Crc32::new();
    crc.update(&head_bytes[8..FRAME_HEAD]);
    crc.update(payload);
    crc.finish() == head.crc
}

/// What a file holds, read from its start.
pub(crate) enum FileHead {
    Whole {
        header: Vec<u8>,
        end: u64,
    },
    /// Shorter than its own head: a crash while the log was being created.
    Torn {
        bytes: Vec<u8>,
    },
}

pub(crate) fn read_file_head(file: &mut File, path: &Path) -> Result<FileHead, LogError> {
    file.seek(SeekFrom::Start(0))?;
    let mut fixed = Vec::with_capacity(FILE_HEAD);
    file.take(FILE_HEAD as u64).read_to_end(&mut fixed)?;
    if !MAGIC.starts_with(&fixed[..fixed.len().min(MAGIC.len())]) {
        return Err(LogError::NotALog(path.to_owned()));
    }
    if fixed.len() < FILE_HEAD {
        return Ok(FileHead::Torn { bytes: fixed });
    }
    let len = u32::from_le_bytes(fixed[8..12].try_into().unwrap());
    let crc = u32::from_le_bytes(fixed[12..16].try_into().unwrap());
    if len > MAX_HEADER {
        return Err(LogError::NotALog(path.to_owned()));
    }
    let mut header = Vec::with_capacity(len as usize);
    file.take(u64::from(len)).read_to_end(&mut header)?;
    if header.len() < len as usize {
        fixed.extend_from_slice(&header);
        return Ok(FileHead::Torn { bytes: fixed });
    }
    if crc32(&header) != crc {
        return Err(LogError::NotALog(path.to_owned()));
    }
    Ok(FileHead::Whole {
        header,
        end: (FILE_HEAD + len as usize) as u64,
    })
}

/// The frames found by [`scan`]: their offsets, and where the trusted bytes end.
pub(crate) struct Scan {
    pub offsets: Vec<u64>,
    pub last_stamp: Option<Stamp>,
    pub end: u64,
}

/// Walks whole, valid frames from `start`, where frame `first` is expected. Stops at the
/// first frame that is short, fails its CRC, or carries the wrong seq or a stamp that does
/// not increase; that frame and everything after it are the torn tail.
pub(crate) fn scan(
    file: &mut File,
    start: u64,
    first: Seq,
    mut last_stamp: Option<Stamp>,
) -> Result<Scan, LogError> {
    file.seek(SeekFrom::Start(start))?;
    let mut r = BufReader::new(file);
    let mut offsets = Vec::new();
    let mut end = start;
    let mut seq = first;
    let mut head_bytes = [0u8; FRAME_HEAD];
    let mut payload = Vec::new();
    loop {
        if !read_full(&mut r, &mut head_bytes)? {
            break;
        }
        let head = decode_head(&head_bytes).unwrap();
        payload.clear();
        let got = (&mut r)
            .take(u64::from(head.payload_len))
            .read_to_end(&mut payload)?;
        let in_order = last_stamp.is_none_or(|last| head.stamp > last);
        if got < head.payload_len as usize
            || head.seq != seq
            || !in_order
            || !verify(&head_bytes, &payload, &head)
        {
            break;
        }
        offsets.push(end);
        end += (FRAME_HEAD + payload.len()) as u64;
        seq = seq.next();
        last_stamp = Some(head.stamp);
    }
    Ok(Scan {
        offsets,
        last_stamp,
        end,
    })
}

/// Fills `buf` completely, or returns `false` at EOF (including a partial fill).
pub(crate) fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// CRC-32 (IEEE), table-driven; std has none.
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(bytes);
    crc.finish()
}

struct Crc32(u32);

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

impl Crc32 {
    fn new() -> Self {
        Crc32(!0)
    }
    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = CRC_TABLE[((self.0 ^ u32::from(b)) & 0xFF) as usize] ^ (self.0 >> 8);
        }
    }
    fn finish(&self) -> u32 {
        !self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_ieee_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }
}
