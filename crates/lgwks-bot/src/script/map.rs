//! The architecture map every `script!` emits.

use std::fmt;

use lgwks_std::json::Serialize;

// ── Architecture ────────────────────────────────────────────────────────────

/// The orchestration a `script!` block declares, compiled from its tokens.
///
/// Every `script!` emits one as `ARCHITECTURE`. It is the map an agent or a
/// reviewer reads instead of re-deriving the flow graph from source, and since
/// it is built from the same tokens as the code it cannot describe a flow the
/// code does not run. [`Display`](fmt::Display) renders the tree;
/// [`Architecture::to_json`] is the machine form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(crate = "lgwks_std::json::serde")]
pub struct Architecture {
    /// The flows, in declaration order.
    flows: &'static [FlowShape],
}

impl Architecture {
    #[doc(hidden)]
    /// Assemble a map. Called by `script!`.
    #[must_use]
    pub const fn new(flows: &'static [FlowShape]) -> Self {
        Self { flows }
    }

    /// The flows, in declaration order.
    #[must_use]
    pub const fn flows(&self) -> &'static [FlowShape] {
        self.flows
    }

    /// The flow named `name`.
    #[must_use]
    pub fn flow(&self, name: &str) -> Option<&'static FlowShape> {
        self.flows.iter().find(|flow| flow.name == name)
    }

    /// The map as JSON.
    ///
    /// # Errors
    ///
    /// The encoder's error; the map holds only strings and numbers, so this
    /// does not fail in practice.
    pub fn to_json(&self) -> Result<String, lgwks_std::json::Error> {
        lgwks_std::json::to_string(self)
    }
}

impl fmt::Display for Architecture {
    /// One line per flow and per block, indented by nesting, each ending with
    /// the source line it was declared on: `lgwks_ast::script::write_map`, the
    /// one rendering the script tool also prints a parsed script through.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        lgwks_ast::script::write_map(
            formatter,
            self.flows
                .iter()
                .map(|flow| (flow.signature, flow.line, flow.steps)),
        )
    }
}

impl lgwks_ast::script::MapStep for StepShape {
    fn detail(&self) -> &str {
        self.detail
    }

    fn line(&self) -> impl fmt::Display {
        self.line
    }

    fn children(&self) -> &[Self] {
        self.steps
    }
}

/// One flow in an [`Architecture`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(crate = "lgwks_std::json::serde")]
pub struct FlowShape {
    /// The flow's name.
    name: &'static str,
    /// `name(params) -> output`, as written.
    signature: &'static str,
    /// The source line of the `flow` header.
    line: u32,
    /// The blocks directly inside the flow.
    steps: &'static [StepShape],
}

impl FlowShape {
    #[doc(hidden)]
    /// Describe a flow. Called by `script!`.
    #[must_use]
    pub const fn new(
        name: &'static str,
        signature: &'static str,
        line: u32,
        steps: &'static [StepShape],
    ) -> Self {
        Self {
            name,
            signature,
            line,
            steps,
        }
    }

    /// The name the document declared for this flow.
    ///
    /// A `&'static str` because it comes from the script, which lives as long
    /// as the process, so no caller has to hold the script to read it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// `name(params) -> output`, as written.
    #[must_use]
    pub const fn signature(&self) -> &'static str {
        self.signature
    }

    /// The source line of the `flow` header.
    #[must_use]
    pub const fn line(&self) -> u32 {
        self.line
    }

    /// The blocks directly inside the flow.
    #[must_use]
    pub const fn steps(&self) -> &'static [StepShape] {
        self.steps
    }

    /// Every flow this one runs, anywhere inside it, in source order.
    #[must_use]
    pub fn runs(&self) -> Vec<&'static str> {
        runs_in(self.steps)
    }
}

/// The callee of every `run` in `steps`, depth first.
fn runs_in(steps: &'static [StepShape]) -> Vec<&'static str> {
    steps
        .iter()
        .flat_map(|step| {
            let own = (step.kind == StepKind::Run).then_some(step.subject);
            own.into_iter().chain(runs_in(step.steps))
        })
        .collect()
}

/// One block inside a flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(crate = "lgwks_std::json::serde")]
pub struct StepShape {
    /// Which block.
    kind: StepKind,
    /// The block's subject: the step name, the flow a `run` calls, the bound.
    subject: &'static str,
    /// The header as written, for rendering.
    detail: &'static str,
    /// The source line of the header.
    line: u32,
    /// Blocks nested inside.
    steps: &'static [StepShape],
}

impl StepShape {
    #[doc(hidden)]
    /// Describe a block. Called by `script!`.
    #[must_use]
    pub const fn new(
        kind: StepKind,
        subject: &'static str,
        detail: &'static str,
        line: u32,
        steps: &'static [StepShape],
    ) -> Self {
        Self {
            kind,
            subject,
            detail,
            line,
            steps,
        }
    }

    /// Which block.
    #[must_use]
    pub const fn kind(&self) -> StepKind {
        self.kind
    }

    /// The step name, the callee, or the bound, by kind.
    #[must_use]
    pub const fn subject(&self) -> &'static str {
        self.subject
    }

    /// The header as written.
    #[must_use]
    pub const fn detail(&self) -> &'static str {
        self.detail
    }

    /// The source line of the header.
    #[must_use]
    pub const fn line(&self) -> u32 {
        self.line
    }

    /// Blocks nested inside.
    #[must_use]
    pub const fn steps(&self) -> &'static [StepShape] {
        self.steps
    }
}

/// The kinds of block a flow is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(crate = "lgwks_std::json::serde")]
#[non_exhaustive]
pub enum StepKind {
    /// `each x in xs, at most N at once:`; the subject is `N`.
    Each,
    /// `within D:`; the subject is `D`.
    Within,
    /// `retry up to N times[, waiting D]:`; the subject is `N`.
    Retry,
    /// `together:`; the subject is the branch count.
    Together,
    /// `step name:`; the subject is the name.
    Step,
    /// `for x in xs:`, sequential, one scope per item; the subject is its label.
    For,
    /// `run flow(..)`; the subject is the flow.
    Run,
    /// `if cond:`.
    If,
    /// `else:` or `else if cond:`.
    Else,
    /// `fail with ..` or `fail transiently with ..`.
    Fail,
    /// `give back ..`.
    GiveBack,
}
