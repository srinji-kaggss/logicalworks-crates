# ADR 0008: every capability is an SDK, an entrypoint and a `script!` word

Status: **accepted**, 2026-10-29. Refines ADR 0006 (script as the orchestration layer).

Stakeholders: the Director; the agent adding a capability; a flow author who writes only `script!`
Concerns: runtime capabilities shipped as Rust APIs only, so a flow could not reach them by a word
Requirements: R1, R2, R3, R7
Viewpoint: the vocabulary viewpoint — one row per runtime capability: its word, or why it has none
Elements and relations: `lgwks_bot::script` and `rt`, `lgwks_ast::script::LEXICON`, `lgwks_macros::emit`; `word lowers to entrypoint`

## Context

The Director, 2026-10-29: "EVERYTHING IS EITHER SDK IMPLEMENTATION AND ENTRYPOINT + SCRIPT. IF SCRIPT
IS NOT USED, AN ADR IS REQUIRED AS TO WHY SCRIPT WAS NOT ADDED AS AN ENTRYPOINT BY THE AI AGENT."

Read from the code that day: `lgwks_bot::script` re-exported `remember`, `admit` and `Readiness`,
and its own module table promised `await ready(&db, 30s)` and `admit the model's plan:`, yet
`LEXICON` held no word for any of them. ADR 0006 R3–R5 landed `RunFeed`, the axum edge, `WakeHandle`
and `Observe::cadence` as Rust APIs, and drishti's library landed beside its `script!` CLI. No
record said why.

## Decision

1. A capability is three things landed together: the SDK implementation, the entrypoint the macro
   calls (`*_at`, carrying the line's `Site`), and the word: a `LEXICON` row with a refusal case and
   a seeded simulation, its parser, its lowering, and the regenerated pages. A capability with no
   word is named in the table below with the reason, in the same change.
2. Three words are added now:

| Word | Entrypoint | Capability |
|---|---|---|
| `remember name:` | `remembered_at` | the durable step: the block once per run, replayed on resume |
| `admit from model through (gate):` (`tool`, `document`) | `admit_at` | untrusted bytes admitted as a `Plan` |
| `ready x from (readiness) within 30s:` | `Readiness::wait` | the dependant's wait on a service readiness |

3. Every other runtime capability either has its word already or has none, for the reason given:

| Capability | Word | Why |
|---|---|---|
| `each`, `each_at`, `FanOut` | `each` | `FanOut` is `each` for Rust outside a flow, with the caller's own error type |
| `within`, `retry`, `at_most`, `attempts` | `within`, `retry` | — |
| `graph`, `compensated`, `pump`, `stream_at` | `graph`, `saga`, `whenever` | — |
| `Justify` and its rules | `justify` | the rules are values a host builds; the word applies them |
| `observe`, `act`, `observe_typed`, `act_typed` | `observe`, `act` | the typed forms are the words bound with a type |
| `Readiness::new`, `ready`, `fail`, `shutdown` | none | the signalling side is the service starting, which is host code; the flow is the dependant, and `ready` is its word |
| `Gate::new`, `Surface`, `Decoder`, `PlanBudget` | none | admission's configuration is the host's; `admit` reads it |
| `Host`, `Task`, `run_under`, `resume_under` | none | the host runs flows; a flow cannot host itself |
| `Scope::root`, `with_clock`, `with_token`, `Tenant` | none | the root of a run is the host's; every word inside reads it |
| `rt::edge::serve`, `EdgeStop`, `RequestFeed`, `RunFeed` | none | ADR 0006's driver: they carry the world flows run in, and block their thread |
| `WakeHandle`, `Observe::cadence`, `EcsBot` | none | the world's own wiring; a step that drove the world would sit inside it |
| `Architecture`, `StepShape`, `ScriptTrail`, `Site` | none | the macro emits them, and tools read them |
| drishti `tokens`, `ink`, `present` | none | pure rendering called as Rust lines of drishti's `script!` CLI; no bound, deadline, key or tenant for a word to add |
| `lgwks_std`, `lgwks_deps`, `lgwks_ast` libraries | none | not orchestration; flows call them as Rust, and the word-admission census (SL-7) turns a recurring line into a word |

## Alternatives rejected

A word for every public item, host wiring included: a `serve` or `wake` word would put the driver
inside a step of the world it drives, a cycle ADR 0006 rules out, and would grow the lexicon past
the 4,000-token budget its pages are held to. Cost: words with no flow that could use them.

A record only, with no new words: the three capabilities above are flow steps the module docs had
already promised, so leaving them as Rust calls is the drift the order names. SD-11 refuses it.

## Consequences

Flows reach durability, admission and readiness by words, with refusals at expansion and keys,
sites and trail entries like every other step. The lexicon is 24 words and 4,000 tokens or under.
`lgwks_ast` gains `Kind::{Remember, Admit, Ready}`, `Admit`, `Ready` and `Producer`;
`lgwks_bot` gains `admit_at`, `remembered_at` and `StepKind::{Remember, Admit, Ready}`. Open: a
host-side `script!` form (`host`/`serve` blocks) needs its own record, with a flow that uses it.

## Correspondence

Refines ADR 0006 (script is the orchestration layer over the one world). Related: ADR 0003 (every
capability on by default), #526 (the supported surface is `script!`).

## Verification

`lgwks_macros`: `every_refusal_names_its_replacement` (the three refusals),
`the_structured_words_expand_to_their_runtime_calls`, `every_guarantee_names_a_test_that_exists`,
`the_lexicon_page_is_rendered_from_the_lexicon` (4,000-token budget), and the README and front-page
table tests. `lgwks_bot`: `sim_script_durable` (`remember_replays_on_resume`,
`admit_refusal_located`, `ready_released_or_timed_out`). `lgwks_ast`: the grammar band draws all
three words.
