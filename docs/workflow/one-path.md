# Workflow §4, §5: one path, one opinion; compounding and surgical change

Moved from `WORKFLOW.md` on 2026-10-22, when every Markdown file in a repository
was held to 120 lines (Director, 2026-10-10). One rule changed in the move, and
only one: §4's storefront carve-out, which ADR 0003 supersedes. Everything else
is verbatim.

---

## 4. One path, one opinion, one architecture

A capability is either **the** architecture or it does not exist.

- **No default-off parallel paths.** A feature flag that makes an implementation
  a *candidate* is how work fails to carry forward: nothing exercises it, the
  gate never compiles it, and it rots into a second opinion nobody chose. If it
  is how the estate does the thing, it is on by default and the gate runs it.
- **One implementation of a job.** Two builders, two executors, or two
  "equivalent" entry points for the same work is one too many. Pick the right
  one, make it the only one, delete the other including its tests, docs, and
  feature flag.
- **Unification beats a seam.** "Parallel seams, not demolition" governs *how* a
  change lands while the tree stays green. It is not a licence to leave both
  halves standing. The seam closes in the same task.
- The default is the decision (ADR 0003, #732). A capability is opt-in only
  where a measured cost or a target that cannot carry it is recorded, and the
  register of those is `contract/CATALOG.toml`, printed by `docs/catalog.md`. A
  **consumer** states its opinion the same way: it holds the whole surface unless
  it says otherwise, and the workspace gate compiles and tests what it says.

---

## 5. Compounding, buildable, surgical

**Compounding.** Every unit, trait, or module added makes future tasks easier.
New capabilities plug in through existing traits without rewriting call sites. If
adding a feature requires rewriting half the repo, the previous architecture was
anti-compounding debt.

**Always buildable.** Every single commit compiles and passes tests. Never
commit a broken intermediate state intending to fix it in the next step. If a
refactor breaks the build for more than twenty minutes, the change radius is too
broad.

**Surgical change radius.**

- Touch only the lines the invariant requires. No gratuitous mass renames, no
  cosmetic formatting rewrites, no sweeping module migrations in the same turn
  as a semantic fix.
- Identify before touching: map callers and downstream consumers first. One
  CodeGraph query replaces five grep chains.
- Parallel seams over in-place demolition when replacing a subsystem: introduce
  the new implementation alongside the old, point callers at it one at a time,
  verify tests, then delete the old code.

Time-box is thirty minutes by default. Not terminal within the box: stop, report
the SHA and the receipt, await direction. Identical-state retry is forbidden.
