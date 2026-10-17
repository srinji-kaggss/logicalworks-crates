//! The two guarantees every model call of a run is held to: no call repeats an
//! earlier one in meaning, and the tokens billed never pass the run's budget.
//!
//! Both are decided before the call is made, because a refusal after the call is
//! a bill already paid. A call is admitted only when its worst-case cost still
//! fits what has been billed plus what the calls in flight may yet bill; its cost
//! is settled when it returns, and a call that failed outright is released, so
//! asking again after an outage is not a repeat. A reply whose usage could not be
//! read is charged its whole reservation, never nothing.

use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;

use super::author::{Author, Reply};
use super::children::guard;
use super::{BenchError, fail};

/// What the run has billed and asked so far.
#[derive(Default)]
struct State {
    /// Tokens billed by finished calls, and by earlier runs of a resumed one.
    spent: u64,
    /// The worst case of the calls in flight.
    reserved: u64,
    /// Calls refused as repeats.
    repeats: u64,
    /// The meaning of every call admitted and not released.
    seen: BTreeSet<Key>,
}

/// The run's token budget and the record of what it has already asked.
pub(super) struct Ledger {
    /// The most tokens, read and written, the run may bill.
    limit: u64,
    /// The most one call's reply may bill, reasoning included.
    call_tokens: u64,
    /// The books, shared by every worker.
    state: Mutex<State>,
}

/// Where the books stand.
pub(super) struct Standing {
    /// Tokens billed.
    pub(super) spent: u64,
    /// The budget.
    pub(super) limit: u64,
    /// Calls refused as repeats.
    pub(super) repeats: u64,
}

/// What a call means, as 128 bits: two SipHash-1-3 digests, the two orders of the
/// same two strings, so a collision between different calls needs both to meet.
/// `std`'s hasher is fixed-key, so a meaning is the same wherever it is computed
/// in the run, and it needs no dependency; a collision would refuse a call, the
/// safe direction, never admit a repeat.
type Key = (u64, u64);

/// What a call means: the model and the prompt with its case and spacing folded.
///
/// Two prompts that differ only in capitals or whitespace ask the same thing, so
/// they hash alike; a `str` hashes with its own terminator, so the model is framed
/// apart from the prompt and one prompt put to two models is two calls.
fn meaning(model: &str, prompt: &str) -> Key {
    let folded = prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut low = DefaultHasher::new();
    model.hash(&mut low);
    folded.hash(&mut low);
    let mut high = DefaultHasher::new();
    folded.hash(&mut high);
    model.hash(&mut high);
    (low.finish(), high.finish())
}

/// `len` as a token count, one token a byte, which no tokenizer exceeds, so the
/// count never undercounts. Built from the length's own bytes: every target Rust
/// supports has a `usize` of at most 64 bits, so no conversion can fail.
fn bytes(len: usize) -> u64 {
    let mut raw = [0_u8; 8];
    for (slot, byte) in raw.iter_mut().zip(len.to_le_bytes()) {
        *slot = byte;
    }
    u64::from_le_bytes(raw)
}

impl Ledger {
    /// A ledger of `limit` tokens, `already` of them billed by an earlier run.
    pub(super) fn new(limit: u64, call_tokens: u64, already: u64) -> Self {
        Self {
            limit,
            call_tokens,
            state: Mutex::new(State {
                spent: already,
                ..State::default()
            }),
        }
    }

    /// The worst a call of `prompt` may bill: the prompt read at a token a byte
    /// and a reply of at most `call_tokens`.
    fn worst_case(&self, prompt: &str) -> u64 {
        bytes(prompt.len()).saturating_add(self.call_tokens)
    }

    /// Admits the call or refuses it, reserving its worst case.
    fn admit(&self, key: Key, worst: u64) -> Result<(), BenchError> {
        let mut state = guard(&self.state);
        if state.seen.contains(&key) {
            state.repeats = state.repeats.saturating_add(1);
            return fail(BenchError::Repeated, "admit");
        }
        let committed = state
            .spent
            .saturating_add(state.reserved)
            .saturating_add(worst);
        if committed > self.limit {
            return fail(
                BenchError::Refused(format!(
                    "the token budget of {} would be passed: {} billed, {} in flight, {worst} asked",
                    self.limit, state.spent, state.reserved
                )),
                "admit",
            );
        }
        state.seen.insert(key);
        state.reserved = state.reserved.saturating_add(worst);
        Ok(())
    }

    /// Books a finished call: its reservation is released and `billed` is spent.
    /// A call that never answered (`None`) releases its meaning too.
    fn settle(&self, key: &Key, worst: u64, billed: Option<u64>) {
        let mut state = guard(&self.state);
        state.reserved = state.reserved.saturating_sub(worst);
        match billed {
            Some(tokens) => state.spent = state.spent.saturating_add(tokens),
            None => {
                state.seen.remove(key);
            }
        }
    }

    /// Where the books stand now.
    pub(super) fn standing(&self) -> Standing {
        let state = guard(&self.state);
        Standing {
            spent: state.spent,
            limit: self.limit,
            repeats: state.repeats,
        }
    }
}

/// An author held to a [`Ledger`].
pub(super) struct Budgeted<'run> {
    /// The author that makes the calls.
    pub(super) inner: &'run dyn Author,
    /// The run's books.
    pub(super) ledger: &'run Ledger,
    /// The model the calls go to.
    pub(super) model: &'run str,
}

impl Author for Budgeted<'_> {
    fn ask(&self, prompt: &str) -> Result<Reply, BenchError> {
        let key = meaning(self.model, prompt);
        let worst = self.ledger.worst_case(prompt);
        self.ledger.admit(key, worst)?;
        let answered = self.inner.ask(prompt);
        let billed = match answered {
            Ok(ref reply) => {
                let tokens = reply.tokens_in.saturating_add(reply.tokens_out);
                Some(if tokens == 0 { worst } else { tokens })
            }
            Err(_) => None,
        };
        self.ledger.settle(&key, worst, billed);
        answered
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use super::{Budgeted, Ledger};
    use crate::author::{Author, Reply};
    use crate::{BenchError, fail};

    /// An author that bills a fixed cost and counts the calls it was really sent.
    struct Stub {
        calls: AtomicU64,
        tokens: (u64, u64),
        down: bool,
    }

    impl Author for Stub {
        fn ask(&self, _prompt: &str) -> Result<Reply, BenchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.down {
                return fail(
                    BenchError::Refused("the upstream is down".to_owned()),
                    "ask",
                );
            }
            Ok(Reply {
                text: String::new(),
                tokens_in: self.tokens.0,
                tokens_out: self.tokens.1,
                wall_ms: 1,
                raw: String::new(),
            })
        }
    }

    fn stub(tokens: (u64, u64), down: bool) -> Stub {
        Stub {
            calls: AtomicU64::new(0),
            tokens,
            down,
        }
    }

    fn refused(result: &Result<Reply, BenchError>, needle: &str) -> bool {
        matches!(result, Err(BenchError::Refused(why)) if why.contains(needle))
    }

    /// The Director's order of 2026-10-17: zero semantic repetition in calls. A
    /// prompt that differs only in capitals or spacing asks the same thing.
    #[test]
    fn a_repeat_in_meaning_is_refused_before_it_is_sent() {
        let inner = stub((10, 10), false);
        let ledger = Ledger::new(1_000_000, 100, 0);
        let author = Budgeted {
            inner: &inner,
            ledger: &ledger,
            model: "m",
        };
        assert!(
            author.ask("Write  a Rust\nfunction").is_ok(),
            "the first ask is made"
        );
        assert!(
            matches!(
                author.ask("write a rust function"),
                Err(BenchError::Repeated)
            ),
            "case and spacing do not make a new question"
        );
        assert!(
            author.ask("write a different function").is_ok(),
            "a new question is made"
        );
        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            2,
            "the repeat never reached the model"
        );
        assert_eq!(ledger.standing().repeats, 1, "the refusal is counted");
        let other = Budgeted {
            inner: &inner,
            ledger: &ledger,
            model: "n",
        };
        assert!(
            other.ask("write a rust function").is_ok(),
            "the same words put to another model are another call"
        );
    }

    /// A call is refused before it is made when its worst case would pass the
    /// budget, so spending never overshoots by a whole call that was avoidable.
    #[test]
    fn a_call_that_would_pass_the_budget_is_refused_before_it_is_sent() {
        let inner = stub((50, 50), false);
        let ledger = Ledger::new(250, 100, 0);
        let author = Budgeted {
            inner: &inner,
            ledger: &ledger,
            model: "m",
        };
        assert!(author.ask("a").is_ok(), "worst case 101 fits 250");
        assert!(
            author.ask("b").is_ok(),
            "101 billed as 100, 100 + 101 fits 250"
        );
        assert!(
            refused(&author.ask("c"), "token budget"),
            "200 billed and 101 more would pass 250"
        );
        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            2,
            "the refused call was never sent"
        );
        assert_eq!(ledger.standing().spent, 200, "two calls billed 100 each");
    }

    /// A resumed run starts from what the interrupted one already billed.
    #[test]
    fn a_resumed_run_carries_the_tokens_the_earlier_run_billed() {
        let inner = stub((1, 1), false);
        let ledger = Ledger::new(300, 100, 250);
        let author = Budgeted {
            inner: &inner,
            ledger: &ledger,
            model: "m",
        };
        assert!(
            refused(&author.ask("x"), "token budget"),
            "250 already billed leaves no room for a 101-token worst case"
        );
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0, "nothing was sent");
    }

    /// A call that failed outright gave no answer and billed nothing, so asking
    /// again is not a repeat; a reply with unreadable usage is charged its whole
    /// reservation.
    #[test]
    fn a_failed_call_is_released_and_an_unread_reply_is_charged_in_full() {
        let down = stub((0, 0), true);
        let ledger = Ledger::new(1_000, 100, 0);
        let author = Budgeted {
            inner: &down,
            ledger: &ledger,
            model: "m",
        };
        assert!(author.ask("again").is_err(), "the upstream is down");
        assert!(
            author.ask("again").is_err() && down.calls.load(Ordering::SeqCst) == 2,
            "the second ask reached the model: a failure is not a repeat"
        );
        assert_eq!(ledger.standing().spent, 0, "a failure bills nothing");
        let silent = stub((0, 0), false);
        let author = Budgeted {
            inner: &silent,
            ledger: &ledger,
            model: "m",
        };
        assert!(
            author.ask("abcd").is_ok(),
            "an empty-usage reply is a reply"
        );
        assert_eq!(
            ledger.standing().spent,
            104,
            "four bytes of prompt and the 100-token reply allowance"
        );
    }

    /// Concurrency 8 over 64 distinct prompts: whatever the interleaving, the
    /// books never pass the limit and the model sees exactly the admitted calls.
    #[test]
    fn eight_workers_never_pass_the_budget() {
        let inner = stub((50, 50), false);
        let ledger = Ledger::new(1_000, 100, 0);
        let made = AtomicU64::new(0);
        thread::scope(|scope| {
            for worker in 0..8_u32 {
                let (inner, ledger, made) = (&inner, &ledger, &made);
                scope.spawn(move || {
                    let author = Budgeted {
                        inner,
                        ledger,
                        model: "m",
                    };
                    for ask in 0..8_u32 {
                        if author.ask(&format!("prompt {worker}/{ask}")).is_ok() {
                            made.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                });
            }
        });
        let standing = ledger.standing();
        assert!(
            standing.spent <= standing.limit,
            "spent {} of {}",
            standing.spent,
            standing.limit
        );
        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            made.load(Ordering::SeqCst),
            "every admitted call was sent and every refused one was not"
        );
        assert_eq!(
            standing.spent,
            made.load(Ordering::SeqCst) * 100,
            "each admitted call billed 100"
        );
    }
}
