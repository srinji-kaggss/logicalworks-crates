# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (11/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 11 of 13. [Changelog index](../../CHANGELOG.md)

### Fixed (continued)

- **The invariant audit reported "enforced" for states it never examined**
  (`lgwks_deps`). A register entry naming a lint no manifest declares, a file
  that exists but declares no test, or a lint declared at `allow` all produced a
  clean verdict, so the gate could not fail. The register now answers four
  questions separately — registration validity, reference resolution, execution,
  and verified outcome — and `check` prints `resolve`/`resolved`/`attested`,
  never `enforced`. Every verdict carries a `SCOPE` line stating that no enforcer
  was executed, so a resolved reference is not read as proof the invariant holds.
  A reference that walks out of the repository (`..`) is refused at load, before
  the filesystem is consulted.
- **A malformed `policy.enforce` silently disabled both gates** (`lgwks_deps`).
  The token was read as `value == "true"`, so `True`, `"true"`, `1`, `yes`, and
  the empty string all became `false` with no diagnostic, standing the gate down.
  A closed Boolean grammar now accepts exactly `true` and `false` and otherwise
  refuses with a typed error naming the line and the value; the key set is
  closed, and a repeated key or `[policy]` section is refused rather than
  last-write-wins.
- **The self-exemption trusted a package name** (`lgwks_deps`). Any edge whose
  *name* was `lgwks_std` or `lgwks_deps` was exempt, including an external path
  or Git package wearing that name, so an unapproved source could be admitted by
  being called the right thing. The name list is gone; the only exemption is
  Cargo's own `workspace_members` list.

- **Flow validation accepted a read before initialization** (`lgwks_bot`).
  Validation tested variable names for *global* writer membership, so a document
  was accepted whenever a writer existed anywhere — including one that runs only
  after the read. Validation now runs a forward definite-assignment analysis: a
  node may read a variable only if every entry-reaching path assigns it. Reads
  of undeclared names still report `UndeclaredVariable` first, and a branch's
  own condition variable is not counted as a read because the executor never
  resolves it. New typed variant `BotError::VariableReadBeforeInit`.
- **The scanner reported discarded errors that were never errors**
  (`lgwks_deps`). ERROR-SWALLOW was a claim with no evidence behind it: every
  `.unwrap_or_default()` and every initialized `let _` was reported, including
  `Option` fallbacks and infallible bindings. Fallibility is now resolved from
  the file being scanned — a free function whose written or aliased return type
  is `Result`, a curated table of std routines, or an annotated binding — and a
  receiver resolved to a non-`Result` is never reported. Where the type is not
  visible the finding says so, and its evidence states that the shape is all
  there is rather than implying proof.
- **A shadowing binding satisfied the use check while the error was discarded**
  (`lgwks_deps`). Any ident spelling counted as a read, so a local that rebound
  the error's name cleared the finding and the error was dropped. A read now
  requires a value-position path naming the binding with no enclosing scope
  having rebound it; field and method segments, macro strings, and comments are
  not evidence, and `drop(binding)` is a discard rather than a use.

- **A validated flow could expand to gigabytes from a sub-megabyte document**
  (`lgwks_bot`). `FlowBounds` bounded *steps*, validation bounded the document's
  *shape*, and nothing bounded *bytes*: a template repeating `${answer}` 20,000
  times is a 200 KB document that interpolates to 163,840,000 bytes in one step,
  and four such steps reach 10 GB. Four ceilings now bound it — utterance,
  stored value, computed record expansion, and aggregate session retention — and
  a template's expansion is *sized* with checked arithmetic before it is
  rendered, so the amplification costs a bounded number of additions instead of
  an unbounded allocation. A template whose literal bytes alone exceed the
  ceiling is refused at load, because it could never render at any value.
  `ResourceLimits` lets an operator tighten the shipped ceilings; a document may
  tighten its own and may not raise them.
- **A declared terminal outcome was accepted and then ignored**
  (`lgwks_bot`). `Handoff` and `Refer` computed their outcome from the node
  alone, so a document that said a handoff was not authorized ran as a handoff.
  One calculation now backs both validation and execution, and a declaration
  that contradicts its node is refused at load rather than accepted and dropped.
- **An ask could offer an option the declared variable cannot store**
  (`lgwks_bot`). Validation did not decode ask candidates, so a flow offering an
  unanswerable option loaded cleanly and failed for the person answering it. The
  same decoding that runs at store time now runs at load over every candidate.
