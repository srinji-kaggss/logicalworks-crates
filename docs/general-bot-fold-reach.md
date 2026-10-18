# Folding the recorder: reaching a page that does not want to be reached

Pages of this guide: [scope and finding](general-bot-fold.md) · [what is new](general-bot-fold-new.md) · [recorder, settlement, language](general-bot-fold-language.md) · [semantic tier and mapping](general-bot-fold-semantic.md) · [reaching a page](general-bot-fold-reach.md) · [evasion and injection](general-bot-fold-evasion.md) · [sequence 1 to 5](general-bot-fold-sequence.md) · [sequence 6 to 14](general-bot-fold-frontier.md)

## 5. Reaching a page that does not want to be reached

> **Corrected after review.** The first version of this section refused "stealth"
> as a single thing and refused it outright. That was wrong twice over: the
> cluster is three unrelated mechanisms, and two of them are legitimate
> configuration rather than evasion. The review position — *some stealth is
> needed, or a way for it to crawl eventually* — is correct, and this section is
> the corrected design. What survives is narrower and better founded: the
> security-boundary flags stay refused, declared identity is the default, and
> evasion becomes a **granted, recorded** capability rather than a default flag.

The framing error is worth naming because it is easy to repeat. "Getting blocked"
has several distinct causes, and each has a different remedy:

| Cause | Remedy | Is it evasion? |
|---|---|---|
| Request rate | adaptive per-host delay and backoff | no |
| Undeclared identity | a declared crawler identity and contact | no |
| No session | persistent profile and cookie reuse | no |
| `robots.txt` | obey it; negotiate access for the rest | no |
| Recognised as automation | a granted, recorded spoof | **yes** |

Four of the five remedies are not evasion at all, and they are the ones that
actually work: most blocking is triggered by rate and by an undeclared identity,
not by a fingerprint. A bot that declares itself, paces itself, and keeps a
session gets through where a spoofing hammer gets banned. The fifth is real and
is designed for below rather than denied.

### 5.1 The browser flag cluster — still refused

`--disable-web-security`, `--disable-site-isolation-trials`, `--no-sandbox`.

- `--disable-web-security` disables the **same-origin policy**. A browser driving
  an authenticated session with the same-origin policy off can read cross-origin
  responses from the pages it visits. That is not a hardening trade-off; it is
  the removal of the boundary the rest of the web's security model is built on.
- `--disable-site-isolation-trials` removes **process isolation between
  origins**, which is the mitigation for the Spectre class of attacks.
- `--no-sandbox` removes the container's second line of defence: a renderer
  escape becomes a host escape.

`docs/security-posture.md` states the ceiling of this workspace's security case
and rests it on the bot being readable and declared. Adopting these three flags
would make that document false, and it would do so in the one place a reviewer
looks first. They are refused.

These three are also **not stealth and not needed for crawling**. They disable
the same-origin policy, process isolation, and container containment
respectively; none of them changes how a page classifies the client. Keeping
them out of the refusal is what makes the rest of this section possible: once
they are separated out, the remaining question — *how does the bot get through?*
— has honest answers.

### 5.2 Declared identity — permitted, and the default

A fixed user agent, declared Accept-Language and viewport, and a crawler
identity with a contact URL. This is configuration, not evasion, and the
distinction is not a technicality: a declared identity is one a site owner can
recognise, contact, allowlist, or block deliberately. That is the behaviour
`robots.txt` and the crawler ecosystem are built around, and it is what
`User-Agent` is for.

It is also deterministic, which the refusal above cared about: the identity is
declared once per flow and recorded, so every run of that flow presents the same
client. There is no conflict with `docs/bot-on-ecs.md` §5.

**Adopted as the default.** The estate's automation browser presents a declared
identity, and the flow document records which one.

### 5.3 Politeness, and why it is the remedy that actually works

Adaptive per-host delay, bounded per-host concurrency, and backoff that respects
`Retry-After`. This is not a concession to the target; it is the dominant term in
whether a crawl completes at all, because rate is the trigger for most blocking.
It is also already the estate's committed design: `docs/bot-on-ecs.md` §8
specifies Mercator's bounded per-host queue under a global host priority queue
and Scrapy's `AutoThrottle`-style adaptation, and records the correction that
Mercator's adaptive criteria live in US patent 6,263,364 rather than the paper.

Politeness is a **scheduling policy**, so it belongs on the frontier and not in
the browser. A delay implemented inside a page driver is one that a second
driver bypasses.

Two properties it must have to be worth having:

- **The clock is `Time<Virtual>`**, so a test can run a ten-thousand-request
  schedule without touching wall-clock (`docs/bot-on-ecs.md` §5).
- **The admission decision is a three-way verdict** — `Permitted`,
  `Throttled { retry_after }`, `Refused { reason }` — not a boolean and not a
  sleep. `Refused` is where an explicit disallow lands, and it is terminal for
  that host. This is the same shape as `Recognition` in §3.1 and for the same
  reason: *wait* and *no* are different answers, and a `bool` cannot hold both.
