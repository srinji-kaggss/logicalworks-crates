//! One request-head reader for every HTTP fixture server.
//!
//! Unit tests (through `src/http/tests/mod.rs`) and integration sims
//! (through `tests/it/main.rs`) include this file by path, so every
//! loopback server reads a request head the same way: through the end of
//! the header block, returning what arrived, empty when the peer closed
//! before sending anything. A second copy would be a second server
//! behavior the harnesses could silently disagree on.

use std::io::Read;

/// Read through the end of one HTTP request header block.
///
/// Returns the bytes read: empty when the peer closed before sending
/// anything, a prefix without the terminator when it closed mid-head.
/// Callers that need a complete head check for the terminator themselves.
pub fn read_request_head(stream: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut request = [0_u8; 1024];
    let mut head = Vec::new();
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut request)?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(&request[..read]);
    }
    Ok(head)
}
