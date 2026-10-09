//! The lexicon: every word of `script!`, one row each (SL-1, #380).
//!
//! The parser dispatches on this table, and `lgwks_macros` renders its
//! documentation from it: the block table in that crate's documentation, the
//! word table in its README and the one-page lexicon are each checked against
//! the table by that crate's tests, so a word cannot be added, renamed or
//! re-described in one place and not the other.
//!
//! A word is reachable only through its row. [`Kind`] is constructed nowhere
//! but in [`LEXICON`], so a kind with no row is a variant the compiler reports
//! as never constructed, which the workspace's `-D warnings` refuses; and the
//! parser dispatches on the kind of the row a line's first token names, so a
//! line whose word has no row is plain Rust and never reaches a word's reader.
//!
//! The particles inside a form (`in`, `up to`, `times`, `waiting`, `at most`,
//! `with`) are part of that word's grammar, read by its reader; they are not
//! words, and a line cannot start with one.

use lgwks_deps::proc_macro2::TokenTree;

use super::lines::{Line, ident};

/// What a word does, which is what the parser dispatches on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Kind {
    /// `flow name(inputs) -> Output:`
    Flow,
    /// `each x in xs:`
    Each,
    /// `within 2s:`
    Within,
    /// `retry up to 3 times:`
    Retry,
    /// `together:`
    Together,
    /// `step name:`
    Step,
    /// `for x in xs:`
    For,
    /// `if cond:`
    If,
    /// `else:` / `else if cond:`
    Else,
    /// `let x = <block>:`
    Let,
    /// `run other(args)`
    Run,
    /// `give back value`
    GiveBack,
    /// `fail with reason`
    Fail,
}

impl Kind {
    /// The token a line starts with to say this word.
    #[must_use]
    pub const fn spelling(self) -> &'static str {
        match self {
            Self::Flow => "flow",
            Self::Each => "each",
            Self::Within => "within",
            Self::Retry => "retry",
            Self::Together => "together",
            Self::Step => "step",
            Self::For => "for",
            Self::If => "if",
            Self::Else => "else",
            Self::Let => "let",
            Self::Run => "run",
            Self::GiveBack => "give",
            Self::Fail => "fail",
        }
    }
}

/// Where a word may stand.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Position {
    /// Opens a flow at the top level of a script.
    Flow,
    /// Opens an indented block: its line ends in `:`.
    Block,
    /// Starts a line that opens no block.
    Line,
    /// Anywhere inside a line of Rust.
    Inline,
}

impl Position {
    /// The name the README table gives this position.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Flow => "flow",
            Self::Block => "block",
            Self::Line => "line",
            Self::Inline => "inline",
        }
    }
}

/// The axis a guarantee serves (SL-4).
///
/// The nine axes decide what a word must guarantee before it is admitted; they
/// are a design requirement, not a score. Only the axes some word's guarantee
/// serves today are variants here: a variant no row names would be dead code
/// the workspace's `-D warnings` refuses, and adding it is part of admitting
/// the word that first serves it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Axis {
    /// Holds for any input and any schedule, not only the common one.
    Generalized,
    /// The caller depends on a typed answer, never on the callee's internals.
    Decoupled,
    /// Nothing outlives the run, and a rerun reproduces its keys.
    Ephemeral,
    /// Bounded in work, time and retries, however wide the input.
    Hyperscale,
    /// Idiomatic Rust: no panic, no hidden control flow.
    Idiomatic,
    /// Every key and every call stays inside one tenant.
    MultiTenant,
}

impl Axis {
    /// The axis as the README names it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Generalized => "generalized",
            Self::Decoupled => "decoupled",
            Self::Ephemeral => "ephemeral",
            Self::Hyperscale => "hyperscale",
            Self::Idiomatic => "idiomatic",
            Self::MultiTenant => "multi-tenant",
        }
    }
}

/// The test that proves a guarantee: a compile-fail case or a deterministic
/// simulation, the two kinds of evidence SL-4 accepts.
#[derive(Debug)]
#[non_exhaustive]
pub enum Evidence {
    /// A case of `lgwks_macros`' compile-fail table (`REFUSALS` in its
    /// `src/tests.rs`), named by its case.
    Refusal(&'static str),
    /// A seeded simulation: a `#[test]`, or a family a `band_family!` sweeps,
    /// in a `sim_` file named from the workspace root.
    Sim {
        /// The file, from the workspace root.
        file: &'static str,
        /// The test or family.
        test: &'static str,
    },
}

/// One thing that holds wherever a word is used, and the test that proves it.
#[derive(Debug)]
pub struct Guarantee {
    /// The axis it serves.
    axis: Axis,
    /// What holds, in one clause.
    claim: &'static str,
    /// The test that fails if it stops holding.
    evidence: Evidence,
}

impl Guarantee {
    /// Which of the nine axes this guarantee is evidence for.
    #[must_use]
    pub const fn axis(&self) -> Axis {
        self.axis
    }

    /// What holds, in one clause.
    #[must_use]
    pub const fn claim(&self) -> &'static str {
        self.claim
    }

    /// The test that fails if it stops holding.
    #[must_use]
    pub const fn evidence(&self) -> &Evidence {
        &self.evidence
    }
}

/// A guarantee proved by a simulation in `lgwks_bot`'s `sim_script.rs`, the
/// file whose flows the real macro expanded.
const fn script_sim(axis: Axis, claim: &'static str, test: &'static str) -> Guarantee {
    Guarantee {
        axis,
        claim,
        evidence: Evidence::Sim {
            file: SCRIPT_SIMS,
            test,
        },
    }
}

/// A guarantee proved by a compile-fail case.
const fn refused(axis: Axis, claim: &'static str, case: &'static str) -> Guarantee {
    Guarantee {
        axis,
        claim,
        evidence: Evidence::Refusal(case),
    }
}

/// The simulation file whose flows the real macro expanded.
const SCRIPT_SIMS: &str = "crates/lgwks-bot/tests/it/sim_script.rs";

/// One written form of a word and what it means.
#[derive(Debug)]
pub struct Form {
    /// The form as an author writes it.
    written: &'static str,
    /// What it does, in one clause.
    means: &'static str,
}

impl Form {
    /// The form as an author writes it, as Markdown.
    #[must_use]
    pub const fn written(&self) -> &'static str {
        self.written
    }

    /// What it does, in one clause, as Markdown.
    #[must_use]
    pub const fn means(&self) -> &'static str {
        self.means
    }
}

/// One word of the language: the six things SL-1 names, plus where it stands.
#[derive(Debug)]
pub struct Word {
    /// What the word does; its spelling is [`Kind::spelling`].
    kind: Kind,
    /// Where it may stand.
    position: Position,
    /// The `lgwks_bot::script` primitive the word expands to a call of.
    primitive: &'static str,
    /// Its written forms.
    forms: &'static [Form],
    /// What the word itself refuses.
    refuses: &'static [&'static str],
    /// What holds wherever the word is used, each with its proof (SL-4).
    guarantees: &'static [Guarantee],
    /// A whole script using the word.
    example: &'static str,
}

impl Word {
    /// What the word does; its spelling is [`Kind::spelling`].
    #[must_use]
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// Where it may stand.
    #[must_use]
    pub const fn position(&self) -> Position {
        self.position
    }

    /// The `lgwks_bot::script` primitive the word expands to a call of, as
    /// Markdown: code in backticks, and prose, such as a word that expands to
    /// plain Rust, as prose.
    #[must_use]
    pub const fn primitive(&self) -> &'static str {
        self.primitive
    }

    /// Its written forms. A word documented inside another word's form
    /// (`else` in `if`'s) has none of its own.
    #[must_use]
    pub const fn forms(&self) -> &'static [Form] {
        self.forms
    }

    /// What the word itself refuses, beyond the refusals every passthrough
    /// line carries.
    #[must_use]
    pub const fn refuses(&self) -> &'static [&'static str] {
        self.refuses
    }

    /// What holds wherever the word is used, each with its proof (SL-4).
    #[must_use]
    pub const fn guarantees(&self) -> &'static [Guarantee] {
        self.guarantees
    }

    /// A whole script using the word, which `lgwks_macros`' tests expand.
    #[must_use]
    pub const fn example(&self) -> &'static str {
        self.example
    }
}

/// A written form and what it means.
const fn form(written: &'static str, means: &'static str) -> Form {
    Form { written, means }
}

/// `if` and `else` are one construct, so they share one example.
const IF_ELSE_EXAMPLE: &str = "flow route(code: u16) -> u8:\n    if code < 400:\n        give back 0\n    else if code < 500:\n        give back 1\n    else:\n        give back 2\n";

/// Every word, in the order the documentation lists them.
pub const LEXICON: [Word; 13] = [
    Word {
        kind: Kind::Flow,
        position: Position::Flow,
        primitive: "`Scope::enter`",
        forms: &[form(
            "`[pub] flow name(inputs) [-> Output]:`",
            "an `async fn` taking the tenant [`Scope`] first and returning `Result<Output, FlowError>`",
        )],
        refuses: &["a promised output the last line never produces"],
        guarantees: &[
            script_sim(
                Axis::MultiTenant,
                "every step keyed by tenant and path",
                "tenants_isolated",
            ),
            refused(
                Axis::Idiomatic,
                "the panicking calls refused by resolved path",
                "unwrap by path",
            ),
        ],
        example: "flow count(items: Vec<u8>) -> usize:\n    give back items.len()\n",
    },
    Word {
        kind: Kind::Each,
        position: Position::Block,
        primitive: "`each`",
        forms: &[
            form(
                "`each x in xs:`",
                "every item, as many at once as the machine sustains, results in input order, first failure stops the rest",
            ),
            form(
                "`each x in xs, at most (limit) at once:`",
                "the same, under a limit that comes from somewhere named (an upstream's quota)",
            ),
        ],
        refuses: &[
            "a concurrency bound typed as a number",
            "a bound outside `1..=65536`",
        ],
        guarantees: &[
            script_sim(Axis::Hyperscale, "bounded fan-out", "fan_bounded"),
            script_sim(
                Axis::Generalized,
                "results in input order",
                "fan_machine_sized",
            ),
            script_sim(
                Axis::Hyperscale,
                "first failure cancels the rest",
                "fan_fail_fast",
            ),
        ],
        example: "flow sizes(pages: Vec<String>) -> Vec<usize>:\n    let sizes = each page in pages:\n        page.len()\n    give back sizes\n",
    },
    Word {
        kind: Kind::Within,
        position: Position::Block,
        primitive: "`within`",
        forms: &[form(
            "`within 2s:`",
            "the block, or `TimedOut` when the deadline passes",
        )],
        refuses: &["a zero duration", "a duration with no unit"],
        guarantees: &[Guarantee {
            axis: Axis::Hyperscale,
            claim: "a deadline, on the scope's clock",
            evidence: Evidence::Sim {
                file: "crates/lgwks-bot/tests/it/sim_clock_wiring.rs",
                test: "a_within_budget_is_refused_by_an_advance_with_no_real_wait",
            },
        }],
        example: "flow ping(host: &Host) -> u16:\n    let code = within 2s:\n        host.ping().await.or_retry()?\n    give back code\n",
    },
    Word {
        kind: Kind::Retry,
        position: Position::Block,
        primitive: "`retry`",
        forms: &[form(
            "`retry up to 3 times[, waiting 100ms]:`",
            "the block again while it fails transiently, same key each attempt, within the run's retry budget",
        )],
        refuses: &["attempts outside `1..=1000`", "a zero wait"],
        guarantees: &[
            script_sim(
                Axis::Ephemeral,
                "one idempotency key across attempts",
                "retry_one_key",
            ),
            script_sim(
                Axis::Hyperscale,
                "attempts within the run's retry budget",
                "retry_budget_holds",
            ),
        ],
        example: "flow fetch(site: &Site) -> Page:\n    let page = retry up to 3 times, waiting 200ms:\n        site.get(scope.key()).await.or_retry()?\n    give back page\n",
    },
    Word {
        kind: Kind::Together,
        position: Position::Block,
        primitive: "`try_join!`",
        forms: &[form(
            "`together:`",
            "each line underneath concurrently; `let x = ..` lines bind their result",
        )],
        refuses: &[
            "anything before its `:`",
            "an indent under a line that opens no block",
        ],
        guarantees: &[
            script_sim(
                Axis::Ephemeral,
                "every branch owned by this task",
                "together_owned",
            ),
            script_sim(
                Axis::Hyperscale,
                "first failure cancels the rest",
                "together_fail_fast",
            ),
        ],
        example: "flow both(left: &Api, right: &Api):\n    together:\n        let up = run probe(left)\n        let down = run probe(right)\n",
    },
    Word {
        kind: Kind::Step,
        position: Position::Block,
        primitive: "`Scope::enter`",
        forms: &[form(
            "`step name:`",
            "a named scope: its own key and error location",
        )],
        refuses: &["a name that is not one word"],
        guarantees: &[
            script_sim(Axis::Ephemeral, "its own step key", "step_own_key"),
            script_sim(
                Axis::Decoupled,
                "failures located at the step",
                "step_failure_located",
            ),
        ],
        example: "flow deploy(target: &Host):\n    step upload:\n        target.upload().await.or_fail()?\n",
    },
    Word {
        kind: Kind::For,
        position: Position::Block,
        primitive: "`Scope::item`",
        forms: &[form(
            "`for x in xs:`",
            "every item in turn, each in its own scope",
        )],
        refuses: &["a missing pattern or missing items"],
        guarantees: &[
            script_sim(
                Axis::Generalized,
                "items in order, one at a time",
                "for_in_order",
            ),
            script_sim(Axis::Ephemeral, "one scope per item", "for_scope_per_item"),
        ],
        example: "flow notify(users: Vec<User>):\n    for user in users:\n        run email(user)\n",
    },
    Word {
        kind: Kind::If,
        position: Position::Block,
        primitive: "none: Rust `if`",
        forms: &[form("`if cond:` / `else if cond:` / `else:`", "as written")],
        refuses: &["a missing condition"],
        guarantees: &[refused(
            Axis::Idiomatic,
            "a value only when an `else:` ends the chain",
            "if with no else as the value",
        )],
        example: IF_ELSE_EXAMPLE,
    },
    Word {
        kind: Kind::Else,
        position: Position::Block,
        primitive: "none: Rust `else`",
        forms: &[],
        refuses: &[
            "an `else` not after an `if`",
            "an `else:` that is not the last branch",
        ],
        guarantees: &[refused(
            Axis::Idiomatic,
            "only ever continues an `if` chain",
            "else without if",
        )],
        example: IF_ELSE_EXAMPLE,
    },
    Word {
        kind: Kind::Let,
        position: Position::Block,
        primitive: "the block's own",
        forms: &[form(
            "`let x = <block>:`",
            "the block's last line becomes `x`",
        )],
        refuses: &["a `let` with no `=` or nothing after it"],
        guarantees: &[script_sim(
            Axis::Idiomatic,
            "the block's failure propagates before `x` is bound",
            "let_fails_before_binding",
        )],
        example: "flow total(xs: Vec<u8>) -> u8:\n    let sum = within 1s:\n        xs.iter().sum()\n    give back sum\n",
    },
    Word {
        kind: Kind::Run,
        position: Position::Inline,
        primitive: "the callee flow",
        forms: &[form(
            "`run other(args)`",
            "call another flow in this scope, await it, propagate its failure",
        )],
        refuses: &[],
        guarantees: &[script_sim(
            Axis::MultiTenant,
            "the callee runs in this tenant's scope",
            "run_in_callers_tenant",
        )],
        example: "flow outer(site: &Site) -> usize:\n    let pages = run crawl(site)\n    give back pages\n",
    },
    Word {
        kind: Kind::GiveBack,
        position: Position::Line,
        primitive: "none: `return Ok`",
        forms: &[form("`give back value`", "return from the flow")],
        refuses: &["`give back` inside a block that has its own value"],
        guarantees: &[refused(
            Axis::Idiomatic,
            "leaves the flow, never a nested block",
            "give back from a nested block",
        )],
        example: "flow one() -> u8:\n    give back 1\n",
    },
    Word {
        kind: Kind::Fail,
        position: Position::Line,
        primitive: "`FlowError::failed` / `FlowError::transient`",
        forms: &[form(
            "`fail with reason` / `fail transiently with reason`",
            "stop with a permanent / retryable failure",
        )],
        refuses: &["a reason missing after `with`"],
        guarantees: &[script_sim(
            Axis::Decoupled,
            "a typed failure the caller can classify",
            "fail_is_classified",
        )],
        example: "flow check(ready: bool):\n    if !ready:\n        fail with \"not ready\"\n",
    },
];

/// The row `token` names, when it is a word.
///
/// The identifier is rendered once and compared with each spelling, rather
/// than asked of each row, so a lookup costs one rendering however long the
/// table grows.
pub(crate) fn word(token: &TokenTree) -> Option<&'static Word> {
    let name = ident(token)?.to_string();
    LEXICON.iter().find(|row| row.kind.spelling() == name)
}

/// Whether `token` is the word `kind`.
pub(crate) fn is(token: &TokenTree, kind: Kind) -> bool {
    word(token).is_some_and(|row| row.kind == kind)
}

/// The word a line starts with, if it starts with one.
pub(crate) fn kind_of(line: &Line) -> Option<Kind> {
    line.tokens.first().and_then(word).map(|row| row.kind)
}

/// The block words, as the refusal for an unknown block lists them: in lexicon
/// order, `else` written with the `if` it belongs to, `let` in the form that
/// opens a block, and the last one after an "or".
pub(crate) fn block_words() -> String {
    let mut words: Vec<String> = LEXICON
        .iter()
        .filter(|row| row.position == Position::Block)
        .filter_map(|row| match row.kind {
            Kind::Else => None,
            Kind::If => Some(format!(
                "`{}`/`{}`",
                Kind::If.spelling(),
                Kind::Else.spelling()
            )),
            Kind::Let => Some(format!("`{} x = <block>:`", Kind::Let.spelling())),
            kind => Some(format!("`{}`", kind.spelling())),
        })
        .collect();
    match words.pop() {
        Some(last) if !words.is_empty() => format!("{}, or {last}", words.join(", ")),
        Some(last) => last,
        None => String::new(),
    }
}
