//! The script tree [`parse`](super::parse) answers with.
//!
//! Every word is already read: a bound is a number or the expression the
//! runtime checks, a duration is nanoseconds or an expression of type
//! `Duration`, every step carries its structural label, and every `run` call
//! inside a line of Rust is a [`Call`]. A caller generating code or drawing a
//! map works from these values and never reads a token of the language again,
//! which is what keeps the macro and every tool on one reading (SL-2).
//!
//! Only the parser builds these values, so their fields are private to this
//! crate and read through accessors; the enums are `#[non_exhaustive]`, so the
//! language can grow a word without a caller's exhaustive match compiling into
//! a silent misreading.

use lgwks_deps::proc_macro2::{Delimiter, Ident, Span, TokenStream, TokenTree};

use super::lexicon::Kind;

/// A whole `script!` block: its flows, in source order.
#[derive(Debug, Clone)]
pub struct Script {
    /// Every flow the block declares.
    pub(crate) flows: Vec<Flow>,
}

impl Script {
    /// Every flow the block declares.
    #[must_use]
    pub const fn flows(&self) -> &[Flow] {
        self.flows.as_slice()
    }
}

/// `[pub] flow name(inputs) [-> Output]:` and its body.
#[derive(Debug, Clone)]
pub struct Flow {
    /// The attributes written above the flow, doc comments included, in order.
    pub(crate) attributes: TokenStream,
    /// What stands before `flow`: `pub`, `pub(crate)`, or nothing.
    pub(crate) visibility: TokenStream,
    /// The flow's name.
    pub(crate) name: Ident,
    /// The tokens inside the inputs' `( )`, as written.
    pub(crate) inputs: TokenStream,
    /// The promised output type; `None` when the flow promises `()`.
    pub(crate) output: Option<TokenStream>,
    /// The flow's body.
    pub(crate) body: Block,
    /// The flow's entry in the architecture map.
    pub(crate) shape: FlowShape,
}

impl Flow {
    /// The attributes written above the flow, doc comments included, in order.
    #[must_use]
    pub const fn attributes(&self) -> &TokenStream {
        &self.attributes
    }

    /// What stands before `flow`: `pub`, `pub(crate)`, or nothing.
    #[must_use]
    pub const fn visibility(&self) -> &TokenStream {
        &self.visibility
    }

    /// The identifier after `flow`, which names the generated `async fn`.
    #[must_use]
    pub const fn name(&self) -> &Ident {
        &self.name
    }

    /// The tokens inside the inputs' `( )`, as written.
    #[must_use]
    pub const fn inputs(&self) -> &TokenStream {
        &self.inputs
    }

    /// The promised output type; `None` when the flow promises `()`.
    #[must_use]
    pub const fn output(&self) -> Option<&TokenStream> {
        self.output.as_ref()
    }

    /// The statements under the `flow` header, in source order.
    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }

    /// The flow's entry in the architecture map.
    #[must_use]
    pub const fn shape(&self) -> &FlowShape {
        &self.shape
    }
}

/// The lines of one indented block, already read.
#[derive(Debug, Clone)]
pub struct Block {
    /// Every line, in source order; an `if` chain is one statement.
    pub(crate) statements: Vec<Statement>,
    /// Whether the block's last expression is its value.
    pub(crate) wants_value: bool,
}

impl Block {
    /// Every line, in source order; an `if` chain is one statement.
    #[must_use]
    pub const fn statements(&self) -> &[Statement] {
        self.statements.as_slice()
    }

    /// Whether the block's last expression is its value.
    #[must_use]
    pub const fn wants_value(&self) -> bool {
        self.wants_value
    }
}

/// `each <pattern> in <items>:`, every item concurrently under a bound.
#[derive(Debug, Clone)]
pub struct Each {
    /// The structural label of the fan-out's scope.
    pub(crate) label: String,
    /// The written bound; `None` when the runtime sizes the fan-out.
    pub(crate) bound: Option<Bound>,
    /// The pattern each item binds.
    pub(crate) pattern: TokenStream,
    /// The items.
    pub(crate) items: Code,
    /// The body run for each item.
    pub(crate) body: Block,
}

impl Each {
    /// The structural label of the fan-out's scope.
    #[must_use]
    pub const fn label(&self) -> &str {
        self.label.as_str()
    }

    /// The written bound; `None` when the runtime sizes the fan-out.
    #[must_use]
    pub const fn bound(&self) -> Option<&Bound> {
        self.bound.as_ref()
    }

    /// The pattern each item binds.
    #[must_use]
    pub const fn pattern(&self) -> &TokenStream {
        &self.pattern
    }

    /// The expression after `in`, iterated for the fan-out.
    #[must_use]
    pub const fn items(&self) -> &Code {
        &self.items
    }

    /// The body run for each item.
    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }
}

/// `within <duration>:`, the body under a deadline.
#[derive(Debug, Clone)]
pub struct Within {
    /// The structural label of the deadline's scope.
    pub(crate) label: String,
    /// The deadline.
    pub(crate) limit: Duration,
    /// The body.
    pub(crate) body: Block,
}

impl Within {
    /// The structural label of the deadline's scope.
    #[must_use]
    pub const fn label(&self) -> &str {
        self.label.as_str()
    }

    /// The deadline.
    #[must_use]
    pub const fn limit(&self) -> &Duration {
        &self.limit
    }

    /// The statements that run under the deadline.
    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }
}

/// `retry up to <N> times[, waiting <duration>]:`.
#[derive(Debug, Clone)]
pub struct Retry {
    /// The structural label of the retry's scope.
    pub(crate) label: String,
    /// The attempt budget.
    pub(crate) attempts: Bound,
    /// The pause between attempts; `None` when none was written.
    pub(crate) waiting: Option<Duration>,
    /// The body each attempt runs.
    pub(crate) body: Block,
}

impl Retry {
    /// The structural label of the retry's scope.
    #[must_use]
    pub const fn label(&self) -> &str {
        self.label.as_str()
    }

    /// How many times the block may run, the first try included.
    #[must_use]
    pub const fn attempts(&self) -> &Bound {
        &self.attempts
    }

    /// The pause between attempts; `None` when none was written.
    #[must_use]
    pub const fn waiting(&self) -> Option<&Duration> {
        self.waiting.as_ref()
    }

    /// The body each attempt runs.
    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }
}

/// `step <name>:`, a named scope.
#[derive(Debug, Clone)]
pub struct Step {
    /// The structural label: the name, numbered among same-named siblings.
    pub(crate) label: String,
    /// The body.
    pub(crate) body: Block,
}

impl Step {
    /// The structural label: the name, numbered among same-named siblings.
    #[must_use]
    pub const fn label(&self) -> &str {
        self.label.as_str()
    }

    /// The statements that run inside the named scope.
    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }
}

/// `together:`, every branch concurrently on this task.
#[derive(Debug, Clone)]
pub struct Together {
    /// The branches, in source order.
    pub(crate) branches: Vec<Branch>,
}

impl Together {
    /// The branches, in source order.
    #[must_use]
    pub const fn branches(&self) -> &[Branch] {
        self.branches.as_slice()
    }
}

/// One line under `together:`.
#[derive(Debug, Clone)]
pub struct Branch {
    /// The pattern a `let` branch binds; `None` for a branch that binds nothing.
    pub(crate) pattern: Option<TokenStream>,
    /// What the branch runs.
    pub(crate) value: BranchValue,
}

impl Branch {
    /// The pattern a `let` branch binds; `None` for a branch that binds nothing.
    #[must_use]
    pub const fn pattern(&self) -> Option<&TokenStream> {
        self.pattern.as_ref()
    }

    /// What the branch runs.
    #[must_use]
    pub const fn value(&self) -> &BranchValue {
        &self.value
    }
}

/// `for <pattern> in <items>:`, sequentially, one scope per item.
#[derive(Debug, Clone)]
pub struct For {
    /// The structural label every item's scope is numbered under.
    pub(crate) label: String,
    /// The pattern each item binds.
    pub(crate) pattern: TokenStream,
    /// The items.
    pub(crate) items: Code,
    /// The body run for each item.
    pub(crate) body: Block,
}

impl For {
    /// The structural label every item's scope is numbered under.
    #[must_use]
    pub const fn label(&self) -> &str {
        self.label.as_str()
    }

    /// The pattern each item binds.
    #[must_use]
    pub const fn pattern(&self) -> &TokenStream {
        &self.pattern
    }

    /// The expression after `in`, walked one item at a time.
    #[must_use]
    pub const fn items(&self) -> &Code {
        &self.items
    }

    /// The body run for each item.
    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }
}

/// An `if` chain.
#[derive(Debug, Clone)]
pub struct IfChain {
    /// `if`, then each `else if`, then the `else` if one ends the chain.
    pub(crate) branches: Vec<IfBranch>,
    /// Whether the chain is its block's value: it is the last line of a block that wants one, and an `else:` ends it.
    pub(crate) wants_value: bool,
    /// Whether every branch leaves the block, which makes the chain a way out rather than a value.
    pub(crate) leaves: bool,
}

impl IfChain {
    /// `if`, then each `else if`, then the `else` if one ends the chain.
    #[must_use]
    pub const fn branches(&self) -> &[IfBranch] {
        self.branches.as_slice()
    }

    /// Whether the chain is its block's value: it is the last line of a block that wants one, and an `else:` ends it.
    #[must_use]
    pub const fn wants_value(&self) -> bool {
        self.wants_value
    }

    /// Whether every branch leaves the block, which makes the chain a way out rather than a value.
    #[must_use]
    pub const fn leaves(&self) -> bool {
        self.leaves
    }
}

/// One branch of an `if` chain.
#[derive(Debug, Clone)]
pub struct IfBranch {
    /// The condition; `None` for the final `else:`.
    pub(crate) condition: Option<Code>,
    /// The branch's body.
    pub(crate) body: Block,
}

impl IfBranch {
    /// The condition; `None` for the final `else:`.
    #[must_use]
    pub const fn condition(&self) -> Option<&Code> {
        self.condition.as_ref()
    }

    /// The statements this branch runs when it is taken.
    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }
}

/// A line of Rust with its `run` calls read out of it.
#[derive(Debug, Clone, Default)]
pub struct Code {
    /// The line's tokens, in order, with every `run` call one fragment.
    pub(crate) fragments: Vec<Fragment>,
}

impl Code {
    /// The line's tokens, in order, with every `run` call one fragment.
    #[must_use]
    pub const fn fragments(&self) -> &[Fragment] {
        self.fragments.as_slice()
    }
}

/// `run path(args)`: another flow called in this scope.
#[derive(Debug, Clone)]
pub struct Call {
    /// The callee's path as written, `::` and all.
    pub(crate) path: TokenStream,
    /// The callee's last segment, which names its scope.
    pub(crate) callee: String,
    /// The structural label: the callee, numbered among same-named calls in one scope. A call whose label is not the callee's name enters a scope of that label, so two calls never share a key.
    pub(crate) label: String,
    /// The arguments.
    pub(crate) arguments: Code,
}

impl Call {
    /// The callee's path as written, `::` and all.
    #[must_use]
    pub const fn path(&self) -> &TokenStream {
        &self.path
    }

    /// The callee's last segment, which names its scope.
    #[must_use]
    pub const fn callee(&self) -> &str {
        self.callee.as_str()
    }

    /// The structural label: the callee, numbered among same-named calls in one scope. A call whose label is not the callee's name enters a scope of that label, so two calls never share a key.
    #[must_use]
    pub const fn label(&self) -> &str {
        self.label.as_str()
    }

    /// What is passed to the callee, between its parentheses.
    #[must_use]
    pub const fn arguments(&self) -> &Code {
        &self.arguments
    }
}

/// A flow's entry in the architecture map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowShape {
    /// The flow's name.
    pub(crate) name: String,
    /// `name(inputs) -> Output`, as written.
    pub(crate) signature: String,
    /// The 1-based source line of the `flow` header, as the compiler reports it.
    pub(crate) line: usize,
    /// The steps inside, in source order.
    pub(crate) steps: Vec<StepShape>,
}

impl FlowShape {
    /// The identifier after `flow`, as the architecture map lists it.
    #[must_use]
    pub const fn name(&self) -> &str {
        self.name.as_str()
    }

    /// `name(inputs) -> Output`, as written.
    #[must_use]
    pub const fn signature(&self) -> &str {
        self.signature.as_str()
    }

    /// The source line of the `flow` header.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// The steps inside, in source order.
    #[must_use]
    pub const fn steps(&self) -> &[StepShape] {
        self.steps.as_slice()
    }
}

/// One step in the architecture map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepShape {
    /// The word the step is: never `flow`, whose entry is a [`FlowShape`], and
    /// never `let`, whose entry is the block it binds.
    pub(crate) kind: Kind,
    /// The step's subject: the bound, the deadline, the attempt count, the step's name, the callee, or the branch count.
    pub(crate) subject: String,
    /// The line as written.
    pub(crate) detail: String,
    /// The source line.
    pub(crate) line: usize,
    /// The steps inside, in source order.
    pub(crate) children: Vec<Self>,
}

impl StepShape {
    /// The word the step is: never `flow`, whose entry is a [`FlowShape`], and
    /// never `let`, whose entry is the block it binds.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// The step's subject: the bound, the deadline, the attempt count, the step's name, the callee, or the branch count.
    #[must_use]
    pub const fn subject(&self) -> &str {
        self.subject.as_str()
    }

    /// The line as written.
    #[must_use]
    pub const fn detail(&self) -> &str {
        self.detail.as_str()
    }

    /// The 1-based source line the step starts on, as the compiler reports it.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// The steps inside, in source order.
    #[must_use]
    pub const fn children(&self) -> &[Self] {
        self.children.as_slice()
    }
}

impl Block {
    /// Whether the last line always leaves the block, so no value follows it.
    #[must_use]
    pub fn diverges(&self) -> bool {
        self.statements.last().is_some_and(Statement::diverges)
    }

    /// The statement that is the block's value: the last one, when the block
    /// wants a value and that statement is an expression.
    #[must_use]
    pub fn tail(&self) -> Option<&Statement> {
        self.statements
            .last()
            .filter(|last| self.wants_value && last.is_expression())
    }
}

impl Code {
    /// Whether the code has no tokens at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }
}

/// One line of a block, or one `if` chain.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Statement {
    /// A line of Rust passed through as written, with its `run` calls read.
    Rust(Code),
    /// A `let` line that opens no block: Rust, ending its statement.
    Let(Code),
    /// A line that is exactly one `run` call, whose result is the line's.
    Run(Call),
    /// `let <pattern> = <block word>:`: the block's value, bound.
    Bind {
        /// The pattern before the `=`.
        pattern: TokenStream,
        /// The block whose value is bound.
        block: Construct,
    },
    /// A block word whose result is the line's.
    Construct(Construct),
    /// `together:` and its branches.
    Together(Together),
    /// `for <pattern> in <items>:`.
    For(For),
    /// `if cond:` with its `else if cond:` and `else:` branches.
    If(IfChain),
    /// `give back <value>`: leaves the flow with the value.
    GiveBack(Code),
    /// `fail [transiently] with <reason>`: leaves the flow with a typed error.
    Fail {
        /// Whether the failure is worth repeating (`fail transiently`).
        transient: bool,
        /// The reason, as written.
        reason: Code,
    },
}

impl Statement {
    /// Whether the statement is an expression that could be a block's value.
    #[must_use]
    pub const fn is_expression(&self) -> bool {
        match *self {
            Self::Rust(_) | Self::Run(_) | Self::Construct(_) => true,
            Self::If(ref chain) => !chain.leaves && chain.wants_value,
            Self::Let(_)
            | Self::Bind { .. }
            | Self::Together(_)
            | Self::For(_)
            | Self::GiveBack(_)
            | Self::Fail { .. } => false,
        }
    }

    /// Whether the statement always leaves the block it is in.
    #[must_use]
    pub const fn diverges(&self) -> bool {
        match *self {
            Self::GiveBack(_) | Self::Fail { .. } => true,
            Self::If(ref chain) => chain.leaves,
            Self::Rust(_)
            | Self::Let(_)
            | Self::Run(_)
            | Self::Bind { .. }
            | Self::Construct(_)
            | Self::Together(_)
            | Self::For(_) => false,
        }
    }

    /// Whether the statement's value is a flow result that needs `?` to
    /// become the value: a block word or a `run`.
    #[must_use]
    pub const fn is_fallible(&self) -> bool {
        matches!(*self, Self::Run(_) | Self::Construct(_))
    }
}

/// A block word with its own async body: `each`, `within`, `retry`, `step`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Construct {
    /// `each <pattern> in <items>[, at most (limit) at once]:`.
    Each(Each),
    /// `within <duration>:`.
    Within(Within),
    /// `retry up to <N> times[, waiting <duration>]:`.
    Retry(Retry),
    /// `step <name>:`.
    Step(Step),
}

/// What a `together:` branch runs.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum BranchValue {
    /// A block word.
    Construct(Construct),
    /// Exactly one `run` call.
    Run(Call),
    /// A line of Rust whose value is the branch's.
    Rust(Code),
}

/// A count a word takes: `up to <N>`, `at most (<limit>)`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Bound {
    /// A literal, already checked against the word's range.
    Count(u64),
    /// An expression the runtime checks, ready to splice: the inside of a
    /// written `( )`, or the written tokens in `( )` of their own.
    Expr(TokenStream),
}

/// A duration a word takes: `within <D>`, `waiting <D>`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Duration {
    /// A literal such as `250ms` or `1.5s`, in nanoseconds; never zero.
    Nanos(u64),
    /// The expression inside a written `( )`, of type `Duration`.
    Expr(TokenStream),
}

/// One piece of a line of Rust.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Fragment {
    /// A token passed through as written.
    Token(TokenTree),
    /// A bracketed group, whose inside is read the same way.
    Group {
        /// The bracket.
        delimiter: Delimiter,
        /// The group's span, which a rebuilt group keeps.
        span: Span,
        /// What is inside.
        inner: Code,
    },
    /// A `run` call.
    Run(Call),
}
