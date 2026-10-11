//! The cross-host grant channel: an executor host asks the one lease authority
//! of its domain, on another host, over TCP.
//!
//! One question per connection, one fixed-size answer back. Every frame is
//! tagged with keyed BLAKE3 under a [`WireKey`] both hosts hold, and the two
//! directions carry different magics, so a request can never be read as a
//! reply. The asker draws a fresh 16-byte nonce from the OS for every question
//! and the reply must echo it under the tag: a recorded answer from an earlier
//! epoch cannot be replayed into a grant, because no later question carries its
//! nonce.
//!
//! Everything else fails closed. A refused connect, a timeout, a short read, a
//! bad magic, a bad tag, or a wrong nonce is a [`ChannelError`], so the grant
//! waits — the same rule as a partition. A replayed *acquire* request can only
//! move the epoch forward, which stales every lease and grants nothing.
//!
//! The server answers connections one at a time on the caller's thread and
//! spawns nothing. Every read and write is bounded by its timeout, so a slow or
//! silent peer holds the authority for at most one timeout, and a name longer
//! than [`MAX_NAME`] bytes is refused from its length, before it is read.

use std::fmt;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use lgwks_std::hash::{Digest, keyed};

use super::{
    AuthorityHandle, ChannelError, DispatchError, Fence, GrantChannel, HostId, Lease, LeaseError,
};

/// The magic that opens every request: the protocol and its version.
const REQUEST_MAGIC: [u8; 4] = *b"LGLQ";
/// The magic that opens every reply. Distinct from the request's, and inside
/// the tag, so neither direction's frame verifies as the other's.
const REPLY_MAGIC: [u8; 4] = *b"LGLR";
/// The longest holder name a request may carry, in bytes.
const MAX_NAME: usize = 256;
/// Magic, op, nonce, and the two-byte name length.
const HEADER: usize = 4 + 1 + 16 + 2;
/// The keyed tag that closes every frame.
const TAG: usize = 32;
/// Magic, nonce, epoch, tag.
const REPLY: usize = 4 + 16 + 8 + TAG;
/// The part of a reply the tag covers.
const BODY: usize = REPLY - TAG;

/// The question a request asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// What is the current epoch?
    Check,
    /// Grant to this holder, and say under which epoch.
    Acquire,
}

impl Op {
    /// The byte that names the question on the wire.
    const fn code(self) -> u8 {
        match self {
            Self::Check => 1,
            Self::Acquire => 2,
        }
    }
}

/// Why a frame was refused, on either end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The socket failed, timed out, or closed early, with the OS's kind.
    Io(std::io::ErrorKind),
    /// The frame is shorter than its fixed layout.
    Short,
    /// The frame does not open with this protocol's magic.
    Magic,
    /// The request names no known question.
    Op,
    /// The holder name is longer than [`MAX_NAME`].
    Oversize,
    /// The holder name is not UTF-8.
    Utf8,
    /// The keyed tag does not verify.
    Tag,
    /// The reply answers a different question's nonce.
    Nonce,
    /// The OS had no entropy for a nonce.
    Entropy,
    /// A zero timeout bounds nothing, and the socket refuses it.
    ZeroTimeout,
}

impl Refusal {
    /// The refusal in words, for the log and for [`ChannelError`].
    const fn reason(self) -> &'static str {
        match self {
            Self::Io(_) => "the socket failed, timed out or closed early",
            Self::Short => "the frame is shorter than its layout",
            Self::Magic => "the frame is not a lease-channel frame",
            Self::Op => "the request asks no known question",
            Self::Oversize => "the holder name is longer than 256 bytes",
            Self::Utf8 => "the holder name is not UTF-8",
            Self::Tag => "the frame's keyed tag does not verify",
            Self::Nonce => "the reply answers another question",
            Self::Entropy => "the OS had no entropy for a nonce",
            Self::ZeroTimeout => "a zero timeout bounds nothing",
        }
    }
}

/// Log `refusal` where it is decided, then hand it back as the error.
///
/// Every refusal on either end goes through here, so each leaves a trace line
/// naming its reason before any caller sees the value.
fn refuse<T>(refusal: Refusal) -> Result<T, Refusal> {
    lgwks_std::trace::debug!(refusal = ?refusal, reason = refusal.reason(), "dist_lease: frame refused");
    Err(refusal)
}

/// The refusal for a socket error, keeping the OS's kind.
fn io(error: std::io::Error) -> Refusal {
    Refusal::Io(error.kind())
}

impl From<Refusal> for ChannelError {
    /// A refused frame on the asking side: no authenticated answer arrived.
    fn from(refusal: Refusal) -> Self {
        Self::unanswered(refusal.reason())
    }
}

/// The 32-byte key both ends of a channel tag their frames with.
///
/// It must come from outside both hosts' code and data (a keyring, a secret
/// store): a key shipped beside the frames authenticates nothing. Its
/// [`Debug`] form never shows the bytes.
#[derive(Clone)]
pub struct WireKey([u8; 32]);

impl WireKey {
    /// The key from its raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The tag over `frame`.
    fn tag(&self, frame: &[u8]) -> Digest {
        keyed(&self.0, frame)
    }

    /// Whether `tag` is the tag over `frame`, compared in constant time.
    fn verifies(&self, frame: &[u8], tag: &[u8]) -> Result<(), Refusal> {
        let tag = field::<TAG>(tag, 0)?;
        if self.tag(frame).ct_eq(&Digest::from_bytes(tag)) {
            Ok(())
        } else {
            refuse(Refusal::Tag)
        }
    }
}

impl fmt::Debug for WireKey {
    /// Redacted: a log line must never carry the key.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WireKey(<redacted>)")
    }
}

/// Bound every read and write on `stream` by `timeout`.
fn bound(stream: &TcpStream, timeout: Duration) -> Result<(), Refusal> {
    if timeout.is_zero() {
        return refuse(Refusal::ZeroTimeout);
    }
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(io)
}

/// Read exactly `buffer.len()` bytes, or refuse.
fn fill(stream: &mut TcpStream, buffer: &mut [u8]) -> Result<(), Refusal> {
    stream.read_exact(buffer).map_err(io)
}

/// The `N` bytes of `bytes` at `at`, or a refusal when the frame is shorter.
fn field<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N], Refusal> {
    match bytes
        .get(at..at.saturating_add(N))
        .and_then(|part| <[u8; N]>::try_from(part).ok())
    {
        Some(found) => Ok(found),
        None => refuse(Refusal::Short),
    }
}

/// A request as the server read and verified it.
struct Request {
    /// The question.
    op: Op,
    /// The asker's nonce, echoed under the reply's tag.
    nonce: [u8; 16],
    /// The holder an acquire grants to; empty for a check.
    name: String,
}

/// Encode and tag one request.
fn encode_request(key: &WireKey, op: Op, nonce: [u8; 16], name: &str) -> Result<Vec<u8>, Refusal> {
    if name.len() > MAX_NAME {
        return refuse(Refusal::Oversize);
    }
    let length = u16::try_from(name.len()).map_err(|error| {
        lgwks_std::trace::debug!(%error, "dist_lease: a holder name length does not fit the wire");
        Refusal::Oversize
    })?;
    let mut frame = Vec::with_capacity(HEADER.saturating_add(name.len()).saturating_add(TAG));
    frame.extend_from_slice(&REQUEST_MAGIC);
    frame.push(op.code());
    frame.extend_from_slice(&nonce);
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(name.as_bytes());
    let tag = key.tag(&frame);
    frame.extend_from_slice(tag.as_bytes());
    Ok(frame)
}

/// Read and verify one request. The name's length is checked before a byte of
/// it is read, so a hostile length costs nothing.
fn read_request(stream: &mut TcpStream, key: &WireKey) -> Result<Request, Refusal> {
    let mut header = [0u8; HEADER];
    fill(stream, &mut header)?;
    if field::<4>(&header, 0)? != REQUEST_MAGIC {
        return refuse(Refusal::Magic);
    }
    let op = match field::<1>(&header, 4)? {
        [1] => Op::Check,
        [2] => Op::Acquire,
        _ => return refuse(Refusal::Op),
    };
    let length = usize::from(u16::from_le_bytes(field::<2>(&header, 21)?));
    if length > MAX_NAME {
        return refuse(Refusal::Oversize);
    }
    let mut rest = vec![0u8; length.saturating_add(TAG)];
    fill(stream, &mut rest)?;
    let Some((name, tag)) = rest.split_at_checked(length) else {
        return refuse(Refusal::Short);
    };
    let mut signed = Vec::with_capacity(HEADER.saturating_add(length));
    signed.extend_from_slice(&header);
    signed.extend_from_slice(name);
    key.verifies(&signed, tag)?;
    let name = std::str::from_utf8(name).map_err(|error| {
        lgwks_std::trace::debug!(%error, "dist_lease: a holder name is not UTF-8");
        Refusal::Utf8
    })?;
    Ok(Request {
        op,
        nonce: field::<16>(&header, 5)?,
        name: name.to_owned(),
    })
}

/// Encode and tag one reply.
fn encode_reply(key: &WireKey, nonce: [u8; 16], epoch: u64) -> Vec<u8> {
    let mut frame = Vec::with_capacity(REPLY);
    frame.extend_from_slice(&REPLY_MAGIC);
    frame.extend_from_slice(&nonce);
    frame.extend_from_slice(&epoch.to_le_bytes());
    let tag = key.tag(&frame);
    frame.extend_from_slice(tag.as_bytes());
    frame
}

/// Verify one reply against the nonce asked, returning the epoch it carries.
fn decode_reply(key: &WireKey, nonce: [u8; 16], frame: &[u8]) -> Result<u64, Refusal> {
    let Some((body, tag)) = frame.split_at_checked(BODY) else {
        return refuse(Refusal::Short);
    };
    if field::<4>(body, 0)? != REPLY_MAGIC {
        return refuse(Refusal::Magic);
    }
    key.verifies(body, tag)?;
    if field::<16>(body, 4)? != nonce {
        return refuse(Refusal::Nonce);
    }
    Ok(u64::from_le_bytes(field::<8>(body, 20)?))
}

/// What one [`AuthorityServer::serve`] call did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServeReport {
    /// Connections answered with a tagged reply.
    answered: usize,
    /// Connections refused: a failed accept or socket, a bad frame, a bad tag.
    refused: usize,
}

impl ServeReport {
    /// Connections answered with a tagged reply.
    #[must_use]
    pub fn answered(&self) -> usize {
        self.answered
    }

    /// Connections refused without a reply.
    #[must_use]
    pub fn refused(&self) -> usize {
        self.refused
    }
}

/// The far end of the channel: the one lease authority of a domain, answering
/// executor hosts over TCP.
#[derive(Debug)]
pub struct AuthorityServer {
    /// The authority every answer reads or moves.
    handle: AuthorityHandle,
    /// The key every frame is tagged with.
    key: WireKey,
    /// The bound on each connection's reads and writes.
    timeout: Duration,
}

impl AuthorityServer {
    /// A server for `handle`'s domain, tagging with `key` and bounding each
    /// connection by `timeout`. A zero timeout refuses every connection.
    #[must_use]
    pub fn new(handle: AuthorityHandle, key: WireKey, timeout: Duration) -> Self {
        Self {
            handle,
            key,
            timeout,
        }
    }

    /// Answer the next `connections` connections on `listener`, one at a
    /// time, and say how many were answered and refused.
    ///
    /// The bound is the caller's: a long-lived authority passes `usize::MAX`
    /// on a thread it owns, and a test passes the count it drives. A refused
    /// connection is closed without a reply, logged, and counted.
    #[must_use]
    pub fn serve(&self, listener: &TcpListener, connections: usize) -> ServeReport {
        let mut report = ServeReport::default();
        for incoming in listener.incoming().take(connections) {
            match incoming.map_err(io).and_then(|stream| self.answer(stream)) {
                Ok(()) => report.answered = report.answered.saturating_add(1),
                Err(refusal) => {
                    lgwks_std::trace::warn!(
                        refusal = ?refusal,
                        reason = refusal.reason(),
                        "dist_lease: authority refused a connection without a reply"
                    );
                    report.refused = report.refused.saturating_add(1);
                }
            }
        }
        report
    }

    /// Read one request, answer it from the authority, and reply.
    fn answer(&self, mut stream: TcpStream) -> Result<(), Refusal> {
        bound(&stream, self.timeout)?;
        let request = read_request(&mut stream, &self.key)?;
        let epoch = match request.op {
            Op::Check => self.handle.epoch(),
            Op::Acquire => self.handle.acquire(&request.name).epoch(),
        };
        stream
            .write_all(&encode_reply(&self.key, request.nonce, epoch))
            .map_err(io)
    }
}

/// The asking end: an executor host's channel to the authority on another
/// host. Implements [`GrantChannel`] and [`Fence`], so
/// [`Coordinator::with_fence`](super::Coordinator::with_fence) validates every
/// grant across it.
#[derive(Debug, Clone)]
pub struct TcpGrantChannel {
    /// Where the authority listens.
    addr: SocketAddr,
    /// The key every frame is tagged with.
    key: WireKey,
    /// The bound on the connect and on each read and write.
    timeout: Duration,
}

impl TcpGrantChannel {
    /// A channel to the authority at `addr`, tagging with `key`, waiting at
    /// most `timeout` for each step. A zero timeout answers nothing.
    #[must_use]
    pub fn new(addr: SocketAddr, key: WireKey, timeout: Duration) -> Self {
        Self { addr, key, timeout }
    }

    /// Ask one question and return the epoch the authenticated reply carries.
    fn ask(&self, op: Op, name: &str) -> Result<u64, ChannelError> {
        self.exchange(op, name).map_err(|refusal| {
            lgwks_std::trace::warn!(
                reason = refusal.reason(),
                addr = %self.addr,
                "dist_lease: no authenticated answer from the lease authority"
            );
            ChannelError::from(refusal)
        })
    }

    /// The exchange behind [`ask`](Self::ask), refusing at the first fault.
    fn exchange(&self, op: Op, name: &str) -> Result<u64, Refusal> {
        if self.timeout.is_zero() {
            return refuse(Refusal::ZeroTimeout);
        }
        let nonce = lgwks_std::random::bytes::<16>().map_err(|error| {
            lgwks_std::trace::warn!(%error, "dist_lease: no entropy for a nonce");
            Refusal::Entropy
        })?;
        let frame = encode_request(&self.key, op, nonce, name)?;
        let mut stream = TcpStream::connect_timeout(&self.addr, self.timeout).map_err(io)?;
        bound(&stream, self.timeout)?;
        stream.write_all(&frame).map_err(io)?;
        let mut reply = [0u8; REPLY];
        fill(&mut stream, &mut reply)?;
        decode_reply(&self.key, nonce, &reply)
    }

    /// Ask the authority to grant to `holder`, staling every outstanding
    /// lease in the domain.
    ///
    /// # Errors
    ///
    /// [`ChannelError`] when no authenticated answer arrived: no lease is
    /// returned, and the caller asks again rather than proceeding.
    pub fn acquire(&self, holder: &str) -> Result<Lease, ChannelError> {
        let epoch = self.ask(Op::Acquire, holder)?;
        Ok(Lease::minted(HostId::new(holder), epoch))
    }
}

impl GrantChannel for TcpGrantChannel {
    /// Ask the authority for its epoch and compare.
    fn validate(&self, lease: &Lease) -> Result<bool, ChannelError> {
        Ok(self.ask(Op::Check, "")? == lease.epoch())
    }
}

impl Fence for TcpGrantChannel {
    /// Admit a current lease; refuse a stale one naming both epochs; refuse an
    /// unanswered question as unvalidated.
    fn admit(&self, lease: &Lease) -> Result<(), DispatchError> {
        let current = self.ask(Op::Check, "")?;
        if current == lease.epoch() {
            Ok(())
        } else {
            Err(DispatchError::Lease(LeaseError::StaleEpoch {
                presented: lease.epoch(),
                current,
            }))
        }
    }
}
