//! The reply `opencode run --format json` prints: one JSON event per line, read
//! back as the text the model wrote, the tokens it was billed and its wall time.
//!
//! The model command of a run is whatever the manifest names, and a command
//! prints either one final result line (read by `read_reply`) or these events.

use lgwks_std::json::{self, Value};

use super::author::{Reply, count_of};

/// The reply the events in `raw` carry, or `None` when none is an event of the stream.
///
/// Every `text` part is the model's words, in order. Every `step-finish` part
/// carries the tokens that step was billed: what it read (fresh and cached) and
/// what it wrote (the answer and the reasoning behind it, both of which are
/// billed). The wall time runs from the first event to the last.
pub(super) fn read_events(raw: &str) -> Option<Reply> {
    let mut texts: Vec<String> = Vec::new();
    let mut finished = false;
    let (mut tokens_in, mut tokens_out) = (0_u64, 0_u64);
    let (mut first, mut last) = (u64::MAX, 0_u64);
    for line in raw.lines() {
        let Ok(event) = json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            continue;
        };
        let part = event.get("part");
        match kind {
            "text" => {
                if let Some(words) = part
                    .and_then(|found| found.get("text"))
                    .and_then(Value::as_str)
                {
                    texts.push(words.to_owned());
                }
            }
            "step_finish" => {
                finished = true;
                let tokens = part.and_then(|found| found.get("tokens"));
                let cache = tokens.and_then(|found| found.get("cache"));
                tokens_in = tokens_in
                    .saturating_add(count_of(tokens, "input"))
                    .saturating_add(count_of(cache, "read"))
                    .saturating_add(count_of(cache, "write"));
                tokens_out = tokens_out
                    .saturating_add(count_of(tokens, "output"))
                    .saturating_add(count_of(tokens, "reasoning"));
            }
            "step_start" => {}
            _ => continue,
        }
        let at = count_of(Some(&event), "timestamp");
        first = first.min(at);
        last = last.max(at);
    }
    if !finished && texts.is_empty() {
        return None;
    }
    Some(Reply {
        text: texts.join("\n"),
        tokens_in,
        tokens_out,
        wall_ms: last.saturating_sub(first.min(last)),
        raw: String::new(),
    })
}

/// What the last `error` event in `raw` says, with its HTTP status when it has one.
///
/// A call the provider refuses before any model step runs prints only this event:
/// no text, no step, no usage. It is a failed call, never a billed one.
pub(super) fn read_failure(raw: &str) -> Option<String> {
    let mut found = None;
    for line in raw.lines() {
        let Ok(event) = json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("error") {
            continue;
        }
        let error = event.get("error");
        let data = error.and_then(|held| held.get("data"));
        let said = data
            .and_then(|held| held.get("message"))
            .and_then(Value::as_str);
        let named = error
            .and_then(|held| held.get("name"))
            .and_then(Value::as_str);
        let message = match (said, named) {
            (Some(words), _) | (None, Some(words)) => words,
            (None, None) => "an error event with no message",
        };
        found = Some(match count_of(data, "statusCode") {
            0 => message.to_owned(),
            status => format!("HTTP {status}: {message}"),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{read_events, read_failure};

    /// A real `opencode run --format json` call, opencode 1.18.34 on the Go
    /// muse model with every tool off, 2026-10-17: one step, one text part,
    /// 190 tokens read, 23 written and 573 of reasoning.
    const REAL: &str = "{\"type\":\"step_start\",\"timestamp\":1792231437906,\"sessionID\":\"ses_x\",\"part\":{\"type\":\"step-start\"}}\n\
{\"type\":\"text\",\"timestamp\":1792231440891,\"sessionID\":\"ses_x\",\"part\":{\"type\":\"text\",\"text\":\"```rust\\nfn main() {}\\n```\"}}\n\
{\"type\":\"step_finish\",\"timestamp\":1792231440908,\"sessionID\":\"ses_x\",\"part\":{\"type\":\"step-finish\",\"reason\":\"stop\",\"tokens\":{\"total\":786,\"input\":190,\"output\":23,\"reasoning\":573,\"cache\":{\"write\":0,\"read\":0}}}}\n";

    /// A real `opencode run --format json` call, opencode 1.18.34, 2026-10-18: the
    /// Go key had expired, so the provider answered 401 before any step ran.
    const EXPIRED_KEY: &str = "{\"type\":\"error\",\"timestamp\":1792345049508,\"sessionID\":\"ses_x\",\"error\":{\"name\":\"APIError\",\"data\":{\"message\":\"This API key has expired. Create a new API key in OpenCode Console and replace it in your client or secret store.\",\"statusCode\":401,\"isRetryable\":false}}}\n\n";

    #[test]
    fn an_expired_key_is_a_failure_with_its_status_and_no_reply() {
        assert_eq!(
            read_failure(EXPIRED_KEY).as_deref(),
            Some(
                "HTTP 401: This API key has expired. Create a new API key in OpenCode Console and replace it in your client or secret store."
            ),
            "the provider's own words and status are kept"
        );
        assert!(
            read_events(EXPIRED_KEY).is_none(),
            "an error event is no reply, so nothing was billed"
        );
        assert!(
            read_failure(REAL).is_none(),
            "a stream that answered carries no failure"
        );
        assert!(
            read_failure("").is_none(),
            "silence is not a failure report"
        );
        assert_eq!(
            read_failure("{\"type\":\"error\"}\n").as_deref(),
            Some("an error event with no message"),
            "an error event with nothing in it is still named"
        );
    }

    #[test]
    fn a_real_event_stream_gives_text_billed_tokens_and_wall_time() {
        let reply = read_events(REAL);
        assert_eq!(
            reply
                .as_ref()
                .map(|found| (found.tokens_in, found.tokens_out, found.wall_ms)),
            Some((190, 596, 3002)),
            "reasoning is billed with the answer; the wall runs first event to last"
        );
        assert!(
            reply.is_some_and(|found| found.text.contains("fn main() {}")),
            "the text part is the reply"
        );
    }

    #[test]
    fn cached_reads_are_billed_as_reads_and_steps_add_up() {
        let raw = "{\"type\":\"step_finish\",\"timestamp\":5,\"part\":{\"tokens\":{\"input\":10,\"output\":1,\"reasoning\":2,\"cache\":{\"write\":3,\"read\":4}}}}\n\
{\"type\":\"step_finish\",\"timestamp\":9,\"part\":{\"tokens\":{\"input\":20,\"output\":5,\"reasoning\":0,\"cache\":{\"write\":0,\"read\":6}}}}\n";
        assert_eq!(
            read_events(raw).map(|found| (found.tokens_in, found.tokens_out, found.wall_ms)),
            Some((43, 8, 4)),
            "two steps: 10+3+4 and 20+6 read, 1+2 and 5 written"
        );
    }

    #[test]
    fn output_that_is_not_an_event_stream_is_no_reply() {
        assert!(
            read_events("Error: Unexpected error\nUnknown: FileSystem.readFile\n").is_none(),
            "an error banner is not a reply"
        );
        assert!(
            read_events("{\"type\":\"result\",\"finalText\":\"x\"}\n").is_none(),
            "another command's result line is read elsewhere"
        );
        assert!(read_events("").is_none(), "silence is not a reply");
    }
}
