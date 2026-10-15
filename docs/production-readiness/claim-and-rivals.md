# 2. The claim under test, and 3. Why each rival should win

Part of [production readiness](../production-readiness.md), §2 and §3.

## 2. The claim under test

> `lgwks_bot` replaces every non-full-AI automation and RPA solution: UiPath,
> Automation Anywhere, WalkMe, Zapier, n8n, Node-RED, Ansible, Robocorp,
> Playwright scripts, and `cron` plus shell.

The field survey in [`framework-comparison.md`](../framework-comparison.md) tested
a weaker version of this claim and found the structural answer. Every framework
examined implements the same five parts — Observe, Decide, Act, State, Bounds —
and there is no sixth part anywhere. The differences are four parameters:
observation surface, concurrency model, decision source, and authority.

Axes 1 and 3 are where the commercial field competes. Axis 4 is **empty
everywhere in the sample**. Nothing scopes what an action may do at the
invocation, carries proof that a specific call was authorised, or records a
decision with provenance a third party can verify.

So the replacement claim has two halves and they should be scored separately.

**Half A — the mechanism.** Yes. Same five parts, and this is one of them done
in a language that makes the bounds checkable. That is a claim about category,
not about maturity.

**Half B — the product.** Not yet. Coverage, connectors, and the observed
behaviour under real-world mess are what people actually buy from RPA, and those
are the three places this crate is thinnest. See §6.

## 3. Why each rival should win

A comparison that only lists a rival's weaknesses is not evidence. Each of these
has a reason to be chosen over this crate, and that reason is real.

| Rival | Why it should win | Where it actually loses |
|---|---|---|
| **UiPath / Automation Anywhere** | Two decades of enterprise selectors, SAP and Oracle connectors, an orchestration server, attended and unattended robot fleets. Nothing here is close on packaged line-of-business coverage. | Runs its rules engine on the application's own thread and carries ambient, unbounded authority. |
| **WalkMe / Pendo / Appcues** | In-page on the live form state, which no external observer can match for fidelity. Ships as a script tag. | Same thread problem, worse: every check competes with the application for the frame budget. No declared limits, no tiered escalation. |
| **Zapier / Make / n8n / Activepieces** | Several thousand SaaS connectors, hosted, zero ops. Connect two APIs in four minutes. | Hosted means the automation's authority is whatever the vendor's OAuth token holds. No local effects, no process control, no proof of what ran. |
| **Node-RED / Huginn** | Visual flow editor, enormous contrib library, runs on a Raspberry Pi. | JavaScript interpreter on every decision, and an unbounded authority model. |
| **Ansible / Salt / Rundeck** | Agentless over SSH, idempotent modules, a whole server fleet in one playbook. | Not a state watcher at all. Reaches outside the host through an unscoped shell, and its unit of work is a playbook run rather than a reaction. |
| **Playwright / Selenium scripts** | Any page, any language binding, raw DOM and network access. | A library, not a runtime. Every caller reinvents state, retry, bounds and authority, and most of them get authority wrong. |
| **cron + shell** | Zero dependencies, zero learning curve, forty years of operational knowledge. | No observation, no change detection, no authority, no record. |

Each rival's strength is on axes 1–3 of the survey. The table is here so the
gaps in §6 are read as gaps rather than as surprises.
