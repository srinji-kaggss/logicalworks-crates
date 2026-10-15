//! The rows a caller writes and reads: run starts, policies, closes, run and lane rows, and open runs.

/// What a run records before its first lane.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunStart<'a> {
    /// The run id, unique across tenants.
    pub run_id: &'a str,
    /// The tenant that owns the run.
    pub tenant: &'a str,
    /// The repository under test.
    pub repo: &'a str,
    /// The host the coordinator runs on.
    pub host: &'a str,
    /// The coordinator's process id.
    pub coordinator_pid: u32,
    /// The coordinator's lock, relative to the state directory.
    pub coordinator_lock: &'a str,
    /// When the run started, as an opaque string the caller mints.
    pub started_at: &'a str,
}

impl<'a> RunStart<'a> {
    /// The run's opening record, borrowing every field from the caller.
    ///
    /// Borrowed rather than owned because the record is framed and written
    /// inside [`TenantStore::begin_run`](super::TenantStore::begin_run) and never
    /// outlives the call.
    #[must_use]
    pub fn new(
        run_id: &'a str,
        tenant: &'a str,
        repo: &'a str,
        host: &'a str,
        coordinator_pid: u32,
        coordinator_lock: &'a str,
        started_at: &'a str,
    ) -> Self {
        Self {
            run_id,
            tenant,
            repo,
            host,
            coordinator_pid,
            coordinator_lock,
            started_at,
        }
    }
}

/// A tenant's immutable policy: the gates as stored, the hardness level, and
/// when it was recorded.
///
/// The gates are opaque here — the string `logical_ci` wrote — because
/// decoding them against that crate's family ladder is its job, and this store
/// stays decoupled from it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Policy {
    /// The gate set, exactly as recorded. Private because a policy is
    /// written once and never rewritten; readers use [`gates_json`](Self::gates_json).
    gates_json: String,
    /// The hardness level, exactly as recorded.
    pub hardness: u8,
    /// When the policy was recorded. Private for the same reason as the gates.
    created_at: String,
}

impl Policy {
    /// A policy exactly as the store holds it.
    #[must_use]
    pub fn new(gates_json: String, hardness: u8, created_at: String) -> Self {
        Self {
            gates_json,
            hardness,
            created_at,
        }
    }

    /// The gate set, exactly as recorded.
    #[must_use]
    pub fn gates_json(&self) -> &str {
        &self.gates_json
    }

    /// When the policy was recorded.
    #[must_use]
    pub fn created_at(&self) -> &str {
        &self.created_at
    }
}

/// How a closed run ended: when, with what verdict, and what it sealed.
///
/// Grouped rather than threaded as five arguments through every row
/// constructor, because the five travel together from every closer and a
/// second order of them would be a second definition of one fact.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunClose {
    /// When the run finished. Private; readers use [`finished_at`](Self::finished_at).
    finished_at: String,
    /// The verdict the run closed with. Private; readers use [`verdict`](Self::verdict).
    verdict: String,
    /// Why the run closed the way it did, if it named one.
    pub reason: Option<String>,
    /// The sealed record, if the run sealed one.
    pub record: Option<String>,
    /// The seal, if the run sealed one.
    pub seal: Option<String>,
}

impl RunClose {
    /// How a run closed, exactly as the store holds it.
    #[must_use]
    pub fn new(
        finished_at: String,
        verdict: String,
        reason: Option<String>,
        record: Option<String>,
        seal: Option<String>,
    ) -> Self {
        Self {
            finished_at,
            verdict,
            reason,
            record,
            seal,
        }
    }

    /// When the run finished.
    #[must_use]
    pub fn finished_at(&self) -> &str {
        &self.finished_at
    }

    /// The verdict the run closed with.
    #[must_use]
    pub fn verdict(&self) -> &str {
        &self.verdict
    }
}

/// One run as listed or shown.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunRow {
    /// The run id. Private; readers use [`run_id`](Self::run_id).
    run_id: String,
    /// The repository under test. Private; readers use [`repo`](Self::repo).
    repo: String,
    /// When the run started. Private; readers use [`started_at`](Self::started_at).
    started_at: String,
    /// How the run closed, if it did.
    pub close: Option<RunClose>,
}

impl RunRow {
    /// One run row exactly as the store holds it.
    #[must_use]
    pub fn new(run_id: String, repo: String, started_at: String, close: Option<RunClose>) -> Self {
        Self {
            run_id,
            repo,
            started_at,
            close,
        }
    }

    /// The identifier the coordinator passed at begin: the row key every lane
    /// and close of this run is folded under, so one run's history is one key.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// The repository under test.
    #[must_use]
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// When the run started.
    #[must_use]
    pub fn started_at(&self) -> &str {
        &self.started_at
    }

    /// When the run finished, if it did.
    #[must_use]
    pub fn finished_at(&self) -> Option<&str> {
        self.close.as_ref().map(|close| close.finished_at())
    }

    /// The verdict, if the run closed.
    #[must_use]
    pub fn verdict(&self) -> Option<&str> {
        self.close.as_ref().map(|close| close.verdict())
    }

    /// Why the run closed the way it did, if it named one.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.close
            .as_ref()
            .and_then(|close| close.reason.as_deref())
    }

    /// The sealed record, if the run sealed one.
    #[must_use]
    pub fn record(&self) -> Option<&str> {
        self.close
            .as_ref()
            .and_then(|close| close.record.as_deref())
    }

    /// The closer's sealed record, when the run closed with one: `None` while
    /// the run is open and when the closer sealed nothing (an abandonment).
    #[must_use]
    pub fn seal(&self) -> Option<&str> {
        self.close.as_ref().and_then(|close| close.seal.as_deref())
    }
}

/// One lane row of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LaneRow {
    /// The lane id from the plan. Private; readers use [`id`](Self::id).
    id: String,
    /// `pending`, `dispatched` or `ended`. Private; readers use [`state`](Self::state).
    state: String,
    /// The outcome, once the lane ended.
    pub outcome: Option<String>,
    /// The detail, once the lane ended.
    pub detail: Option<String>,
    /// The captured tail, once the lane ended.
    pub tail: Option<Vec<u8>>,
}

impl LaneRow {
    /// One lane row exactly as the store holds it.
    #[must_use]
    pub fn new(
        id: String,
        state: String,
        outcome: Option<String>,
        detail: Option<String>,
        tail: Option<Vec<u8>>,
    ) -> Self {
        Self {
            id,
            state,
            outcome,
            detail,
            tail,
        }
    }

    /// The lane id from the plan.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// `pending`, `dispatched` or `ended`.
    #[must_use]
    pub fn state(&self) -> &str {
        &self.state
    }
}

/// A run whose coordinator may have died.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenRun {
    /// The run id. Private; readers use [`run_id`](Self::run_id).
    run_id: String,
    /// The coordinator's process id, for the liveness check.
    pub coordinator_pid: u32,
}

impl OpenRun {
    /// An open run exactly as the store holds it.
    #[must_use]
    pub fn new(run_id: String, coordinator_pid: u32) -> Self {
        Self {
            run_id,
            coordinator_pid,
        }
    }

    /// The run this claim is fenced to: every lane step checks the authority's
    /// epoch for this id before it commits, and a stale one is refused.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
}
