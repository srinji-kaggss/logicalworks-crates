//! Redirects and deadlines: no-follow, header stripping across origins, loops, method rewriting, Location resolution, and the deadline that bounds a whole chain or a trickled body.

use super::*;

/// No-follow returns a redirect response without making a second exchange.
#[test]
fn no_follow_returns_the_redirect_without_contacting_its_target()
-> Result<(), Box<dyn std::error::Error>> {
    let reply = b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/next?token=secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let (port, server) = serve_raw(reply.to_vec())?;
    let response = get_with(
        &format!("http://127.0.0.1:{port}/start?credential=hidden"),
        &quiet().redirect_policy(RedirectPolicy::NoFollow),
    )?;
    assert_eq!(
        response.status, 302,
        "the redirect remains an HTTP response"
    );
    assert_eq!(
        response.final_target(),
        format!("http://127.0.0.1:{port}/start")
    );
    assert_eq!(response.redirect_chain().len(), 1);
    assert!(!response.final_target().contains("hidden"));
    join_server(server)?;
    Ok(())
}

/// Following records sanitized provenance and preserves ureq's no-auth redirect rule.
#[test]
fn followed_redirect_records_target_and_strips_sensitive_headers()
-> Result<(), Box<dyn std::error::Error>> {
    let destination = TcpListener::bind("127.0.0.1:0")?;
    let destination_port = destination.local_addr()?.port();
    let destination_server = fixture_thread(move || -> std::io::Result<String> {
        let (mut stream, _) = destination.accept()?;
        let request = read_request_head(&mut stream)?;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")?;
        Ok(String::from_utf8_lossy(&request).into_owned())
    })?;
    let origin = TcpListener::bind("127.0.0.1:0")?;
    let origin_port = origin.local_addr()?.port();
    let origin_server = fixture_thread(move || -> std::io::Result<()> {
        let (mut stream, _) = origin.accept()?;
        let _ = read_request_head(&mut stream)?;
        let reply = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{destination_port}/final?token=hidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(reply.as_bytes())
    })?;
    let options = quiet()
        .header("Authorization", "Bearer secret")
        .redirect_policy(RedirectPolicy::FollowAtMost(1));
    let response = get_with(
        &format!("http://127.0.0.1:{origin_port}/start?token=hidden"),
        &options,
    )?;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.final_target(),
        format!("http://127.0.0.1:{destination_port}/final")
    );
    assert_eq!(response.redirect_chain().len(), 2);
    assert!(
        response
            .redirect_chain()
            .iter()
            .all(|target| !target.contains("hidden") && !target.contains("secret")),
        "redirect provenance strips query secrets"
    );
    let received = destination_server
        .join()
        .map_err(|_| "redirect destination thread panicked")??;
    assert!(
        !received.to_ascii_lowercase().contains("authorization:"),
        "sensitive authorization is not forwarded"
    );
    origin_server
        .join()
        .map_err(|_| "redirect origin thread panicked")??;
    Ok(())
}

/// Redirect cycles stop at the declared hop limit and report a typed refusal.
#[test]
fn redirect_loop_refuses_at_the_configured_limit() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let server = fixture_thread(move || -> std::io::Result<usize> {
        let mut count = 0;
        for _ in 0..=2 {
            let (mut stream, _) = listener.accept()?;
            let _ = read_request_head(&mut stream)?;
            let reply = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/loop\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(reply.as_bytes())?;
            count += 1;
        }
        Ok(count)
    })?;
    let result = get_with(
        &format!("http://127.0.0.1:{port}/loop"),
        &quiet().redirect_policy(RedirectPolicy::FollowAtMost(2)),
    );
    assert!(matches!(
        result,
        Err(Error::Failure {
            stage: FailureStage::Redirect,
            kind: FailureKind::RedirectLimit,
            ..
        })
    ));
    assert_eq!(
        server
            .join()
            .map_err(|_| "redirect loop server panicked")??,
        3,
        "the receiver observed the initial request plus two followed hops"
    );
    Ok(())
}

/// The whole-call deadline is carried across hops: each hop here answers
/// well inside the per-phase timeout, yet the chain as a whole overruns
/// the deadline and is refused at the deadline stage (#191).
#[test]
fn a_deadline_bounds_the_whole_redirect_chain() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_done = std::sync::Arc::clone(&done);
    let server = fixture_thread(move || -> std::io::Result<usize> {
        let mut served = 0;
        // A blocking accept. Once the client has returned, the test wakes it
        // with one last connection, and `done` tells that connection apart
        // from a hop.
        while served < 10 {
            let (mut stream, _) = listener.accept()?;
            if server_done.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let _ = read_request_head(&mut stream)?;
            // Each hop answers after 200 ms, or never once the client has
            // given up on it, which is the point of the test.
            if client_hung_up_within(&mut stream, Duration::from_millis(200))? {
                served += 1;
                break;
            }
            let reply = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/hop\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            // The client may already have given up on this hop, which is
            // the point of the test, so a refused write ends the chain.
            if stream.write_all(reply.as_bytes()).is_err() {
                served += 1;
                break;
            }
            served += 1;
        }
        Ok(served)
    })?;
    let started = std::time::Instant::now();
    let result = get_with(
        &format!("http://127.0.0.1:{port}/hop"),
        &quiet()
            .timeout(Duration::from_secs(2))
            .deadline(Duration::from_millis(500))
            .redirect_policy(RedirectPolicy::FollowAtMost(10)),
    );
    let elapsed = started.elapsed();
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    // Wake an accept still waiting for a hop the client will never make. A
    // server that already stopped has dropped its listener, and the refused
    // connect means the same thing.
    let _woken = TcpStream::connect(("127.0.0.1", port));
    assert!(
        matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::Deadline,
                kind: FailureKind::Timeout,
                ..
            })
        ),
        "the chain is refused at its deadline, got {result:?}"
    );
    assert!(
        elapsed < Duration::from_millis(1_500),
        "the deadline bounds the chain, not ten hops of 200 ms: {elapsed:?}"
    );
    let served = server.join().map_err(|_| "redirect server panicked")??;
    assert!(
        (2..10).contains(&served),
        "the chain made progress, then stopped at the deadline: {served} hops"
    );
    Ok(())
}

/// A body that trickles in under the per-phase timeout is still stopped by
/// the whole-call deadline, and the failure names the deadline.
#[test]
fn a_deadline_bounds_a_trickled_body() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let server = fixture_thread(move || -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        let _ = read_request_head(&mut stream)?;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\n\r\n")?;
        for _ in 0..64 {
            // One byte every 50 ms, until the client hangs up.
            if client_hung_up_within(&mut stream, Duration::from_millis(50))? {
                break;
            }
            if stream.write_all(b"x").is_err() {
                break;
            }
        }
        Ok(())
    })?;
    let started = std::time::Instant::now();
    let result = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &quiet()
            .timeout(Duration::from_secs(2))
            .deadline(Duration::from_millis(400)),
    );
    let elapsed = started.elapsed();
    assert!(
        matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::Deadline,
                kind: FailureKind::Timeout,
                ..
            })
        ),
        "a trickled body is stopped at the call deadline, got {result:?}"
    );
    assert!(
        elapsed < Duration::from_millis(1_500),
        "the body was not read to its 3.2 s end: {elapsed:?}"
    );
    server.join().map_err(|_| "trickle server panicked")??;
    Ok(())
}

/// Body capacity grows geometrically and never past the ceiling: a 1 MiB
/// body in 8 KiB reads reallocates about log2(128) times, not 128 (#191).
#[test]
fn body_capacity_grows_geometrically_within_the_ceiling() -> Result<(), Box<dyn std::error::Error>>
{
    let body_len = 1 << 20;
    let ceiling = body_len + 3;
    let mut body = Vec::new();
    let mut growths = 0;
    let mut last_capacity = body.capacity();
    while body.len() < body_len {
        let incoming = READ_CHUNK_BYTES.min(body_len - body.len());
        reserve_for(&mut body, incoming, ceiling)?;
        body.extend(std::iter::repeat_n(b'x', incoming));
        if body.capacity() != last_capacity {
            growths += 1;
            last_capacity = body.capacity();
        }
        assert!(
            body.capacity() <= ceiling,
            "capacity stays within the ceiling"
        );
    }
    assert!(growths <= 9, "{growths} reallocations for 128 reads");

    // Near the ceiling the growth is clamped to it, not doubled past it:
    // doubling 10 would ask for 20, and the ceiling is 15.
    let mut near_limit = Vec::with_capacity(10);
    near_limit.extend([0_u8; 10]);
    reserve_for(&mut near_limit, 5, 15)?;
    assert!(near_limit.capacity() >= 15, "room for the incoming read");
    assert!(
        near_limit.capacity() < 20,
        "the reservation was clamped to the ceiling: {}",
        near_limit.capacity()
    );
    Ok(())
}

/// Redirect limits above the supported ceiling fail before any socket opens.
#[test]
fn oversized_redirect_limit_is_refused_before_dialing() {
    assert!(matches!(
        get_with(
            "http://127.0.0.1:9/",
            &quiet().redirect_policy(RedirectPolicy::FollowAtMost(
                MAX_REDIRECT_HOPS.saturating_add(1)
            )),
        ),
        Err(Error::RedirectLimitTooLarge { .. })
    ));
}

const OK_REPLY: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

/// A bodiless redirect reply with `status` pointing at `location`.
fn redirect_reply(status: &str, location: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// Answer one connection per raw reply, in order, and return every
/// request head received, lowercased, in arrival order.
fn serve_recording(
    replies: Vec<String>,
) -> std::io::Result<(u16, thread::JoinHandle<std::io::Result<Vec<String>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let handle = fixture_thread(move || -> std::io::Result<Vec<String>> {
        let mut heads = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept()?;
            let head = read_request_head(&mut stream)?;
            heads.push(String::from_utf8_lossy(&head).to_ascii_lowercase());
            stream.write_all(reply.as_bytes())?;
        }
        Ok(heads)
    })?;
    Ok((port, handle))
}

/// Only a host that is an IP address drops the resolve bound. A name, a
/// name that begins like an address, a zoned or future IPv6 form and an
/// unreadable target all keep it, because only a real literal's lookup
/// cannot block.
#[test]
fn only_an_ip_literal_host_drops_the_resolve_bound() {
    for literal in [
        "http://127.0.0.1/",
        "http://127.0.0.1:8080/path?query",
        "https://[::1]:443/",
        "http://[2001:db8::1]/",
    ] {
        assert!(host_is_literal(literal), "{literal} names an address");
    }
    for name in [
        "http://localhost/",
        "https://example.com/",
        "http://127.0.0.1.example/",
        "http://[fe80::1%25en0]/",
        "http://[v1.fe]/",
        "not a url",
    ] {
        assert!(!host_is_literal(name), "{name} keeps the resolve bound");
    }
}

/// A hop to another origin carries none of the caller's headers: not the
/// credentials ureq stripped, and not the custom ones it forwarded.
#[test]
fn a_cross_origin_redirect_carries_no_caller_header() -> Result<(), Box<dyn std::error::Error>> {
    let (destination_port, destination) = serve_recording(vec![OK_REPLY.to_owned()])?;
    let (origin_port, origin) = serve_recording(vec![redirect_reply(
        "302 Found",
        &format!("http://127.0.0.1:{destination_port}/landed"),
    )])?;
    let options = quiet()
        .header("X-Api-Key", "key-secret")
        .header("Proxy-Authorization", "Basic proxy-secret")
        .header("Authorization", "Bearer bearer-secret");
    let response = get_with(&format!("http://127.0.0.1:{origin_port}/start"), &options)?;
    assert_eq!(response.status, 200);
    let sent = origin
        .join()
        .map_err(|_| "origin server panicked")??
        .concat();
    assert!(
        sent.contains("x-api-key: key-secret"),
        "the first hop carries the caller's headers:\n{sent}"
    );
    let forwarded = destination
        .join()
        .map_err(|_| "destination server panicked")??
        .concat();
    assert!(
        !forwarded.contains("secret"),
        "no caller header reaches another origin:\n{forwarded}"
    );
    Ok(())
}

/// A hop back to the same origin keeps ordinary caller headers and drops
/// credentials; a relative `Location` resolves and loses its fragment.
#[test]
fn a_same_origin_redirect_keeps_ordinary_headers_but_not_credentials()
-> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve_recording(vec![
        redirect_reply("302 Found", "next?page=2#section"),
        OK_REPLY.to_owned(),
    ])?;
    let options = quiet()
        .header("X-Api-Key", "key-value")
        .header("Authorization", "Bearer bearer-secret");
    let response = get_with(&format!("http://127.0.0.1:{port}/dir/start"), &options)?;
    assert_eq!(
        response.final_target(),
        format!("http://127.0.0.1:{port}/dir/next")
    );
    let heads = server.join().map_err(|_| "server panicked")??;
    let (Some(first), Some(second), 2) = (heads.first(), heads.get(1), heads.len()) else {
        return Err(format!("expected two requests, saw {}", heads.len()).into());
    };
    assert!(first.contains("authorization: bearer bearer-secret"));
    assert!(
        second.starts_with("get /dir/next?page=2 "),
        "the relative Location resolves against the target, without its fragment:\n{second}"
    );
    assert!(
        second.contains("x-api-key: key-value"),
        "same origin keeps ordinary headers:\n{second}"
    );
    assert!(
        !second.contains("authorization:"),
        "credentials are not re-sent on a server's say-so:\n{second}"
    );
    Ok(())
}

/// 303 turns a POST into a bodiless GET with no content type.
#[test]
fn a_see_other_redirect_turns_a_post_into_a_bodiless_get() -> Result<(), Box<dyn std::error::Error>>
{
    let (port, server) = serve_recording(vec![
        redirect_reply("303 See Other", "/after"),
        OK_REPLY.to_owned(),
    ])?;
    let response = post_with(
        &format!("http://127.0.0.1:{port}/submit"),
        "application/json",
        b"",
        &quiet(),
    )?;
    assert_eq!(response.status, 200);
    let heads = server.join().map_err(|_| "server panicked")??;
    let (Some(first), Some(second), 2) = (heads.first(), heads.get(1), heads.len()) else {
        return Err(format!("expected two requests, saw {}", heads.len()).into());
    };
    assert!(first.starts_with("post /submit "), "{first}");
    assert!(second.starts_with("get /after "), "{second}");
    assert!(
        !second.contains("content-type:"),
        "the GET carries no body, so no content type:\n{second}"
    );
    Ok(())
}

/// 307 keeps the method, so following it would replay the POST body at a
/// target the caller never named; it is refused without a second request.
#[test]
fn a_method_keeping_redirect_of_a_post_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve_recording(vec![redirect_reply("307 Temporary Redirect", "/again")])?;
    let result = post_with(
        &format!("http://127.0.0.1:{port}/submit"),
        "application/json",
        b"",
        &quiet(),
    );
    assert!(
        matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::Redirect,
                kind: FailureKind::Transport,
                ..
            })
        ),
        "{result:?}"
    );
    let heads = server.join().map_err(|_| "server panicked")??;
    assert_eq!(heads.len(), 1, "the body was not replayed");
    Ok(())
}

/// Origins compare lowercased scheme and host and the effective port.
#[test]
fn origins_compare_scheme_host_and_effective_port() {
    assert!(origin_of("HTTP://Example.COM/a").is_some());
    assert_eq!(
        origin_of("HTTP://Example.COM/a"),
        origin_of("http://example.com:80/b")
    );
    assert_eq!(
        origin_of("https://example.com/"),
        origin_of("https://example.com:443/")
    );
    assert_ne!(
        origin_of("https://example.com/"),
        origin_of("http://example.com/"),
        "a downgrade is another origin"
    );
    assert_ne!(
        origin_of("http://example.com:8080/"),
        origin_of("http://example.com/")
    );
    assert_eq!(
        origin_of("http://[::1]:8080/"),
        origin_of("http://[::1]:8080/x")
    );
    assert!(
        origin_of("http://example.com:99999/").is_none(),
        "an unreadable port is never the same origin"
    );
}

/// A `Location` resolves to an absolute http(s) target or is refused
/// without echoing it.
#[test]
fn a_location_resolves_to_an_absolute_http_target_or_is_refused() -> Result<(), Error> {
    assert_eq!(
        resolve_location("http://a.test/dir/page", "next?x=1#top")?,
        "http://a.test/dir/next?x=1"
    );
    assert_eq!(
        resolve_location("http://a.test/dir/page", "//b.test/p")?,
        "http://b.test/p"
    );
    for hostile in [
        "ftp://a.test/token-secret",
        "javascript:token-secret",
        "http://",
        "not a uri token-secret",
    ] {
        let refusal = resolve_location("http://a.test/", hostile);
        assert!(
            matches!(
                refusal,
                Err(Error::Failure {
                    stage: FailureStage::Redirect,
                    ..
                })
            ),
            "{hostile} must be refused"
        );
        assert!(
            !format!("{refusal:?}").contains("token-secret"),
            "the Location is not echoed"
        );
    }
    Ok(())
}
