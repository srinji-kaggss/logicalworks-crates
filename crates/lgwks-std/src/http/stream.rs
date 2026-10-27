//! Streaming bodies, server-sent events, and cursor pagination.
//!
//! The buffered [`Response`](super::Response) is the right shape when the
//! caller wants the whole body: one value, bounded by
//! [`Options::max_body_bytes`](super::Options::max_body_bytes), refused past
//! it. This module is for everything that does not fit in one value: a body
//! read chunk by chunk under the same ceiling, an event stream dispatched
//! line by line, and a paginated listing walked cursor by cursor.
//!
//! The ceiling still bounds everything. A stream stops at
//! `max_body_bytes` total bytes: under [`BodyPolicy::Whole`](super::BodyPolicy)
//! that stop is [`Error::BodyTooLarge`](super::Error::BodyTooLarge), under
//! [`BodyPolicy::Preview`](super::BodyPolicy) it is a clean end marked
//! [`Truncation::Cut`](super::Truncation). An SSE line longer than the same
//! ceiling is always refused, because a line is only deliverable whole.
//!
//! What this module is not: an async transport. There is no async streaming
//! client here by decision, not by omission — see the crate guide's HTTP
//! page for the seam. From async code, run the blocking client below on a
//! blocking thread and stream there.
//!
//! ```rust
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use lgwks_std::http::{Page, Paginate, PaginationLimits, paginate};
//! let limits = PaginationLimits::default().max_pages(3);
//! let pages = paginate(Some("start".to_owned()), limits, |cursor| {
//!     Ok::<_, Paginate<std::convert::Infallible>>(match cursor {
//!         Some("start") => Page::new(vec!["a", "b"], Some("next".to_owned())),
//!         _ => Page::new(vec!["c"], None),
//!     })
//! })?;
//! assert_eq!(pages, vec!["a", "b", "c"], "every page is collected in order");
//! # Ok(())
//! # }
//! ```

use std::io::Read;

use super::{BodyPolicy, Error, FailureKind, FailureStage, Options};

// ── Streamed response ───────────────────────────────────────────────────────

/// A completed exchange head with its body still on the wire.
///
/// Status, headers, and redirect history are read exactly like
/// [`Response`](super::Response); the body arrives through
/// [`StreamedBody::read_chunk`], chunk by chunk, until it returns `None`.
/// Dropping the body half-read closes the connection without returning it
/// to the pool: only a fully consumed stream pools.
pub struct StreamResponse {
    /// HTTP status code, including 4xx/5xx.
    pub status: u16,
    /// The shared head: headers and provenance, read the same way as a
    /// buffered [`Response`](super::Response).
    head: super::Head,
    /// The per-line ceiling for SSE dispatch: the exchange's `max_body_bytes`.
    line_cap: usize,
    /// The body still arriving.
    body: StreamedBody,
}

impl StreamResponse {
    /// Compatibility projection of headers, lossy UTF-8 like
    /// [`Response::headers`](super::Response::headers).
    #[must_use]
    pub fn headers(&self) -> &[(String, String)] {
        self.head.headers()
    }

    /// Header names with exact legal value bytes, one item per value.
    pub fn header_values(&self) -> impl ExactSizeIterator<Item = (&str, &[u8])> {
        self.head.header_values()
    }

    /// The sanitized final request target after any followed redirects.
    #[must_use]
    pub fn final_target(&self) -> &str {
        self.head.final_target()
    }

    /// Sanitized redirect history, including original and final targets.
    #[must_use]
    pub fn redirect_chain(&self) -> &[String] {
        self.head.redirect_chain()
    }

    /// The body still arriving.
    pub fn body(&mut self) -> &mut StreamedBody {
        &mut self.body
    }

    /// Take the body, leaving the head behind.
    #[must_use]
    pub fn into_body(self) -> StreamedBody {
        self.body
    }

    /// Dispatch this stream as server-sent events.
    #[must_use]
    pub fn into_sse(self) -> SseStream {
        SseStream::new(self)
    }

    /// Build a streamed response from one raw exchange.
    pub(super) fn new(
        status: u16,
        headers: Vec<(String, String)>,
        header_bytes: Vec<(String, Vec<u8>)>,
        final_target: String,
        redirect_chain: Vec<String>,
        options: &Options,
        reader: ureq::BodyReader<'static>,
    ) -> Self {
        Self {
            status,
            head: super::Head {
                headers,
                header_bytes,
                final_target,
                redirect_chain,
            },
            line_cap: options.max_body_bytes,
            body: StreamedBody::new(reader, options),
        }
    }
}

// ── Streamed body ───────────────────────────────────────────────────────────

/// A response body arriving in chunks under the exchange's ceiling.
///
/// [`read_chunk`](StreamedBody::read_chunk) is the typed path: each call
/// returns `Some(n)` with `n > 0` for bytes read, `None` for a clean end,
/// and a typed [`Error`] naming the stage and class of whatever stopped the
/// stream. The [`Read`] implementation is the lossy projection for callers
/// that already speak `Read` (like [`BufReader`](std::io::BufReader)): same
/// bytes, but failures arrive as [`std::io::Error`] with the closest kind
/// and the typed rendering as their message.
pub struct StreamedBody {
    /// The transport reader. `None` once the end is known, so a second read
    /// past the end answers `None` instead of touching the socket.
    reader: Option<ureq::BodyReader<'static>>,
    /// Bytes still allowed under the ceiling.
    remaining: usize,
    /// The ceiling from the exchange's options.
    ceiling: usize,
    /// What a ceiling stop means for this stream.
    policy: BodyPolicy,
    /// The terminal state, once known.
    end: Option<super::Truncation>,
}

impl StreamedBody {
    /// Wrap one transport reader under `options`' ceiling and policy.
    pub(super) fn new(reader: ureq::BodyReader<'static>, options: &Options) -> Self {
        Self {
            reader: Some(reader),
            remaining: options.max_body_bytes,
            ceiling: options.max_body_bytes,
            policy: options.body_policy,
            end: None,
        }
    }

    /// Read the next chunk: `Some(n)` with `n > 0`, `None` at the clean end.
    ///
    /// An empty `buf` answers `Some(0)` without touching the transport. A
    /// body that reaches the ceiling is refused with
    /// [`Error::BodyTooLarge`](super::Error::BodyTooLarge) under
    /// [`BodyPolicy::Whole`](super::BodyPolicy), or ends cleanly with
    /// [`Truncation::Cut`](super::Truncation) under
    /// [`BodyPolicy::Preview`](super::BodyPolicy).
    pub fn read_chunk(&mut self, buf: &mut [u8]) -> Result<Option<usize>, Error> {
        if self.end.is_some() {
            return Ok(None);
        }
        if buf.is_empty() {
            return Ok(Some(0));
        }
        let allowed = buf.len().min(self.remaining);
        if allowed == 0 {
            return self.probe_for_more();
        }
        let Some(reader) = self.reader.as_mut() else {
            self.end = Some(super::Truncation::Complete);
            return Ok(None);
        };
        match reader.read(&mut buf[..allowed]) {
            Ok(0) => {
                self.reader = None;
                self.end = Some(super::Truncation::Complete);
                Ok(None)
            }
            Ok(read) => {
                self.remaining = self.remaining.saturating_sub(read);
                Ok(Some(read))
            }
            Err(read_error) => Err(super::exchange::map_read_error(
                read_error,
                FailureStage::Body,
            )),
        }
    }

    /// The terminal state, once the stream has reached it.
    ///
    /// `None` while the stream is open: only a finished stream can say
    /// whether it finished whole or cut.
    #[must_use]
    pub fn truncation(&self) -> Option<super::Truncation> {
        self.end
    }

    /// Read the one byte that tells an exact-ceiling end from an overrun.
    ///
    /// The mirror of the buffered path's probe: a body that ends exactly at
    /// the ceiling is complete, one that continues is cut. The byte is
    /// dropped either way — it lies past the ceiling, which exists so the
    /// buffer never holds it.
    fn probe_for_more(&mut self) -> Result<Option<usize>, Error> {
        let mut scratch = [0_u8; 1];
        let Some(reader) = self.reader.as_mut() else {
            self.end = Some(super::Truncation::Complete);
            return Ok(None);
        };
        match reader.read(&mut scratch) {
            Ok(0) => {
                self.reader = None;
                self.end = Some(super::Truncation::Complete);
                Ok(None)
            }
            Ok(_) => {
                self.reader = None;
                if self.policy == BodyPolicy::Whole {
                    let refusal = Err(Error::BodyTooLarge {
                        limit: self.ceiling,
                    });
                    #[cfg(feature = "trace")]
                    crate::trace::debug!(
                        error = ?refusal.as_ref().err(),
                        "probe_for_more: a whole stream reached its ceiling"
                    );
                    return refusal;
                }
                self.end = Some(super::Truncation::Cut);
                Ok(None)
            }
            Err(read_error) => Err(super::exchange::map_read_error(
                read_error,
                FailureStage::EofProbe,
            )),
        }
    }
}

impl Read for StreamedBody {
    /// The lossy projection of [`read_chunk`](StreamedBody::read_chunk):
    /// same bytes, but a failure arrives as [`std::io::Error`] with the
    /// closest [`std::io::ErrorKind`] and the typed rendering as its message.
    /// Callers that need the stage and class use `read_chunk` instead.
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.read_chunk(buf) {
            Ok(None) => Ok(0),
            Ok(Some(read)) => Ok(read),
            Err(typed) => Err(io_error_of(&typed)),
        }
    }
}

/// Project a typed exchange failure onto the closest I/O kind.
///
/// The message keeps the typed rendering, so the stage and class survive as
/// text even where the type cannot travel.
fn io_error_of(typed: &Error) -> std::io::Error {
    let kind = match *typed {
        Error::BodyTooLarge { .. } => std::io::ErrorKind::OutOfMemory,
        Error::Failure { kind, .. } => match kind {
            FailureKind::Timeout => std::io::ErrorKind::TimedOut,
            FailureKind::Interrupted => std::io::ErrorKind::Interrupted,
            FailureKind::InvalidUtf8 => std::io::ErrorKind::InvalidData,
            FailureKind::Transport => std::io::ErrorKind::ConnectionReset,
            FailureKind::RedirectLimit | FailureKind::InvalidRequest | FailureKind::Resource => {
                std::io::ErrorKind::Other
            }
        },
        Error::InvalidUrl | Error::RedirectLimitTooLarge { .. } => std::io::ErrorKind::Other,
    };
    std::io::Error::new(kind, typed.to_string())
}

// ── Server-sent events ──────────────────────────────────────────────────────

/// One dispatched server-sent event: the SSE stream's unit of meaning.
///
/// `event` defaults to `"message"` when the event carried data but no event
/// field, per the SSE specification. `data` joins multiple `data:` lines
/// with `\n` and carries no trailing newline. `retry` is the reconnection
/// time this event carried, if any; it does not persist onto later events.
///
/// The fields are read through accessors: an event is what the stream
/// dispatched, and a caller holding one reads it rather than edits it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SseEvent {
    /// The event type, or `"message"` when the event named none.
    event: String,
    /// The event payload, multiple lines joined with `\n`.
    data: String,
    /// The last event ID seen at or before this event.
    id: Option<String>,
    /// The reconnection time this event carried, in milliseconds.
    retry: Option<u64>,
}

impl SseEvent {
    /// The event type, or `"message"` when the event named none.
    #[must_use]
    pub fn event(&self) -> &str {
        &self.event
    }

    /// The event payload, multiple lines joined with `\n`.
    #[must_use]
    pub fn data(&self) -> &str {
        &self.data
    }

    /// The last event ID seen at or before this event: what a reconnect
    /// sends as `Last-Event-ID`.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// The reconnection time this event carried, in milliseconds.
    #[must_use]
    pub const fn retry(&self) -> Option<u64> {
        self.retry
    }
}

/// A server-sent event stream dispatched from one [`StreamResponse`].
///
/// Lines end at `\n`, `\r\n` or `\r`; a blank line dispatches; lines
/// starting with `:` are comments. The `event`, `data`, `id` and `retry`
/// fields behave per the SSE specification, unknown fields are ignored, and
/// the last-seen ID persists across events the way reconnection needs it to.
/// A leading UTF-8 BOM is stripped once, at the start of the stream.
pub struct SseStream {
    /// The body still arriving.
    body: StreamedBody,
    /// Bytes accumulated but not yet formed into a line.
    pending: Vec<u8>,
    /// Whether the BOM strip and the first read have happened.
    started: bool,
    /// The per-line ceiling: the exchange's `max_body_bytes`.
    line_cap: usize,
    /// The event type being accumulated, if named.
    event: Option<String>,
    /// The data lines accumulated for the pending event.
    data: Vec<String>,
    /// The ID being accumulated, or the last dispatched one.
    id: Option<String>,
    /// The reconnection time carried by the pending event, if any.
    retry: Option<u64>,
    /// Whether any field of the pending event has been seen.
    dirty: bool,
}

impl SseStream {
    /// Dispatch `response` as events.
    #[must_use]
    pub fn new(response: StreamResponse) -> Self {
        let line_cap = response.line_cap;
        let body = response.into_body();
        Self {
            body,
            pending: Vec::new(),
            started: false,
            line_cap,
            event: None,
            data: Vec::new(),
            id: None,
            retry: None,
            dirty: false,
        }
    }

    /// The stream's terminal state, once the body has reached it.
    #[must_use]
    pub fn truncation(&self) -> Option<super::Truncation> {
        self.body.truncation()
    }

    /// Dispatch the next event, or `None` at a clean end.
    ///
    /// A stream cut at its ceiling under
    /// [`BodyPolicy::Preview`](super::BodyPolicy) ends here: a partial
    /// event is not dispatchable, so it is dropped and the truncation says
    /// [`Truncation::Cut`](super::Truncation). A stream that ends with a
    /// pending but undispatched event dispatches it rather than losing it.
    pub fn next_event(&mut self) -> Result<Option<SseEvent>, Error> {
        let mut chunk = [0_u8; 8_192];
        loop {
            if let Some(line) = self.take_line()? {
                if let Some(event) = self.feed_line(&line)? {
                    return Ok(Some(event));
                }
                continue;
            }
            match self.body.read_chunk(&mut chunk)? {
                None => {
                    if self.dirty {
                        return Ok(Some(self.dispatch()));
                    }
                    return Ok(None);
                }
                Some(0) => continue,
                Some(read) => {
                    let bytes = chunk.get(..read).ok_or(Error::Failure {
                        stage: FailureStage::Body,
                        kind: FailureKind::Transport,
                        cause: None,
                    })?;
                    if !self.started {
                        self.started = true;
                        let unmarked = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
                            Some(stripped) => stripped,
                            None => bytes,
                        };
                        self.pending.extend_from_slice(unmarked);
                    } else {
                        self.pending.extend_from_slice(bytes);
                    }
                }
            }
        }
    }

    /// Split one line off the pending bytes, if a terminator has arrived.
    ///
    /// A line longer than the line cap is refused outright: a line is only
    /// deliverable whole, so previewing it would hand the caller a fragment
    /// shaped like an event.
    fn take_line(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let end = self
            .pending
            .iter()
            .position(|byte| *byte == b'\n' || *byte == b'\r');
        let Some(end) = end else {
            if self.pending.len() > self.line_cap {
                let refusal = Err(Error::BodyTooLarge {
                    limit: self.line_cap,
                });
                #[cfg(feature = "trace")]
                crate::trace::debug!(
                    error = ?refusal.as_ref().err(),
                    "take_line: a buffered line passed its ceiling"
                );
                return refusal;
            }
            return Ok(None);
        };
        let mut stop = end.saturating_add(1);
        if self.pending[end] == b'\r' && self.pending.get(stop) == Some(&b'\n') {
            stop = stop.saturating_add(1);
        }
        let mut line: Vec<u8> = self.pending.drain(..stop).collect();
        while line
            .last()
            .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
        {
            line.pop();
        }
        if line.len() > self.line_cap {
            let refusal = Err(Error::BodyTooLarge {
                limit: self.line_cap,
            });
            #[cfg(feature = "trace")]
            crate::trace::debug!(
                error = ?refusal.as_ref().err(),
                "take_line: a split line passed its ceiling"
            );
            return refusal;
        }
        Ok(Some(line))
    }

    /// Feed one line into the pending event, dispatching on a blank line.
    fn feed_line(&mut self, line: &[u8]) -> Result<Option<SseEvent>, Error> {
        if line.is_empty() {
            if self.dirty {
                return Ok(Some(self.dispatch()));
            }
            return Ok(None);
        }
        if line.starts_with(b":") {
            return Ok(None);
        }
        let text = match std::str::from_utf8(line) {
            Ok(text) => text,
            Err(utf8_error) => {
                let refusal = Err(Error::Failure {
                    stage: FailureStage::TextDecode,
                    kind: FailureKind::InvalidUtf8,
                    cause: Some(super::FailureCause::sanitized(format!(
                        "valid UTF-8 ends at byte {}",
                        utf8_error.valid_up_to()
                    ))),
                });
                #[cfg(feature = "trace")]
                crate::trace::debug!(
                    error = ?refusal.as_ref().err(),
                    "feed_line: an event line is not UTF-8"
                );
                return refusal;
            }
        };
        let (field, value) = match text.split_once(':') {
            Some((field, rest)) => match rest.strip_prefix(' ') {
                Some(value) => (field, value),
                None => (field, rest),
            },
            None => (text, ""),
        };
        match field {
            "event" => {
                self.event = Some(value.to_owned());
                self.dirty = true;
            }
            "data" => {
                self.data.push(value.to_owned());
                self.dirty = true;
            }
            "id" => {
                if !value.contains('\0') {
                    self.id = Some(value.to_owned());
                    self.dirty = true;
                }
            }
            "retry" => {
                if let Ok(millis) = value.parse::<u64>() {
                    self.retry = Some(millis);
                    self.dirty = true;
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// Dispatch the pending event and clear the accumulator.
    ///
    /// An event with no data dispatches nothing: per the specification, only
    /// data (or an explicit reminder to reconnect, which this typed shape
    /// has no room for) fires an event.
    fn dispatch(&mut self) -> SseEvent {
        let event = SseEvent {
            event: match self.event.take() {
                Some(event) => event,
                None => "message".to_owned(),
            },
            data: self.data.join("\n"),
            id: self.id.clone(),
            retry: self.retry.take(),
        };
        self.data.clear();
        self.dirty = false;
        event
    }
}

// ── Cursor pagination ───────────────────────────────────────────────────────

/// One page of a cursor-paginated listing: items plus the opaque cursor.
///
/// The cursor is an opaque token, never interpreted here: it is passed back
/// byte-identical on the next fetch, and `None` ends the walk. There is no
/// offset spelling anywhere in this module — an offset lets a concurrent
/// writer duplicate or drop rows across pages, which is the failure cursors
/// exist to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Page<Item> {
    /// The items on this page, in the order the server sent them.
    items: Vec<Item>,
    /// The cursor for the next page, or `None` for the last page.
    next_cursor: Option<String>,
}

impl<Item> Page<Item> {
    /// One page: `items` in server order, `next_cursor` for what follows.
    #[must_use]
    pub fn new(items: Vec<Item>, next_cursor: Option<String>) -> Self {
        Self { items, next_cursor }
    }
}

/// Bounds on a [`paginate`] walk: how many pages, how many items.
///
/// Both bounds terminate degenerate servers (an empty page that keeps its
/// cursor would otherwise walk forever) instead of trusting them to end.
/// Zero pages refuses before the first fetch; zero items refuses on the
/// first non-empty page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PaginationLimits {
    /// Maximum pages fetched in one walk.
    pub max_pages: usize,
    /// Maximum items collected in one walk.
    pub max_items: usize,
}

impl Default for PaginationLimits {
    /// One hundred pages and ten thousand items: generous for a listing,
    /// bounded against a server that never ends.
    fn default() -> Self {
        Self {
            max_pages: 100,
            max_items: 10_000,
        }
    }
}

impl PaginationLimits {
    /// Maximum pages fetched in one walk.
    #[must_use]
    pub fn max_pages(mut self, max_pages: usize) -> Self {
        self.max_pages = max_pages;
        self
    }

    /// Maximum items collected in one walk.
    #[must_use]
    pub fn max_items(mut self, max_items: usize) -> Self {
        self.max_items = max_items;
        self
    }
}

/// What a [`paginate`] walk can report besides its items.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Paginate<Fetch> {
    /// The caller's fetch refused: transport, decode, or anything else the
    /// fetch closure reports. Carried, never reinterpreted.
    Fetch(Fetch),
    /// The walk reached a bound with pages still unfetched. The counts say
    /// how far it got; the collected items are not returned, because a
    /// prefix handed back as a listing is the short read this estate refuses
    /// everywhere else.
    LimitExceeded {
        /// Pages fetched before the walk stopped.
        pages_fetched: usize,
        /// Items collected before the walk stopped.
        items_collected: usize,
    },
}

impl<Fetch> std::fmt::Display for Paginate<Fetch>
where
    Fetch: std::fmt::Display,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Matched through `*self` so every arm's pattern is the enum's own
        // type; `Fetch` binds its payload by reference, since the enum is
        // not `Copy` and formatting must not consume it.
        match *self {
            Self::Fetch(ref fetch) => write!(formatter, "page fetch failed: {fetch}"),
            Self::LimitExceeded {
                pages_fetched,
                items_collected,
            } => write!(
                formatter,
                "pagination limit reached after {pages_fetched} pages and {items_collected} items"
            ),
        }
    }
}

impl<Fetch> std::error::Error for Paginate<Fetch>
where
    Fetch: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // `*self` with a `ref` binding, like the `Display` above: the arm's
        // pattern is the enum's own type and nothing is consumed.
        match *self {
            Self::Fetch(ref fetch) => Some(fetch),
            Self::LimitExceeded { .. } => None,
        }
    }
}

/// Refuse a walk that reached a bound with pages still unfetched.
///
/// The page and item bounds stop in different places, but the refusal they
/// return is one shape, built once here. Each stop announces itself through
/// `trace` at its own call site, so no error leaves here as a bare value.
fn limit_exceeded<Fetch>(pages_fetched: usize, items_collected: usize) -> Paginate<Fetch> {
    Paginate::LimitExceeded {
        pages_fetched,
        items_collected,
    }
}

/// Walk a cursor-paginated listing to its end, collecting every item.
///
/// `fetch` receives the cursor for the page to read (`None` for the first)
/// and returns that page decoded; decoding stays with the caller, which owns
/// its format. Pages arrive in order and so do the collected items. The walk
/// stops at `None`, at [`PaginationLimits`], or at the first fetch refusal —
/// whichever comes first.
pub fn paginate<Item, Fetch>(
    first: Option<String>,
    limits: PaginationLimits,
    mut fetch: impl FnMut(Option<&str>) -> Result<Page<Item>, Fetch>,
) -> Result<Vec<Item>, Paginate<Fetch>> {
    let mut items = Vec::new();
    let mut cursor = first;
    let mut pages_fetched = 0_usize;
    loop {
        if pages_fetched >= limits.max_pages {
            let refusal = Err(limit_exceeded(pages_fetched, items.len()));
            #[cfg(feature = "trace")]
            crate::trace::debug!(
                pages_fetched,
                collected = items.len(),
                "paginate: the page bound stopped the walk"
            );
            return refusal;
        }
        let page = fetch(cursor.as_deref()).map_err(Paginate::Fetch)?;
        pages_fetched = pages_fetched.saturating_add(1);
        if items.len().saturating_add(page.items.len()) > limits.max_items {
            let refusal = Err(limit_exceeded(pages_fetched, items.len()));
            #[cfg(feature = "trace")]
            crate::trace::debug!(
                pages_fetched,
                collected = items.len(),
                incoming = page.items.len(),
                "paginate: the item bound stopped the walk"
            );
            return refusal;
        }
        items.extend(page.items);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(items),
        }
    }
}
