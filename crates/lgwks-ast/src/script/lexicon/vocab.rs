//! The structured words' rows: `graph` and `node`, `saga` and `compensate`,
//! `whenever`, and `justify` (#649, #651, #653, #654, #657, #694), and the
//! step words `remember`, `admit` and `ready` (ADR 0008).
//!
//! Each is a capability the runtime already had, now reached through
//! `script!` alone: the words are read here and nowhere else, and each
//! expands to one `lgwks_bot::script` call.

use super::{Axis, Form, Guarantee, Kind, Position, Word, form, refused, script_sim_in};

/// The simulation file for the structured words, whose flows the real macro
/// expanded.
const WORD_SIMS: &str = "crates/lgwks-bot/tests/it/sim_script_words.rs";

/// The simulation file for `whenever` and `justify`.
const FEED_SIMS: &str = "crates/lgwks-bot/tests/it/sim_script_feeds.rs";

/// A guarantee proved by a family in [`WORD_SIMS`].
const fn word_sim(axis: Axis, claim: &'static str, test: &'static str) -> Guarantee {
    script_sim_in(WORD_SIMS, axis, claim, test)
}

/// A guarantee proved by a family in [`FEED_SIMS`].
const fn feed_sim(axis: Axis, claim: &'static str, test: &'static str) -> Guarantee {
    script_sim_in(FEED_SIMS, axis, claim, test)
}

/// A block word's row.
const fn block(
    kind: Kind,
    primitive: &'static str,
    forms: &'static [Form],
    refuses: &'static [&'static str],
    guarantees: &'static [Guarantee],
    example: &'static str,
) -> Word {
    Word {
        kind,
        position: Position::Block,
        primitive,
        forms,
        refuses,
        guarantees,
        example,
    }
}

/// `graph` and `node` are one construct, so they share one example.
const GRAPH_EXAMPLE: &str = "flow ship(repo: &Repo) -> usize:\n    let report = graph:\n        node build:\n            repo.build().await.or_fail()?\n        node lint:\n            repo.lint().await.or_fail()?\n        node test needs build, lint:\n            repo.test().await.or_fail()?\n    give back report.succeeded().len()\n";

/// `saga` and `compensate` are one construct, so they share one example.
const SAGA_EXAMPLE: &str = "flow book(trip: &Trip) -> Ticket:\n    let ticket = saga booking:\n        step charge:\n            trip.charge().await.or_fail()?\n        compensate charge:\n            trip.refund().await.or_fail()?\n        step issue:\n            trip.issue().await.or_fail()?\n    give back ticket\n";

/// What `graph` and `node` share: a node starts only after its needs.
const NEEDS_RESPECTED: Guarantee = word_sim(
    Axis::Generalized,
    "a node starts only after every node it needs has succeeded; a node whose need failed never runs",
    "graph_needs_respected",
);

/// What `saga` and `compensate` share: undos run in reverse, once, on failure.
const REVERSE_ONCE: Guarantee = word_sim(
    Axis::Generalized,
    "when the saga fails, the finished steps' undos run in reverse order, once each; a saga that succeeds runs none",
    "saga_reverse_once",
);

/// `graph[, at most (limit) at once]:`.
pub(super) const GRAPH: Word = block(
    Kind::Graph,
    "`graph`",
    &[
        form(
            "`graph:` with `node name[ needs a, b]:` under it",
            "every node once, each as soon as the nodes it needs have succeeded, longest remaining chain first, as many at once as the machine sustains; the value is the run's `GraphReport`",
        ),
        form(
            "`graph, at most (limit) at once:`",
            "the same, under a limit that comes from somewhere named",
        ),
    ],
    &[
        "a line under it that is not a `node`",
        "a node needing itself, a node no line declares, or a cycle",
        "a name declared twice",
        "a concurrency bound typed as a number",
    ],
    &[
        NEEDS_RESPECTED,
        word_sim(
            Axis::Hyperscale,
            "at most the bound of nodes run at once, and each node runs once",
            "graph_bounded",
        ),
        word_sim(
            Axis::Ephemeral,
            "each node runs in its own scope, `<graph>/<node>`, with its own key",
            "graph_node_scope",
        ),
        refused(
            Axis::Idiomatic,
            "a graph that cannot run is refused at the word, naming the node",
            "graph with a cycle",
        ),
    ],
    GRAPH_EXAMPLE,
);

/// `node name[ needs a, b]:` inside a `graph`.
pub(super) const NODE: Word = block(
    Kind::Node,
    "`GraphNode::new`",
    &[form(
        "`node name[ needs a, b]:`",
        "one node of the enclosing `graph`: its body runs once, after `a` and `b` have succeeded",
    )],
    &["a `node` outside a `graph`", "a name that is not one word"],
    &[
        NEEDS_RESPECTED,
        refused(
            Axis::Idiomatic,
            "only ever stands inside a `graph`",
            "node outside a graph",
        ),
    ],
    GRAPH_EXAMPLE,
);

/// `saga name:`.
pub(super) const SAGA: Word = block(
    Kind::Saga,
    "`compensated`",
    &[form(
        "`saga name:` with `compensate step:` after the steps it undoes",
        "a named scope whose finished steps are undone, newest first, when a later line fails; its value is the body's",
    )],
    &["a name that is not one word"],
    &[
        REVERSE_ONCE,
        word_sim(
            Axis::Ephemeral,
            "each undo runs under its own key, `<saga>/undo:<step>`, never the key of the step it reverses",
            "saga_undo_own_key",
        ),
        word_sim(
            Axis::Decoupled,
            "an undo that fails is returned to the caller in place of the step's failure, and every undo still runs",
            "saga_undo_failure_returned",
        ),
        word_sim(
            Axis::Hyperscale,
            "at most `MAX_COMPENSATIONS` undos registered; one past it fails the saga",
            "saga_bounded",
        ),
    ],
    SAGA_EXAMPLE,
);

/// `compensate step:` inside a `saga`.
pub(super) const COMPENSATE: Word = block(
    Kind::Compensate,
    "`Compensations::add`",
    &[form(
        "`compensate step:`",
        "the undo of `step`, registered once that step has finished, run only if the saga later fails",
    )],
    &[
        "a `compensate` outside a `saga`",
        "a step no earlier line of the saga names",
    ],
    &[
        REVERSE_ONCE,
        refused(
            Axis::Idiomatic,
            "only ever undoes a step written before it in the same saga",
            "compensate outside a saga",
        ),
        refused(
            Axis::Generalized,
            "a step the saga never ran is refused at the word, naming the steps it has",
            "compensate an unknown step",
        ),
    ],
    SAGA_EXAMPLE,
);

/// `whenever x in batches, buffering N, <overflow>:`.
pub(super) const WHENEVER: Word = block(
    Kind::Whenever,
    "`pump`",
    &[form(
        "`whenever x in batches, buffering N, drop oldest:` (or `drop newest`, `refuse`)",
        "each event of a live feed in arrival order, each in its own scope; `batches` is awaited for the next `Option<Vec<_>>` and `None` ends the feed; at most `N` events wait, and past that the policy decides; the value is the run's `PumpReport`",
    )],
    &[
        "a feed with no `buffering N`",
        "a buffer outside `1..=65536`",
        "an overflow policy other than `drop oldest`, `drop newest` or `refuse`",
    ],
    &[
        feed_sim(
            Axis::Hyperscale,
            "never more than `N` events buffered, whatever the producer's rate; past it the declared policy, counted",
            "whenever_bounded",
        ),
        feed_sim(
            Axis::Generalized,
            "events handled in arrival order, each in its own scope with its own key",
            "whenever_in_order",
        ),
        refused(
            Axis::Idiomatic,
            "a feed with no overflow policy is refused at the word, naming the three",
            "whenever with no policy",
        ),
    ],
    "flow watch(feed: &Feed) -> u64:\n    let report = whenever tick in feed.next().await, buffering 1024, drop oldest:\n        tick.record()\n    give back report.handled\n",
);

/// `justify with (rules):`.
pub(super) const JUSTIFY: Word = block(
    Kind::Justify,
    "`Scope::with_justification`",
    &[form(
        "`justify with (rules):`",
        "the block in a scope that answers to `rules`: every step, node and retry attempt inside asks them first, and is slowed, parked or stopped as they decide",
    )],
    &["a `justify` with no `with (rules)`"],
    &[
        feed_sim(
            Axis::Decoupled,
            "a step the rules stop fails with `Justified`, naming the rule, before its body runs",
            "justify_stops_before_the_body",
        ),
        feed_sim(
            Axis::MultiTenant,
            "the rules govern only the block's own scope, never a sibling's",
            "justify_governs_its_block",
        ),
        refused(
            Axis::Idiomatic,
            "a `justify` with no rules is refused at the word, naming the form",
            "justify with no rules",
        ),
    ],
    "flow sync(api: &Api, rules: &Justify) -> u16:\n    let code = justify with (rules):\n        step call:\n            api.call().await.or_fail()?\n    give back code\n",
);

/// The simulation file for `remember`, `admit` and `ready`.
const STEP_WORD_SIMS: &str = "crates/lgwks-bot/tests/it/sim_script_durable.rs";

/// A guarantee proved by a family in [`STEP_WORD_SIMS`].
const fn durable_sim(axis: Axis, claim: &'static str, test: &'static str) -> Guarantee {
    script_sim_in(STEP_WORD_SIMS, axis, claim, test)
}

/// `remember name:`.
pub(super) const REMEMBER: Word = block(
    Kind::Remember,
    "`remembered_at`",
    &[form(
        "`remember name:`",
        "the block once per run; a resume reads its value back",
    )],
    &["a name that is not one word"],
    &[
        durable_sim(
            Axis::Ephemeral,
            "a resume replays the value, never the block",
            "remember_replays_on_resume",
        ),
        refused(
            Axis::Idiomatic,
            "a nameless `remember` is refused",
            "remember with no name",
        ),
    ],
    "flow ask(model: &Model) -> String:\n    let reply = remember ask:\n        model.ask().await.or_retry()?\n    give back reply\n",
);

/// `admit from <producer> through (gate):`.
pub(super) const ADMIT: Word = block(
    Kind::Admit,
    "`admit_at`",
    &[form(
        "`admit from model through (gate):` (or `tool`, `document`)",
        "the block's bytes admitted as a `Plan`; a refusal fails the step",
    )],
    &["a producer other than `model`, `tool`, `document`"],
    &[
        durable_sim(
            Axis::Decoupled,
            "a refusal is typed, at the step, with its provenance",
            "admit_refusal_located",
        ),
        refused(
            Axis::Idiomatic,
            "an `admit` with no gate is refused",
            "admit with no gate",
        ),
    ],
    "flow plan(reply: &[u8], gate: &Gate) -> usize:\n    let plan = admit from model through (gate):\n        reply\n    give back plan.wanted().len()\n",
);

/// `ready <pattern> from (readiness) within <duration>:`.
pub(super) const READY: Word = block(
    Kind::Ready,
    "`Readiness::wait`",
    &[form(
        "`ready x from (readiness) within 30s:`",
        "the block once `readiness` is signalled, `x` bound to it",
    )],
    &["a `ready` with no `within`"],
    &[
        durable_sim(
            Axis::Hyperscale,
            "released by the signal, never a poll; `TimedOut` past the deadline",
            "ready_released_or_timed_out",
        ),
        refused(
            Axis::Idiomatic,
            "a `ready` with no deadline is refused",
            "ready with no deadline",
        ),
    ],
    "flow rows(db: &Readiness<Pool>) -> u64:\n    let n = ready pool from (db) within 30s:\n        pool.awaited().count()\n    give back n\n",
);
