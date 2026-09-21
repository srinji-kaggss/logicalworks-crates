# Security posture

Status: reference, and the external case. What a security reviewer can check
without trusting the vendor, where each claim stops, and the artifacts this
project must be able to produce. Companion to
[`guidance-runner-spec.md`](guidance-runner-spec.md).

## What this document is

The commercial problem is not performance. It is that a component which drives
a customer's application has to be approved by a security team whose default
answer is no, and whose reason is almost always the same: *automation that runs
inside our page is a script on our page.*

So this document does two things. It states the architectural claim that
distinguishes this component from that class, and it states — in the same place,
at the same weight — what the claim does **not** cover. A vendor document that
only does the first is worth less than nothing to a competent reviewer, because
the reviewer finds the missing ceiling themselves and then discounts everything
else.

## The claim

**`lgwks_bot` executes no code in the consumer's browser.** It is a Rust library
that drives a browser the operator controls, over CDP, resolving elements from
the structural DOM and accessibility tree. No injected script. No
`Runtime.evaluate`. No extension in the critical path.

This is not a policy statement; it is checkable. `crates/lgwks-bot` contains no
JavaScript evaluation surface — grep it. The in-page driver lives in a separate
repository and is not a dependency of the published crate.

### Its ceiling, stated first

This is a **textual argument** from the requirement's own wording. No regulator,
acquirer, or QSA has published a determination applying the scope test to an
out-of-process driver. So:

- We do **not** claim PCI DSS 6.4.3 compliance on a customer's behalf.
- We **do** claim the product supplies the mechanical evidence an assessor's
  evidence list asks for, and never executes in the consumer's browser.
- Presented to a security team, the argument goes with the Customized Approach
  Objective quoted, never as a settled interpretation.

## Why the locus is the load-bearing fact

PCI DSS v4.0.1 **6.4.3** and **11.6.1** became effective **31 March 2025** (the
rest of v4.0 was 31 March 2024). Note the numbering: 11.6 is the heading and
11.6.1 is its only sub-requirement — bare "11.6" is not a citable requirement.

Both are scoped to the consumer's browser, in three separate strings, in the
standard and its glossary:

| Source | Text |
|---|---|
| 6.4.3 | "All payment page scripts that are **loaded and executed in the consumer browser** must be managed as follows: a method is implemented to confirm that each script is authorized; a method is implemented to assure the integrity of each script; an inventory of all scripts is maintained with written business or technical justification for why each is necessary." |
| Glossary | A script is "any programming language commands or instructions on a payment page that are **processed and/or interpreted by a consumer's browser**, including commands or instructions that interact with a page's document object model (DOM)". |
| 11.6.1 | change- and tamper-detection over "the security-impacting HTTP headers and the script contents of payment pages **as received by the consumer browser**", alerting on unauthorized modification including changes, additions and deletions, at least weekly or at a TRA-derived cadence. |

The Customized Approach Objectives are tighter still — 6.4.3's is that
"unauthorized code cannot be executed on the payment page **as it is rendered in
the consumer's browser**."

The scope is the artifact the consumer's browser receives and runs. It is not
the act of driving a page.

### The PCI SSC documents the injection problem itself

The Council's Information Supplement on these two requirements names two
monitoring models, and one of them is this architecture:

> **Agentless:** a process or service (for example a headless browser) that
> regularly navigates through checkout flows, observing loaded scripts, headers,
> and behaviors without introducing additional scripts in real users' sessions.

It lists agent-based monitoring's disadvantages in its own words — "require
integration into each page; incorrectly implemented solutions could affect
performance or conflict with other scripts" — and it records the gaps in the
browser-native controls a site would use against an injected agent:

| Control | The supplement's own stated limitation |
|---|---|
| SRI | "fails silently — there is no native alert mechanism"; "does not support reporting"; "not practical for rapidly changing scripts (for example, dynamic third-party scripts)" |
| CSP | "does not maintain a baseline of normal activity"; "static and cannot track historical or expected states across sessions"; "not inherently able to detect deletions of security-impacting headers" |

An injected automation agent is therefore simultaneously **in scope** for 6.4.3
— it is a new inventory line needing written justification, and a new 11.6.1
alerting subject — and **in conflict** with the controls the merchant is using
to satisfy 6.4.3, since CSP hash-pinning and nonces break injected agents
("using nonces with third-party hosted scripts is impractical").

Two scope facts matter commercially and should be checked with the customer's
acquirer rather than assumed: 3DS scripts are carved out of 6.4.3, and SAQ A had
6.4.3, 11.6.1 and 12.3.1 **removed**, replaced by an eligibility criterion the
merchant satisfies "by using techniques such as, but not limited to, those
spelled out in PCI DSS Requirements 6.4.3 and 11.6.1."

## What the design forbids structurally

Claims enforced by convention decay. These are enforced by types and seams.
The first five are already binding on all element-resolution work, with their
grounds, in [`guidance-runner-spec.md`](guidance-runner-spec.md) — resolver
never asks a human to adjudicate an `Ambiguous` element; recognition state is
never mutated from user input; no fleet-side corpus; structural resolution only,
with any visual path restricted to Set-of-Mark identifiers and never
coordinates; failure is refusal, not repair.

The security-facing ones added here:

**1. Execution locus is a type, not a claim.** An enum whose variants are
`ConsumerBrowser` and `OperatorControlledBrowser`, where the shipped path can
only construct the second. The transport refuses to attach to a page target
originating in a user-profile tab. The claim in this document then rests on a
type the compiler checks, not on a reviewer's reading.

**2. Structural resolution needs no injected helper — because of the CDP domain
allow-list.** The domains an extension is permitted to reach include
`DOMSnapshot`, `Accessibility`, `Page` and `Input`. Everything the resolver needs
is inside that set. There is therefore no engineering reason to reach for
`Runtime.evaluate`, which is the same conclusion the PCI analysis reaches by a
different route. **The two should be stated as one architectural commitment, not
two.**

**3. Observation scope is declared and enforced, not implied.** The bot has
`GrantSet` bounding what an action may *do*, and nothing bounding what it may
*observe*. Discord's `intents` is the nearest precedent: an enumerated,
declared subscription where capturing an undeclared surface is an error rather
than a silent success. This closes the read side of the same gap.

**4. Enterprise policy can nullify the driver, so that must be a readiness
signal.** On managed fleets, `chrome.debugger.attach()` fails outright under
`ExtensionSettings` blocked hosts and under DLP or `DisableScreenshots`
configuration — which is precisely the population an enterprise sale targets.
A policy-blocked attach needs a first-class typed failure surfaced as readiness,
so fleet-wide nullification is detectable *before* a pilot rather than during it.

## The browser-native integrity stack: proves, and does not prove

Every mechanism here is an equality or authority calculus over bytes. None of
them is a safety property, and the fastest way to lose a reviewer who knows the
specifications is to imply otherwise.

| Mechanism | Status | Proves | Does **not** prove |
|---|---|---|---|
| **SRI** | W3C SRI Level 2 WD; all engines since 2015–2018 | The fetched subresource's bytes are the bytes whose hash was declared | Anything about inline scripts, runtime-injected code, or module-graph members beyond the top-level script; that the bytes are benign |
| **CSP hash-pinning** (`script-src 'sha256-…'`) | CSP Level 3 | Inline script content matches a declared hash | Anything about what that script then loads or does |
| **`'strict-dynamic'`** | CSP Level 3, live | One hash-authorized bootstrap may load descendants | — and that is the problem: it is authority to load an unbounded descendant set, so `'strict-dynamic'` and a pinned module graph are in **tension**. Choosing one is an architectural decision |
| **`require-sri-for`** | **Dead** — absent from the CSP3 grammar entirely; an open W3C issue asks whether to revive it | Nothing; do not build a claim on it | — |
| **`Integrity-Policy`** | Shipped Chrome 138 for script destinations; `sources` currently supports only `inline` | The **browser**, not the vendor, enforced an integrity requirement and reported its own violations via the Reporting API | Anything non-inline; has one engine; a merchant-side synthetic deployment proves the synthetic environment's behaviour only |
| **Import-map integrity** | Chromium 127, Safari 18 | Static imports, top-level module scripts, `modulepreload` | The transitive graph beyond what the map declares |
| **Trusted Types** | W3C WD; **all three engines** — Chrome 83 (2020), Safari 26 (2025-09-15), Firefox 148 (2026-02-24) | DOM-XSS sink assignment (`innerHTML`, `eval`, `script.src`) throws under `require-trusted-types-for` | **Not a sanitizer.** MDN is explicit that the API supplies no transformation; a policy returning its input unchanged satisfies the type system and sanitizes nothing. It constrains *how* data reaches a sink, never *whether* the code is trustworthy |

The premise that Trusted Types is Chromium-only is stale as of roughly seven
months ago, and an enterprise browser baseline may not yet reflect the Firefox
and Safari dates — worth confirming per customer rather than asserting.

**The ceiling in one line:** a hash is provenance and equality, never safety. If
the vendor's own build pipeline is the compromise vector, SRI validates the
attack.

## What we do not claim

Collected in one place so it cannot be mistaken for an omission.

- **Not a sandbox.** In-process code can always dial out directly; capability
  gating is auditable authority, not isolation. This is already stated in
  [`SECURITY.md`](../SECURITY.md) and should be quoted *at* a reviewer rather
  than left for them to find — an overstated sandbox claim is what makes a
  whole packet untrustworthy.
- **No PCI compliance opinion.** The scope argument is textual and has no
  precedent letter.
- **SRI and `Integrity-Policy` prove unchanged, not benign.**
- **SLSA Build L2 only, not L3.** The estate builds on a self-hosted runner the
  organisation controls, so L3's "signing material unreachable by user build
  steps" is false unless a separate signing service holds the key. Do not write
  "SLSA 3" in a questionnaire answer.
- **A signature proves which identity signed these exact bytes at some time,
  and nothing else** — not safety, not authorization, not intent, not
  correspondence to any source. Verification is meaningful only with a pinned
  identity policy; without `--certificate-identity` and
  `--certificate-oidc-issuer`, *any* account on the issuer is a valid signer of
  your artifact.
- **An SBOM is not a vulnerability list.** No exploitability, no reachability,
  no completeness guarantee, stale at the next dependency patch.
- **`NOT_AFFECTED` in a VEX is a producer assertion** with no independent check.
- **A questionnaire has no row for what the automation did to the customer's
  application.** CAIQ and SIG ask about logging and access control in general
  terms. The strongest artifacts in the packet answer a row that does not exist,
  so they must be volunteered into the narrative rather than cited.

## The evidence packet

**None of these twelve artifacts ship today.** They are the packet this project
would have to be able to produce, ranked by evidence value over cost, and the
table is here so the gap is legible rather than implied. "Implied" marks the
ones whose substrate already exists and which therefore need an emitter rather
than a new mechanism — read that column as distance, not as availability. A
reviewer asking for any row is asking for work, not for a file.

| # | Artifact | Proves | Does not prove | Cost | Implied? |
|---|---|---|---|---|---|
| 1 | **OpenVEX document** over the advisory position already written in prose in `SECURITY.md` | A named CVE in a named component is `not_affected` here, with justification and reopen condition | That the reasoning is correct | Hours — the argument exists, only the format is missing | **Yes** |
| 2 | **SBOM per release** (SPDX 3.0.1 or CycloneDX 1.6), from the `Cargo.lock` CI already verifies, targeting the 2026 minimum elements incl. SBOM Generation Context and Component Hash Algorithm | Component inventory by hash | Nothing about vulnerabilities or completeness | One release-job step | **Yes** |
| 3 | **Build provenance, SLSA Build L2** | Which commit, workflow and builder produced these bytes | Anything about behaviour; not L3 | Two workflow lines, no key management | Partly |
| 4 | **Signed execution transcript + journal export with replay** | No vendor code entered the consumer's browser; the exact DOM state acted upon; an independently reproducible outcome | What any given consumer's browser received; that the resolver chose a benign target | A verb plus a versioned export format | Partly — the bounded `session` journal and replay-from-journal exist; signing and a versioned export format do not |
| 5 | **Authority grant and revocation receipt** | The exact authority held, for how long, and that it was revoked | That it was well-scoped or well-granted | A serializable signed manifest | **No** — and the name is wrong for what exists. `cap`/`gate::GrantSet` hold an in-process capability *snapshot* with no revoke operation, so there is nothing to emit a revocation receipt for. See the snapshot boundary in `crates/lgwks-bot/README.md` |
| 6 | **in-toto/DSSE envelopes over 4 and 5**, with estate-owned predicate types | That receipt and artifact are bound together and signed by one identity | Predicate truth — the envelope enforces nothing | Build-time only | No |
| 7 | **Sigstore bundles, persisted** | Which OIDC identity signed, and that the entry is in a public append-only log | Anything else | Low-medium; the discipline is persisting bundles | No |
| 8 | **Script inventory with per-script written justification**, one row per observed script | The literal 6.4.3 third element, on its face | That any listed script is benign or the list complete | Derived from 4 | No |
| 9 | **Per-run digest over the script set and security-impacting headers** | The raw material for 6.4.3's inventory and 11.6.1's change/add/delete alerting in one artifact | What executed after runtime mutation | Derived from 4 | No |
| 10 | **OpenSSF Scorecard + OSPS Baseline self-assessment** keyed to published control IDs | Repo hygiene and branch/artifact posture, re-runnable at a public URL | Code quality | An Action plus a maintained table | Partly |
| 11 | **SSDF-mapped questionnaire answers** — each row answered by a practice ID plus an artifact above | Control coverage in the reviewer's vocabulary | Nothing new | Low, once 1–10 exist | No |
| 12 | **SOC 2 Type II / ISO 27001** | That the organisation's *process* operated over a period | That this binary came from that commit | High — audit fees plus a 3–12 month window | No — pursue under deal pressure |

Three ordering decisions worth stating:

**Item 7 is what makes items 3, 6 and 8 mean anything to a stranger.** Without a
pinned identity policy and the persisted bundle, the signature is an assertion
rather than evidence.

**Items 1, 2, 3 and 10 are the cheap ones** — a reviewer can check them without
cooperating with the vendor.

**Items 4, 5 and 8 are where the verifiability properties are actually
differentiated**, and they are the two things no standard questionnaire asks
about. A packet that produced only commodity provenance would be
indistinguishable from any other vendor's.

Note on Sigstore specifically: the log went to a tile-backed v2 and **stopped
serving lookup APIs** — the inclusion proof is handed over at upload and not
retrievable later. The bundle is therefore the artifact to persist, in the
hash-pinned content store, which is exactly the persistence layer the log
deliberately removed.

## The five questions this component trips

A browser-driving automation component trips these regardless of which
questionnaire is used (CAIQ v4.1 / CCM v4.1 domain codes, SIG 2025):

| Question | Domain | Answered by |
|---|---|---|
| Where are session cookies, tokens and credentials stored, and are they encrypted at rest? | IAM, CEK | Item 5; the `guard` and `access` seams |
| What network egress is the runner permitted, and how is that enforced? | DCS | `GrantSet`; declared observation intents |
| Can the customer revoke the automation's access, and how quickly? | IAM | Item 5; the two-clock revocation model |
| Are actions against our application logged in a form we can retrieve and verify? | LOG | Items 4 and 9 — answered as a *customer-facing export*, not an internal capability |
| What data leaves our boundary? | DCS | Declared observation intents; the egress grant |

Answer in two columns and say which is which: **technically verifiable** (SSDF
PS.3, PW.4, RV.1; items 1–10) versus **process, evidenced by report** (item 12).
A reviewer handed only the second column concludes there is no technical story.

## Sources, and what was not retrieved

Primary: PCI DSS v4.0.1 and the PCI SSC Information Supplement *Payment Page
Security and Preventing E-Skimming — Guidance for PCI DSS Requirements 6.4.3 and
11.6.1* (v1.0, March 2025); PCI SSC *Summary of Changes r1* (August 2024);
W3C SRI Level 2, CSP Level 3 (WD-CSP3-20260729), and Trusted Types
(WD-trusted-types-20251103); MDN and web-platform-dx for shipping status; Chrome
Web Store *Additional Requirements for Manifest V3* and the `chrome.debugger`
reference; SLSA v1.2; the in-toto attestation spec v1.1.1; CISA's *2026 Minimum
Elements for SBOM*; CISA's VEX minimum requirements; NIST SP 800-218 SSDF; the
CISA Secure Software Development Attestation Form; OpenSSF Scorecard and OSPS
Baseline (2025-10-10).

**Not retrieved, and no assertion here should be repeated to a QSA without
them:**

- **Table 7 of the PCI supplement** — the per-element testing procedures and
  expected evidence for 6.4.3 — and the ROC Template's verbatim 11.6.1 testing
  procedures. This is the single most load-bearing gap.
- Mozilla's `Integrity-Policy` and import-map-integrity positions were read as
  issue titles, not bodies.
- `chrome.userScripts` world-isolation semantics and developer-mode gating.
- Whether CycloneDX has shipped past 1.6, and ISO/IEC 27001's current edition
  as of 2026-09.

**The one claim with no precedent behind it** is the load-bearing one: that an
operator-controlled browser is outside 6.4.3's scope. It rests on the
requirement's own repeated "consumer browser" wording. It is a textual argument.
Present it as one.
