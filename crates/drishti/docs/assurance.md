# Assurance: strict obligations without fabricated green

This is how the code I have written or will write attempts to be frontier and universal.

The design reuses GPUI and the Logicalworks estate, separates semantics from scheduling and effects, and treats the immersive reference experience as a test fixture rather than a singleton assumption. Research sources and limits are in the research document. Requirements bind to owner seats and independent journey oracles. The current evidence covers only the documentation package; runtime acceptance remains blocked by the decision ledger.

## DMADV now; DMAIC for measured improvements

Define the creator's and end user's jobs and measurable critical-to-quality requirements. Validate the measurement system before comparing frameworks. Analyze existing mechanisms and root causes of coordination burden. Design ownership and state/failure contracts. Verify the actual public experience and internal invariants. Later improvements use measured baselines, reproduced failures, and regression control rather than intuition about speed or fewer lines of code.

No mathematical universality, globally optimal compute, or DO-178 certification is claimed. DO-178C-inspired discipline here means traceable requirements/design/code/tests; configuration identification; independent verification where required; structural coverage with justified gaps; problem reports; and retained evidence. A condition-coverage percentage is not MC/DC.

## Evidence states

- `passed`: the named check ran against the identified subject and its output establishes the assertion.
- `failed`: the identified check observed a violated assertion.
- `blocked`: required execution/evidence could not be obtained.
- `unmeasured`: the requirement has not been exercised.
- `not-applicable`: an explicit reviewed scope reason, never an automatic skip.

Missing evidence never becomes pass. A tool failure is not necessarily a failed user journey; it is blocked verification. A signature or digest establishes integrity/identity, not semantic correctness. One profile passing does not green another profile.

## Required runtime admission families

| Family | Required evidence / rejection rule |
| --- | --- |
| Source and scope identity | Exact source, toolchain, dependencies, feature/target, shader/script, schema and environment identity; unmapped authored files reject complete status |
| Estate dependency admission | Each authored edge has its registered owner; target closure and licenses/advisories reviewed; missing consumer contract blocks |
| Build / format / lint / docs | Actual supported configurations compile and pass strict lint/docs; no green from one convenient default feature set |
| Gate self-tests | Known-bad fixtures and negative-control mutations demonstrate refusal at each gate's real entry point |
| Contract reach | Declared operational fields have real enforcing consumers or are explicitly non-executable design; parsed-but-unused policy rejects runtime admission |
| Unit / property / concurrency | Independent reference oracles for state, ordering, generations, leases, budgets, and cancellation races |
| Fuzz / hostile boundaries | Malformed payloads, scripts, imports, giant graphs/text, overflow, recursion, resource exhaustion and unexpected host failures |
| Security / supply chain / unsafe | Authority and tenant isolation; egress restrictions; unsafe/FFI review; dependency provenance; redacted evidence |
| Formal / structural coverage | Requirement-appropriate proof, condition coverage, and MC/DC independence pairs when claimed; uncovered code and couplings explained |
| Mutation / detector sensitivity | Applicable surviving mutants and missing shards remain non-green; all Director counterexamples have regression proof; false negatives explicitly reported |
| Public human/agent experience | Fresh-state discovery, realistic mistakes, interruption/recovery, accessibility, IME, reduced motion, privacy and real terminal outcomes |
| Recovery / compatibility | Restart, corrupted/truncated state, lost acknowledgments, duplicate/reordered input, authority revocation, code reload, migrations and version skew |
| Performance / resources | Matched workloads; cold/warm distributions; input/frame/completion latency, memory, transfers, energy where measured, build/iteration cost; bounded saturation behavior |
| Independent acceptance | Distinguish author's tests from independent review/evaluator; no fabricated review or model-generated literal `true` evidence |

## Keel: preserved standards, unavailable integration

The Director reports Keel broken, and the inspected current checkout does not establish a working consumer runner. Drishti therefore has **no executable Keel configuration**. No skip-if-missing shell path or empty lane is supplied.

Historical baseline `37dd4667a4724128d1961defd43909cefbfe774b` contains the following relevant strict rules. They are provenance, not a claim that today's Keel enforces them:

| Historical rule | Source at that revision | Treatment here |
| --- | --- | --- |
| Zero scanner findings; suppression/provenance refusal | `keel-tools/src/bin/keel-zero-gate.rs` | Preserve zero-findings intent; qualify actual scanner coverage and negative controls |
| Semantic floor 1.00 | `keel-core/semantic-floor.txt` | Preserve strict baseline; do not invent current scorer execution |
| Condition coverage 100.00 | `keel-core/coverage-floor.txt` | Retain exact declared measurement scope; not automatically whole-program coverage or MC/DC |
| No silently unread contract fields | `schema/contract-reach-budget.json` | Historical explicit unconsumed fields are not permission to ignore Drishti runtime policy |
| Missing/unknown mutation shards non-green | `keel-tools/src/mutation_coverage.rs` | Preserve incomplete-evidence refusal; no invented mutation percentage |
| At most 50 files and 12,000 changed lines per PR | `schema/pr-size-budget.json` | Structure work into reviewable changes rather than weaken the limit |

Historical MI-VS 76 was advisory; the current local MI_Logical source describes a different partially calibrated metric. Neither becomes an unexplained release gate here. Historical detector fixtures also admitted known misses; Drishti must not describe that as complete defect detection. A recorded threshold never substitutes for proving that a gate actually reads and enforces it.

When Keel is restored: pin a real artifact/source identity, discover its supported consumer schema, map every applicable family to a real command and source scope, execute negative controls, and wire local/hosted checks to the same runner and retained terminal record. Do not copy the old self-qualification graph as this consumer's verification. Runtime release stays blocked until required evidence exists.

## Comparison protocol

Use equivalent implementations of context re-entry, search/preview, editing during refresh, interrupted command submission, immersive navigation, accessible forms, and compute visualization. Include idiomatic Dioxus resource management, SwiftUI state/tasks, React's appropriate action abstractions, and direct GPUI. Do not benchmark against an intentionally serial or incorrectly invalidated baseline.

Counterbalance author tasks, report participants/task counts and prior framework experience, keep hidden acceptance cases independent of the implementer, and measure maintenance changes as well as first implementation. No claimed reduction in cognitive load based solely on token counts, LOC, or the author's preference.

Fix workload/data/precision/residency/quality before runtime comparisons. Separate cold preparation from warm steady state. Measure p50/p95/p99 where enough samples support them, uncertainty, wasted work, contention, peak owned bytes, transfer bytes, and compilation/iteration time. Explicitly state timer and energy measurement provenance. Speculation must be compared to an already competent non-speculative scheduler.

## Counterexample discipline

At minimum exercise two identities and workspaces, rapid superseding inputs, duplicate commands, revoked authority, privacy-sensitive reads, all resources pinned, cancellation after dispatch, device loss, missing fonts/IME facilities, code reload during work, corrupted durable host state, and unsupported browser capability.

An uncovered Director case is an acceptance failure: reproduce the lowest violated boundary, extend the independent case inventory, fix the cause, and rerun dependent checks. Never retroactively call the case out of scope to preserve a score.

## Documentation-stage proof

The current package tests registry format, identity/reference uniqueness, planned test-path classification, local document references, unmeasured journey status, and the public documentation namespace. Negative inputs must be rejected by the bundle validator. Cargo builds, lint, tests, Rustdoc, packaging, and publication are checked at their real entry points.

These finite structural checks cannot establish research completeness, correctness of every sentence, future runtime semantics, or usable UI. Independent document review must inspect contradictions and unsupported claims. Product journeys stay `unmeasured` regardless of documentation CI results.

## Finalized-contract admission

The questionnaire and correction ledger in `docs/decisions.md` is the decision authority for the 2026-09-12 amendment. Bundle validation checks all 40 initial and 14 follow-up answer identities, the explicit engineering-derivation boundary, and the current requirement and journey inventories (37 requirements and 33 unmeasured journeys after the 2026-09-30 brand amendment). This checks inventory, not whether prose has perfectly captured every nuance; independent review must compare it with the actual answers.

J20–J30 extend acceptance to pre-Wasm forms, unified HTML/GPU interaction, missing-phone-UI rejection, strict design tokens, coherent tracked transitions, default recovery, the line-33,221 case, confined computation, compact agent operation, causal diagnosis, stable multi-target admission and dynamic capability negotiation. J31–J33 add the Logical Works brand surface: effect resolution and contrast, residency transfer honesty, and reduced-motion equivalence. They complement rather than replace the earlier adversarial cases. Native and browser reference proofs precede shared API stabilization; every advertised target still needs its own real evidence. Ordinary Cargo workflows and developer-selected hosting remain supported product requirements.

Before performance acceptance, fix representative devices/browser versions, correctness-equivalent workloads, cold/warm transfer accounting and resource ceilings. Register numeric diagnostic-time thresholds and agent egress-reduction criteria before evaluating them. "Seconds", "fraction of egress", hardware acceleration and structural safety remain goals without those measurements. A source document proposing formal verification is not a proof artifact, and a passing attenuation unit check is not end-to-end intent/outcome enforcement.
