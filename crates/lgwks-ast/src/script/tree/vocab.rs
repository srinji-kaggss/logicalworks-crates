//! The structured words' nodes: `graph` and its `node`s, `compensate` inside
//! a `saga`, `whenever`, `justify`, `admit` and `ready`.
//!
//! A `saga` is a [`Step`](super::Step): a name and a body, read as one; what
//! makes it a saga is the [`Construct::Saga`](super::Construct::Saga) it is
//! wrapped in and the `compensate` lines its body may hold. A `remember` is a
//! `Step` the same way, wrapped in [`Construct::Remember`](super::Construct::Remember).

use lgwks_deps::proc_macro2::TokenStream;

use super::{Block, Bound, Code, Duration, Site};

/// `graph[, at most (limit) at once]:` and the `node`s under it.
#[derive(Debug, Clone)]
pub struct Graph {
    /// Where it stands in the script; read through `site()`.
    pub(crate) site: Site,
    /// The structural label of the graph's scope.
    pub(crate) label: String,
    /// The written bound on nodes running at once; `None` when the runtime
    /// sizes it to the machine.
    pub(crate) bound: Option<Bound>,
    /// The nodes, in the order they are written.
    pub(crate) nodes: Vec<Node>,
}

impl Graph {
    /// The structural label of the graph's scope.
    #[must_use]
    pub const fn label(&self) -> &str {
        self.label.as_str()
    }

    /// The written bound on nodes running at once; `None` when the runtime
    /// sizes it to the machine.
    #[must_use]
    pub const fn bound(&self) -> Option<&Bound> {
        self.bound.as_ref()
    }

    /// The nodes, in the order they are written.
    #[must_use]
    pub const fn nodes(&self) -> &[Node] {
        self.nodes.as_slice()
    }
}

/// `node <name>[ needs <a>, <b>]:` inside a `graph`.
#[derive(Debug, Clone)]
pub struct Node {
    /// Where it stands in the script; read through `site()`.
    pub(crate) site: Site,
    /// The node's name, unique in its graph; also its scope's label.
    pub(crate) name: String,
    /// The nodes that must succeed before this one starts, as written.
    pub(crate) needs: Vec<String>,
    /// The body, run once.
    pub(crate) body: Block,
}

impl Node {
    /// The node's name, unique in its graph; also its scope's label.
    #[must_use]
    pub const fn name(&self) -> &str {
        self.name.as_str()
    }

    /// The nodes that must succeed before this one starts, as written.
    #[must_use]
    pub const fn needs(&self) -> &[String] {
        self.needs.as_slice()
    }

    /// The body, run once.
    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }
}

/// `compensate <step>:` inside a `saga`: the undo of a finished step.
#[derive(Debug, Clone)]
pub struct Compensate {
    /// Where it stands in the script; read through `site()`.
    pub(crate) site: Site,
    /// The step the undo reverses: a `step` written earlier in the saga.
    pub(crate) undoes: String,
    /// The undo itself.
    pub(crate) undo: Block,
}

impl Compensate {
    /// The step the undo reverses: a `step` written earlier in the saga.
    #[must_use]
    pub const fn undoes(&self) -> &str {
        self.undoes.as_str()
    }

    /// The block that reverses the step, run in `undo:<step>` only when a
    /// later line of the saga fails.
    #[must_use]
    pub const fn undo(&self) -> &Block {
        &self.undo
    }
}

/// What `whenever` does with an event past its buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Overflow {
    /// `drop oldest`: keep the newest events.
    DropOldest,
    /// `drop newest`: keep the events that came first.
    DropNewest,
    /// `refuse`: hand the event back, handling the oldest to make room.
    Refuse,
}

/// `whenever <pattern> in <batches>, buffering <N>, <overflow>:`.
#[derive(Debug, Clone)]
pub struct Whenever {
    /// Where it stands in the script; read through `site()`.
    pub(crate) site: Site,
    /// The structural label of the stream's scope.
    pub(crate) label: String,
    /// The pattern each event binds.
    pub(crate) pattern: TokenStream,
    /// The expression awaited for the next batch; `None` ends the stream.
    pub(crate) batches: Code,
    /// The most events buffered unhandled.
    pub(crate) cap: Bound,
    /// What happens to an event past `cap`.
    pub(crate) overflow: Overflow,
    /// The body run for each event.
    pub(crate) body: Block,
}

impl Whenever {
    /// The structural label of the stream's scope.
    #[must_use]
    pub const fn label(&self) -> &str {
        self.label.as_str()
    }

    /// The pattern each event binds.
    #[must_use]
    pub const fn pattern(&self) -> &TokenStream {
        &self.pattern
    }

    /// The expression awaited for the next batch: `Some(events)`, or `None`
    /// when the feed has ended.
    #[must_use]
    pub const fn batches(&self) -> &Code {
        &self.batches
    }

    /// The most events buffered unhandled.
    #[must_use]
    pub const fn cap(&self) -> &Bound {
        &self.cap
    }

    /// What happens to an event past the buffer.
    #[must_use]
    pub const fn overflow(&self) -> Overflow {
        self.overflow
    }

    /// The body run for each event.
    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }
}

/// `justify with (<rules>):`: a block whose every step answers to `rules`.
#[derive(Debug, Clone)]
pub struct Justify {
    /// Where it stands in the script; read through `site()`.
    pub(crate) site: Site,
    /// The structural label of the governed scope.
    pub(crate) label: String,
    /// The expression inside the written `( )`, of type `Justify`.
    pub(crate) rules: TokenStream,
    /// The block the rules govern.
    pub(crate) governed: Block,
}

impl Justify {
    /// The structural label of the governed scope.
    #[must_use]
    pub const fn label(&self) -> &str {
        self.label.as_str()
    }

    /// The expression inside the written `( )`, of type `Justify`.
    #[must_use]
    pub const fn rules(&self) -> &TokenStream {
        &self.rules
    }

    /// The block the rules govern.
    #[must_use]
    pub const fn governed(&self) -> &Block {
        &self.governed
    }
}

/// Who produced the bytes an `admit` reads: the particle after `from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Producer {
    /// `from model`: a model's output.
    Model,
    /// `from tool`: a tool's output.
    Tool,
    /// `from document`: a document's text.
    Document,
}

/// `admit from <producer> through (<gate>):`: the block's value is the
/// payload, admitted as a plan.
#[derive(Debug, Clone)]
pub struct Admit {
    /// Where it stands in the script; read through `site()`.
    pub(crate) site: Site,
    /// The structural label of the admission's step.
    pub(crate) label: String,
    /// Who produced the payload.
    pub(crate) producer: Producer,
    /// The expression inside the written `( )`, of type `Gate`.
    pub(crate) gate: TokenStream,
    /// The block whose value is the payload's bytes.
    pub(crate) payload: Block,
}

impl Admit {
    /// The structural label of the admission's step.
    #[must_use]
    pub const fn label(&self) -> &str {
        self.label.as_str()
    }

    /// Who produced the payload.
    #[must_use]
    pub const fn producer(&self) -> Producer {
        self.producer
    }

    /// The expression inside the written `( )`, of type `Gate`.
    #[must_use]
    pub const fn gate(&self) -> &TokenStream {
        &self.gate
    }

    /// The block whose value is the payload's bytes.
    #[must_use]
    pub const fn payload(&self) -> &Block {
        &self.payload
    }
}

/// `ready <pattern> from (<readiness>) within <duration>:`.
#[derive(Debug, Clone)]
pub struct Ready {
    /// Where it stands in the script; read through `site()`.
    pub(crate) site: Site,
    /// The structural label of the wait's scope.
    pub(crate) label: String,
    /// The pattern the released value binds.
    pub(crate) pattern: TokenStream,
    /// The expression inside the written `( )`, of type `Readiness`.
    pub(crate) readiness: TokenStream,
    /// The longest the wait may take.
    pub(crate) limit: Duration,
    /// The body run once the readiness releases.
    pub(crate) body: Block,
}

impl Ready {
    /// The structural label of the wait's scope.
    #[must_use]
    pub const fn label(&self) -> &str {
        self.label.as_str()
    }

    /// The pattern the released value binds.
    #[must_use]
    pub const fn pattern(&self) -> &TokenStream {
        &self.pattern
    }

    /// The expression inside the written `( )`, of type `Readiness`.
    #[must_use]
    pub const fn readiness(&self) -> &TokenStream {
        &self.readiness
    }

    /// The longest the wait may take.
    #[must_use]
    pub const fn limit(&self) -> &Duration {
        &self.limit
    }

    /// The body run once the readiness releases.
    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }
}
