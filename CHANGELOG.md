# Changelog

All notable changes to the five crates are recorded here. Versions move
independently; each release lists per-crate deltas. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
semantic from 1.0.0: a major bump breaks, a minor adds, a patch fixes
(`docs/releasing.md` §3). Before 1.0.0 any minor could break, and those
breaks are listed explicitly under the crate.

## Releases, newest first

- Unreleased: `lgwks_ast` exposes its parser, diagnostics and scanner as a documented supported surface, and `lgwks_deps` exposes its gate and a `storefront` module, so an agent can compile against the code-understanding and admission SDK instead of only `script!` (INV-SURFACE-1, #526).
- lgwks_std 3.0.1 / lgwks_bot 3.1.0 / lgwks_ast 2.1.0 / lgwks_deps 4.1.0 / lgwks_macros 1.1.3 - 2026-10-21: [notes](docs/changelog/01-lgwks-std-3-0-1-lgwks-bot-3-1-2026-10-21.md)
- lgwks_std 3.0.0 / lgwks_bot 3.0.0 / lgwks_ast 2.0.0 / lgwks_deps 4.0.0 / lgwks_macros 1.1.2 - 2026-10-18: [1](docs/changelog/00-lgwks-std-3-0-0-lgwks-bot-3-0-p1.md) · [2](docs/changelog/00-lgwks-std-3-0-0-lgwks-bot-3-0-p2.md)
- lgwks_std 2.2.0 / lgwks_bot 2.2.0 / lgwks_deps 3.0.1 - 2026-10-07: [1](docs/changelog/02-lgwks-std-2-2-0-lgwks-bot-2-2-p1.md) · [2](docs/changelog/02-lgwks-std-2-2-0-lgwks-bot-2-2-p2.md)
- lgwks_std 2.1.0 / lgwks_bot 2.1.0 - 2026-10-06: [notes](docs/changelog/03-lgwks-std-2-1-0-lgwks-bot-2-1.md)
- lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06: [1](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p1.md) · [2](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p2.md) · [3](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p3.md) · [4](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p4.md) · [5](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p5.md) · [6](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p6.md) · [7](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p7.md) · [8](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p8.md) · [9](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p9.md) · [10](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p10.md) · [11](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p11.md) · [12](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p12.md) · [13](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p13.md) · [14](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p14.md) · [15](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p15.md) · [16](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p16.md) · [17](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p17.md) · [18](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p18.md) · [19](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p19.md) · [20](docs/changelog/04-lgwks-std-2-0-0-lgwks-ast-1-1-p20.md)
- lgwks_deps 2.0.0 - 2026-10-05: [notes](docs/changelog/05-lgwks-deps-2-0-0-2026-10-05.md)
- lgwks_std 1.1.0 / lgwks_deps 1.1.0 / lgwks_macros 1.1.0 / lgwks_bot 1.1.0 - 2026-10-05: [1](docs/changelog/06-lgwks-std-1-1-0-lgwks-deps-1-1-p1.md) · [2](docs/changelog/06-lgwks-std-1-1-0-lgwks-deps-1-1-p2.md)
- lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04: [1](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p1.md) · [2](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p2.md) · [3](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p3.md) · [4](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p4.md) · [5](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p5.md) · [6](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p6.md) · [7](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p7.md) · [8](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p8.md) · [9](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p9.md) · [10](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p10.md) · [11](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p11.md) · [12](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p12.md) · [13](docs/changelog/07-lgwks-std-1-0-0-lgwks-ast-1-0-p13.md)
- lgwks_std 0.10.0 / lgwks_ast 0.4.0 / lgwks_deps 0.4.0 / lgwks_bot 0.8.0 / lgwks_macros 0.1.2 - 2026-09-30: [notes](docs/changelog/08-lgwks-std-0-10-0-lgwks-ast-0-4.md)
- lgwks_std 0.9.0 / lgwks_deps 0.3.0 / lgwks_bot 0.7.0 / lgwks_macros 0.1.1 - 2026-09-30: [notes](docs/changelog/09-lgwks-std-0-9-0-lgwks-deps-0-3.md)
- lgwks_std 0.8.0 / lgwks_ast 0.3.0 / lgwks_deps 0.2.0 / lgwks_bot 0.6.0 / lgwks_macros 0.1.0 - 2026-09-30: [1](docs/changelog/10-lgwks-std-0-8-0-lgwks-ast-0-3-p1.md) · [2](docs/changelog/10-lgwks-std-0-8-0-lgwks-ast-0-3-p2.md)
- lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 - 2026-09-28: [1](docs/changelog/11-lgwks-std-0-7-0-lgwks-bot-0-5-p1.md) · [2](docs/changelog/11-lgwks-std-0-7-0-lgwks-bot-0-5-p2.md) · [3](docs/changelog/11-lgwks-std-0-7-0-lgwks-bot-0-5-p3.md) · [4](docs/changelog/11-lgwks-std-0-7-0-lgwks-bot-0-5-p4.md) · [5](docs/changelog/11-lgwks-std-0-7-0-lgwks-bot-0-5-p5.md)
- 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above: [1](docs/changelog/12-2026-09-21-cut-never-published-p1.md) · [2](docs/changelog/12-2026-09-21-cut-never-published-p2.md) · [3](docs/changelog/12-2026-09-21-cut-never-published-p3.md) · [4](docs/changelog/12-2026-09-21-cut-never-published-p4.md) · [5](docs/changelog/12-2026-09-21-cut-never-published-p5.md) · [6](docs/changelog/12-2026-09-21-cut-never-published-p6.md) · [7](docs/changelog/12-2026-09-21-cut-never-published-p7.md) · [8](docs/changelog/12-2026-09-21-cut-never-published-p8.md) · [9](docs/changelog/12-2026-09-21-cut-never-published-p9.md) · [10](docs/changelog/12-2026-09-21-cut-never-published-p10.md) · [11](docs/changelog/12-2026-09-21-cut-never-published-p11.md) · [12](docs/changelog/12-2026-09-21-cut-never-published-p12.md) · [13](docs/changelog/12-2026-09-21-cut-never-published-p13.md)
- lgwks_deps 0.1.12 - 2026-09-20: [notes](docs/changelog/13-lgwks-deps-0-1-12-2026-09-20.md)
- lgwks_std 0.6.6 / lgwks_bot 0.4.2 / lgwks_ast 0.2.2 / lgwks_deps 0.1.11 - 2026-09-20: [notes](docs/changelog/14-lgwks-std-0-6-6-lgwks-bot-0-4.md)
- lgwks_std 0.6.5 / lgwks_bot 0.4.1 / lgwks_ast 0.2.1 / lgwks_deps 0.1.10 - 2026-09-20: [notes](docs/changelog/15-lgwks-std-0-6-5-lgwks-bot-0-4.md)
- lgwks_bot 0.4.0 / lgwks_std 0.6.4 / lgwks_deps 0.1.9 / lgwks_ast 0.2.0 - 2026-09-20: [1](docs/changelog/16-lgwks-bot-0-4-0-lgwks-std-0-6-p1.md) · [2](docs/changelog/16-lgwks-bot-0-4-0-lgwks-std-0-6-p2.md)
- lgwks_deps 0.1.8 - 2026-09-17: [notes](docs/changelog/17-lgwks-deps-0-1-8-2026-09-17.md)
- lgwks_std 0.6.3 - 2026-09-13: [notes](docs/changelog/18-lgwks-std-0-6-3-2026-09-13.md)
- lgwks_bot 0.3.2 - 2026-09-13: [notes](docs/changelog/19-lgwks-bot-0-3-2-2026-09-13.md)
- lgwks_ast 0.1.3 - 2026-09-13: [notes](docs/changelog/20-lgwks-ast-0-1-3-2026-09-13.md)
- lgwks_deps 0.1.7 - 2026-09-13: [notes](docs/changelog/21-lgwks-deps-0-1-7-2026-09-13.md)
- Shared repo bar (all four crates, 2026-09-13): [notes](docs/changelog/22-shared-repo-bar-all-four-crate.md)
- 0.6.2 `lgwks_std` - 2026-09-13: [notes](docs/changelog/23-0-6-2-lgwks-std-2026-09-13.md)
- 0.3.1 `lgwks_bot` - 2026-09-13: [notes](docs/changelog/24-0-3-1-lgwks-bot-2026-09-13.md)
- 0.1.6 `lgwks_deps` - 2026-09-13: [notes](docs/changelog/25-0-1-6-lgwks-deps-2026-09-13.md)
- 0.1.2 `lgwks_ast` - 2026-09-13: [notes](docs/changelog/26-0-1-2-lgwks-ast-2026-09-13.md)
