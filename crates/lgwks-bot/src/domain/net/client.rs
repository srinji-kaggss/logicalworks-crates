//! The one pooled client an [`Endpoint`](super::Endpoint) dials through.
//!
//! Every poll, post and listing of one endpoint shares one keep-alive pool
//! (`lgwks_std::http::Client`): a bot that polls an endpoint every tick pays
//! the TCP and TLS handshake once, not once per tick (#712). A poll reads its
//! preview off the wire chunk by chunk, so a server cannot make a probe hold
//! more than the preview's own budget, and an endpoint that answers with
//! server-sent events is previewed as its first event rather than as a body
//! that never ends (#713). A listing follows the `Link: rel="next"` cursor
//! (RFC 8288) under a page and item ceiling. A deployment behind a private CA
//! or mutual TLS points `LGWKS_NET_ROOTS` at its roots and
//! `LGWKS_NET_CLIENT_CERT` / `LGWKS_NET_CLIENT_KEY` at its identity (#714);
//! the material is proven when the endpoint is built, before any socket opens.

use std::time::Duration;

use lgwks_std::http::{
    self, BodyPolicy, Client, Options, Page, Paginate, PaginationLimits, PoolConfig, Response,
    TlsOptions,
};
use lgwks_std::json;

/// Seconds a poll, post or page fetch waits for the endpoint.
const TIMEOUT_SECS: u64 = 10;

/// Idle keep-alive connections one endpoint keeps across every origin its
/// redirects reach.
const MAX_IDLE: usize = 8;

/// Idle connections kept per origin: what a burst of polls reuses.
const MAX_IDLE_PER_HOST: usize = 4;

/// Seconds an idle connection is kept between polls.
const MAX_IDLE_SECS: u64 = 30;

/// Bytes one listing page may hold. A page is decoded whole, so it is
/// refused past this rather than cut.
const PAGE_BYTES: usize = 4 * 1024 * 1024;

/// Pages one listing walk may fetch.
const MAX_PAGES: usize = 64;

/// Items one listing walk may collect.
const MAX_ITEMS: usize = 10_000;

/// Window one preview read fills.
const READ_WINDOW: usize = 8 * 1024;

/// Environment variable naming the PEM roots that replace Mozilla's set.
///
/// Not `SSL_CERT_FILE`: hosts set that to their whole system bundle, whose
/// legacy roots (SHA-1 signatures among them) the strict loader refuses, and
/// one inherited variable would take every endpoint on the host down.
const ROOTS_VAR: &str = "LGWKS_NET_ROOTS";

/// Environment variable naming the PEM client certificate chain.
const CHAIN_VAR: &str = "LGWKS_NET_CLIENT_CERT";

/// Environment variable naming the PEM client private key.
const KEY_VAR: &str = "LGWKS_NET_CLIENT_KEY";

/// Why an endpoint's client could not be built.
#[derive(Debug)]
pub(super) enum Setup {
    /// A TLS material file named by the environment could not be read.
    Read {
        /// The variable that named it.
        var: &'static str,
        /// The read failure.
        source: std::io::Error,
    },
    /// The material was read but refused, or the agent could not be built.
    Http(http::Error),
}

impl std::fmt::Display for Setup {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Read { var, ref source } => {
                write!(
                    formatter,
                    "the file {var} names could not be read: {source}"
                )
            }
            Self::Http(ref refused) => write!(formatter, "{refused}"),
        }
    }
}

impl std::error::Error for Setup {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Read { ref source, .. } => Some(source),
            Self::Http(ref refused) => Some(refused),
        }
    }
}

/// Why a listing walk stopped short of its last page.
#[derive(Debug)]
pub(super) enum Listing {
    /// The exchange failed.
    Http(http::Error),
    /// The server answered a page with a non-success status.
    Status(u16),
    /// A page was not a JSON array.
    Decode(json::Error),
}

impl std::fmt::Display for Listing {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Http(ref refused) => write!(formatter, "{refused}"),
            Self::Status(status) => write!(formatter, "a page answered HTTP {status}"),
            Self::Decode(ref decode) => write!(formatter, "a page is not a JSON array: {decode}"),
        }
    }
}

impl std::error::Error for Listing {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Http(ref refused) => Some(refused),
            Self::Status(_) => None,
            Self::Decode(ref decode) => Some(decode),
        }
    }
}

/// Read the PEM file `var` names, or `None` when the variable is unset.
fn pem_from(var: &'static str) -> Result<Option<Vec<u8>>, Setup> {
    let Some(path) = std::env::var_os(var) else {
        return Ok(None);
    };
    std::fs::read(path)
        .map(Some)
        .map_err(|source| Setup::Read { var, source })
}

/// The TLS material the environment names, applied over the defaults.
fn tls_from_env() -> Result<TlsOptions, Setup> {
    let mut tls = TlsOptions::default();
    if let Some(roots) = pem_from(ROOTS_VAR)? {
        tls = tls.with_roots_pem(roots);
    }
    if let Some(chain) = pem_from(CHAIN_VAR)? {
        tls = tls.with_client_chain_pem(chain);
    }
    if let Some(key) = pem_from(KEY_VAR)? {
        tls = tls.with_client_key_pem(key);
    }
    Ok(tls)
}

/// Build the pooled client one endpoint dials through.
///
/// The construction options are the poll's: a preview of at most
/// `preview_bytes`, cut rather than refused, under a whole-call deadline so a
/// redirect chain cannot give each hop its own timeout.
pub(super) fn build(preview_bytes: usize) -> Result<Client, Setup> {
    let options = Options::default()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .deadline(Duration::from_secs(TIMEOUT_SECS))
        .max_body_bytes(preview_bytes)
        .body_policy(BodyPolicy::Preview)
        .tls(tls_from_env()?);
    let pool = PoolConfig::default()
        .max_idle_connections(MAX_IDLE)
        .max_idle_connections_per_host(MAX_IDLE_PER_HOST)
        .max_idle_age(Duration::from_secs(MAX_IDLE_SECS));
    Client::with_pool(options, pool).map_err(Setup::Http)
}

/// Whether a streamed head announces server-sent events.
fn is_event_stream(response: &http::StreamResponse) -> bool {
    response.header_values().any(|(name, value)| {
        name.eq_ignore_ascii_case("content-type") && value.starts_with(b"text/event-stream")
    })
}

/// GET `url` and return its status and the preview bytes.
///
/// The body is read off the wire under the client's ceiling and never past
/// it. An event stream is previewed as its first event's data: its body never
/// ends, so reading it to the ceiling would wait out the deadline for bytes
/// that are not a preview of anything.
pub(super) fn preview(client: &Client, url: &str) -> Result<(u16, Vec<u8>), http::Error> {
    let response = client.get_stream(url)?;
    let status = response.status;
    if is_event_stream(&response) {
        let mut events: http::SseStream = response.into_sse();
        let first: Option<http::SseEvent> = events.next_event()?;
        let data = match first {
            Some(event) => {
                lgwks_std::trace::debug!(
                    event = event.event(),
                    id = ?event.id(),
                    retry = ?event.retry(),
                    "preview: the endpoint streams events; previewing the first"
                );
                event.data().as_bytes().to_vec()
            }
            None => Vec::new(),
        };
        return Ok((status, data));
    }
    let mut body: http::StreamedBody = response.into_body();
    let mut bytes = Vec::new();
    let mut window = [0_u8; READ_WINDOW];
    while let Some(read) = body.read_chunk(&mut window)? {
        bytes.extend(window.iter().take(read));
    }
    Ok((status, bytes))
}

/// POST `body` to `url` as `content_type`, previewed like a poll.
pub(super) fn post(
    client: &Client,
    url: &str,
    content_type: &str,
    body: &[u8],
) -> Result<Response, http::Error> {
    client.post_with(url, content_type, body, client.options())
}

/// The `rel="next"` target of a response's `Link` headers, if any.
fn next_link(response: &Response) -> Option<String> {
    response
        .header_values()
        .filter(|header| header.0.eq_ignore_ascii_case("link"))
        .filter_map(|(_, value)| std::str::from_utf8(value).ok())
        .find_map(next_in)
}

/// The `rel="next"` target in one `Link` header value.
///
/// RFC 8288: each link is `<target>` followed by `;`-separated parameters,
/// links separated by commas. Only the angle brackets delimit a target, so a
/// comma inside one never splits it.
fn next_in(value: &str) -> Option<String> {
    let mut rest = value;
    while let Some((_, after_open)) = rest.split_once('<') {
        let (target, after_close) = after_open.split_once('>')?;
        let (params, tail) = match after_close.find('<') {
            Some(at) => after_close.split_at_checked(at)?,
            None => (after_close, ""),
        };
        let is_next = params.split(';').any(|param| {
            let param = param.trim().trim_end_matches(',').trim();
            param.eq_ignore_ascii_case("rel=\"next\"") || param.eq_ignore_ascii_case("rel=next")
        });
        if is_next {
            return Some(target.to_owned());
        }
        rest = tail;
    }
    None
}

/// Walk the JSON-array listing at `first`, following `rel="next"`.
///
/// Each page is fetched whole (refused past [`PAGE_BYTES`], never cut, since
/// half an array is not a page) through the same pool the endpoint polls on.
pub(super) fn listing(client: &Client, first: &str) -> Result<Vec<json::Value>, Paginate<Listing>> {
    let options = client
        .options()
        .clone()
        .max_body_bytes(PAGE_BYTES)
        .body_policy(BodyPolicy::Whole);
    let limits = PaginationLimits::default()
        .max_pages(MAX_PAGES)
        .max_items(MAX_ITEMS);
    http::paginate(None, limits, |cursor| {
        let url = match cursor {
            Some(next) => next,
            None => first,
        };
        let response = client.get_with(url, &options).map_err(Listing::Http)?;
        if !(200..300).contains(&response.status) {
            lgwks_std::trace::debug!(
                status = response.status,
                "listing: a page answered a non-success status"
            );
            return Err(Listing::Status(response.status));
        }
        let items: Vec<json::Value> = json::from_slice(response.body()).map_err(Listing::Decode)?;
        Ok(Page::new(items, next_link(&response)))
    })
}

#[cfg(test)]
mod tests {
    use super::next_in;

    /// RFC 8288 section 3, the GitHub REST spelling: the next target is the
    /// one whose parameters name `rel="next"`, wherever it sits in the list.
    #[test]
    fn next_link_reads_the_rel_next_target() {
        let header = "<https://api.example/items?page=1>; rel=\"prev\", \
                      <https://api.example/items?page=3>; rel=\"next\", \
                      <https://api.example/items?page=9>; rel=\"last\"";
        assert_eq!(
            next_in(header).as_deref(),
            Some("https://api.example/items?page=3"),
            "the rel=\"next\" target is followed, not the first or the last"
        );
    }

    /// A comma inside a target belongs to the target: only the angle
    /// brackets delimit it.
    #[test]
    fn next_link_keeps_a_comma_inside_the_target() {
        let header = "<https://api.example/a,b?page=2>; rel=next";
        assert_eq!(
            next_in(header).as_deref(),
            Some("https://api.example/a,b?page=2"),
            "a comma inside the brackets does not split the link"
        );
    }

    /// The last page carries no `rel="next"`, and that ends the walk.
    #[test]
    fn next_link_is_none_on_the_last_page() {
        let header = "<https://api.example/items?page=1>; rel=\"first\", \
                      <https://api.example/items?page=2>; rel=\"prev\"";
        assert_eq!(next_in(header), None, "no next link ends the walk");
        assert_eq!(
            next_in("garbage without brackets"),
            None,
            "no link at all is no next"
        );
    }
}
