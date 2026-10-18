# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (2/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 2 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

- `EcsBuilder::with_poll_deadline`, `DEFAULT_POLL_DEADLINE` and
  `MAX_POLL_DEADLINE` (INV-BOT-123). Every source poll in a tick's observation
  wave now runs under a declared per-poll deadline, so one source that never
  resolves can no longer hold the whole tick. `MAX_IN_FLIGHT_POLLS` bounded the
  wave's fan-out but not its wait, and the wave is joined on the calling thread,
  so every other chain's action was held behind a source nobody could make
  progress for. A poll that misses its deadline is dropped mid-flight: it commits
  nothing, keeps its chain's baseline and forced-refresh mark standing, and is
  reported in `TickReport::stalled` with the chain, the source's `domain_id` and
  the budget applied. The chains beside it commit and act in the same tick.
  The deadline is measured on the wall watchdog of the crate's one declared
  clock, not its caller-advanceable counter, because a wedged source is not
  waiting for time; it is a watchdog thread the poll owns and joins on every
  path, because `Bot::tick` is `lgwks_std::task::block_on` and has no reactor for
  a timer. Zero and over-ceiling deadlines are refused at build rather than
  clamped. **Migration:** none; the default is applied to every existing bot.
  `clock` moved from `rt::clock` to the crate root (re-exported unchanged under
  `rt`), so a `--no-default-features` build can bound its sources too.
- `BotError::{PollStalled, PollDeadlineUnbounded, PollDeadlineExceeded}` and
  `StalledSource`. A cancelled poll is `NotDelivered`, so a retry classifier
  reads the next tick as a plain retry. **Migration:** a caller matching
  `BotError` exhaustively must handle the three new arms; `BotError` is
  `#[non_exhaustive]`, so an existing match already compiles with a wildcard.
- `verb::RefreshReason` and `Observe::cache_state` (INV-BOT-120). A source that
  caches can now declare that its cached baseline is unsound, naming which of
  four failures it was: `Disconnected`, `WatchOverflow`, `StaleRemoteKey` or
  `InvalidationFailed`. Before this the substrate's shortcut — do not re-poll a
  source whose value compares equal — assumed a baseline was sound, and in all
  four cases "unchanged" and "I stopped looking" are the same observation, so the
  bot settled into a permanent quiet state whose only symptom was zero fired
  effects. The reason is read from the source itself after its poll resolves and
  never guessed, because the substrate cannot know whether somebody else's
  transport is up. **Migration:** none. `cache_state` defaults to `None`, so
  every existing `Observe` impl keeps compiling and keeps its current behaviour;
  an existing source that overrides the deprecated `fingerprint` should move its
  state to `cache_state`, since that method no longer suppresses anything.
- `TickReport` and `Bot::tick_report` (INV-BOT-120, INV-BOT-121). What the last
  tick observed about its own sources, beside the count of effects it fired:
  `forced` names the chains whose baseline the tick refused and re-read and the
  cause each source declared, `superseded` names the observations replaced before
  any entry acted on them. The count says what ran; this says what had to be
  re-read to decide, and a quiet bot is exactly where only the first is
  uninformative. Published before the schedule step runs, so a tick that failed
  still reports what its sources had already declared. Every field is private
  behind an accessor. **Migration:** none; reading it is optional.
- `TickReport::watchdogs` (INV-BOT-124). The deadline watchdog threads the tick
  actually started, beside `stalled`: zero for the ordinary tick where every
  source answered on its first poll, and one per observation wave that had a
  source still pending. **Migration:** none; it is an accessor.
- Source-visible simulation evidence for the observation layer's deadline,
  watchdog, refresh and attribution rows (INV-BOT-120..124). Fourteen seeded
  properties in `tests/sim_observe_refresh.rs` drive the public
  `Bot`/`TickReport` surface over wave widths from 1 to 64: an ordinary wave
  starts no watchdog while a wave with a poll that yielded once starts exactly
  one, and the count over a run equals the number of ticks with a pending wave;
  a tick dropped mid-wave leaves the bot usable; a wedged chain keeps its
  baseline and forced-refresh mark and is re-polled next tick; a per-poll
  deadline is accepted at both edges and refused one step past the ceiling; each
  chain's own declared cause, each replaced unacted revision and every stall is
  reported against the right chain; a mass of 100/1,000/10,000 held chains never
  starves an independent chain; and two tenants never cross attribution. No
  production code changed.
- `task::DefinitionIdentity` and `task::Drift`: a recorded step value is only
  replayable under the definition that produced it. Every run-store record now
  carries the task name, a declared definition revision, the input digest, a
  declared durable-value schema id and the count of durable steps (T15).
  **Migration:** the run store's file format version moves from `\x01` to
  `\x02` and a `\x01` store is refused at open as a `FormatVersion` naming both
  versions rather than migrated — reading one as the unversioned identity would
  make every pre-version resume look compatible rather than unprovable. See the
  **lgwks_bot Breaking** section above for the migration note and the refusal's
  shape.
- `Host::run_under`, `Host::resume_under`, `Host::definition` and
  `HostBuilder::durable_codec`: the doors that carry a declared definition
  identity. `resume_under` returns a `Disposition::Refused` report carrying
  `FlowError::Incompatible` — naming the axis that disagrees — before admission,
  so no step body is polled and no record is written. Only a *declared* identity
  is compared; a run that declares none gets an identity derived from its tenant,
  task name and run id, so every durable step that already worked still records
  something stable and still resumes. The declaration is an addition, never a
  precondition.
- `Broker::adopt`: takes ownership of an environment at the generation a journal
  already on the disk was written at, and moves past it (T16). `Broker::register`
  starts at generation 1 whatever the journal holds, so a process adopting
  another worker's journal would mint warrants for a generation that worker had
  been replaced past — internally consistent and jointly wrong. The new refusals
  are `BrokerError::{Journal, ForeignEnvironment, NothingToAdopt}`.
- `script::FlowError::Store`: a typed arm carrying the run store's own
  `StoreError`. A store that cannot read its own records now reaches the caller
  as itself rather than as a `Failed` reason string, so a device refusal is
  distinguishable from a definition drift by the variant alone (INV-BOT-7,
  INV-BOT-59). `FlowError` is `#[non_exhaustive]`, so this is an additive minor
  change.
- `rt::process::CapturedStream::frames(ceiling)`, the door a caller reads its own
  child's output through: infallible, and the reason a `ProcessRun`'s captured
  stdout can be read as frames without the caller re-plumbing the bytes into a
  reader. It is `pub` because callers outside the crate read their child's
  output through it, and it is the path the T05 tests exercise rather than a
  hand-plumbed slice.
- `rt::process::read_frames`, a bounded reader for the length-framed records a
  supervised child's output carries. It reuses the crate's existing frame
  grammar (`journal::frame`) rather than restating it, so a torn tail has one
  meaning across the file stores and a subprocess's streams. A record is
  `FrameRead::Frame` only when its prefix named the bytes that followed; the two
  truncations, a malformed prefix and the caller's ceiling are refusals that
  carry no payload, and `FrameRead::payload()` returns `None` for every one of
  them. The payload ceiling is charged from the prefix before a payload is read,
  so a stream cannot request an allocation by claiming a large record. Accepting
  rows T03, T05, T21 and T22. No existing item changed.
- `rt::process::DEFAULT_FRAME_CEILING`, the one retained-byte ceiling a caller
  needs in order to read a child's framed output without inventing a bound.
