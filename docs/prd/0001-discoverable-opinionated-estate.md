# PRD 0001: a discoverable, opinionated estate

Status: **accepted**, 2026-10-22. Director's order of 2026-10-22 ("IF A FUCKING
THING EXISTS HERE, BY DEFAULT IT WILL BE FUKING ON"). Issue: #732.

## Stakeholders and operational concept

The Director, who owns the product scope; an executing agent in a host
repository, which reaches the estate through `codegraph`, `docs/`, the generated
page and the gate's refusal text; a developer adding a crate to a new project.

## Requirements

### PR-0001.1 The default set is the decision

**Statement.** Each crate MUST carry every capability it owns in its default feature set, and a capability MAY be default-off only where a recorded measurement or an uncarrying target justifies it.

**Rationale.** A capability behind a flag requires the consumer to know it exists before it can be used; #732 measured an agent rebuilding a digest by hand for exactly that reason.

**Source.** Director, 2026-10-22 (#732).

**Priority.** must

**Fit criterion.** Every row of `docs/catalog.md` reads `on` except the three whose measured cost ADR 0003 records, and a consumer using `hash`, `trace` and `build` compiles with no feature flags.

**Verified by.** `catalog --check`; the `docs-default` lane; ADR 0003's recorded consumer run.

**Traces to.** R6, ADR 0003.

### PR-0001.2 One catalog answers "who owns this capability"

**Statement.** The repository MUST publish one capability catalog, generated from a single register, mapping each capability and its search words to its owner path, crate, feature and default state, printable as a table and as JSON.

**Rationale.** The catalog is the page a person reads and the register the gate is checked against; two sources would drift into two answers.

**Source.** Director, 2026-10-22 (#732).

**Priority.** must

**Fit criterion.** `lgwks-deps catalog` and `catalog --json` name every row of `contract/CATALOG.toml`, and `docs/catalog.md` is a byte-for-byte fresh render.

**Verified by.** `catalog --check`; `catalog::tests::the_committed_register_parses_and_names_a_file`.

**Traces to.** R13.

### PR-0001.3 A build script is a capability, not a copy

**Statement.** `lgwks_std::build` MUST carry the input digest, the checked cargo directive writer, the `OUT_DIR` and manifest-directory readers and the environment reads that separate unset from non-UTF-8.

**Rationale.** Secure-Authority's five provider build scripts each carried a hand-written sha2 digest and a `println!`, and every copy broke when the shared vendor tree was removed (#519).

**Source.** Director, 2026-10-22 ("should be in lgwks").

**Priority.** must

**Fit criterion.** A consumer with no feature flags digests its inputs and writes `rerun-if-changed` through `lgwks_std::build` (ADR 0003's run), and `crates/lgwks-std/tests/it/sim_build.rs` holds the digest's seeded families.

**Verified by.** `cargo nextest run -p lgwks_std -E 'test(/sim_build::/)'`; the `tests-std-full` lane.

**Traces to.** R1, ADR 0003.

### PR-0001.4 Tracing is complete by default

**Statement.** `lgwks_std::trace` MUST be in every default build, MUST offer an installer a binary or a build script calls once, and MUST be the only logging path library code uses.

**Rationale.** With tracing off the only logging socket available was `println!`, which the guard refuses, so an agent had no permitted way to report from a build script.

**Source.** Director, 2026-10-22 (#732); `INV-DOC-2`.

**Priority.** must

**Fit criterion.** A default build of `lgwks_std` exports `trace`, `install_default` returns `Ok` in a binary, and `trace` is the sole `tracing` facade in the estate.

**Verified by.** `docs-default` lane; `tests/it/sim_locale.rs`'s trace assertions; ADR 0003's consumer run.

**Traces to.** R13.

### PR-0001.5 The gate and the catalog agree

**Statement.** Every owner path the std-first gate can name in a refusal MUST have a catalog row, and a named owner with no row MUST be a gate defect with a regression test.

**Rationale.** A refusal that points at an owner nobody can look up is a dead end, and it is the exact dead end #732 reports.

**Source.** Director, 2026-10-22 (#732).

**Priority.** must

**Fit criterion.** `catalog --check` reads `check_std_first/scans.rs`, `lib.rs` and `refusal.rs` and exits 2 when any `lgwks_*::path` they name has no row.

**Verified by.** `catalog::tests::a_named_owner_resolves_to_its_module_row`; the `catalog` lane.

**Traces to.** ADR 0004.

### PR-0001.6 The published page lists what the estate provides

**Statement.** The Pages site MUST render the capability catalog from the register and MUST fail its job when the register cannot be read, so no page lists a capability the code does not own.

**Rationale.** The page is the one address a reader or a crawler reaches first, and a hand-maintained list drifts.

**Source.** Director, 2026-10-22 (#732).

**Priority.** should

**Fit criterion.** `render-site --check` exits 0 against the committed register, and the rendered page names every catalog row.

**Verified by.** `render_site::tests::every_catalog_row_is_on_the_page`; the Pages workflow.

**Traces to.** R13.

## Assumptions and constraints

Five workspace members stay, and no dependency is added for this work
(`INV-DEP-1`); the site deploys only where a Pages site exists, which the Pages
workflow reports rather than assumes.

## Exclusions

No crate's semver moves beyond the feature-set change ADR 0003 records. Moving
Secure-Authority's provider build scripts onto `lgwks_std::build` is #732's
acceptance row in that repository, with this catalog as its owner reference.
