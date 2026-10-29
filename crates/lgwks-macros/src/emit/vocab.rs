//! The structured words: `graph` and its nodes, `saga` and its `compensate`
//! lines, `whenever`, `justify`, `remember`, `admit` and `ready`, each one
//! call into `lgwks_bot::script`.

use lgwks_ast::script::{
    Admit, Compensate, Graph, Justify, Overflow, Producer, Ready, Step, Whenever,
};
use lgwks_deps::proc_macro2::TokenStream;
use lgwks_deps::quote::quote;

use super::{body, bound, code, duration, newer_word, runtime, site_of};
use crate::error::Result;

/// The registrar a saga's body holds, which each `compensate` line in it
/// registers on. One name, so a `compensate` reaches only its own saga's.
fn registrar() -> TokenStream {
    quote!(lgwks_script_compensations)
}

/// `graph[, at most (limit) at once]:` and its `node`s.
///
/// Each node's body runs in its own scope, `<graph>/<node>`, asks the scope's
/// justification first, and gives back the node's value; every node's value
/// is one type. The bodies are `async move` so a node holds copies of the
/// references it reads, never a borrow of the closure that built it.
pub(super) fn graph(declared: &Graph) -> Result<TokenStream> {
    let script = runtime();
    let limit = match declared.bound() {
        None => quote!(::core::option::Option::None),
        Some(written) => {
            let limit = bound(written)?;
            quote!(::core::option::Option::Some(#script::at_most(#limit)?))
        }
    };
    let mut nodes = Vec::with_capacity(declared.nodes().len());
    for node in declared.nodes() {
        let name = node.name();
        let needs = node.needs();
        let inner = body(node.body())?.into_ok_block();
        nodes.push(quote! {
            #script::GraphNode::new(#name, &[#(#needs),*], |scope: #script::Scope| {
                ::std::boxed::Box::pin(async move {
                    let scope = &scope;
                    scope.justified(0).await?;
                    #inner
                })
            })
        });
    }
    let label = declared.label();
    let site = site_of(declared.site());
    Ok(quote! {
        #script::graph_at(scope, #label, #site, #limit, ::std::vec![#(#nodes),*]).await
    })
}

/// `saga <name>:`: the body runs with a registrar its `compensate` lines
/// register on, undone in reverse when a later line fails.
pub(super) fn saga(named: &Step) -> Result<TokenStream> {
    let script = runtime();
    let registrar = registrar();
    let label = named.label();
    let site = site_of(named.site());
    let inner = body(named.body())?.into_ok_block();
    Ok(quote! {
        #script::compensated_at(scope, #label, #site, async |scope: #script::Scope, #registrar| {
            let scope = &scope;
            let _ = &#registrar;
            #inner
        }).await
    })
}

/// `compensate <step>:`: registers the undo of `step`, which has just
/// finished, to run in `undo:<step>` only if the saga later fails.
pub(super) fn compensate(undo: &Compensate) -> Result<TokenStream> {
    let script = runtime();
    let registrar = registrar();
    let step = undo.undoes();
    let inner = body(undo.undo())?.into_ok_block();
    Ok(quote! {
        #registrar.add(#step, |scope: #script::Scope| async move {
            let scope = &scope;
            (#inner).map(|_undone| ())
        })?;
    })
}

/// `whenever <pattern> in <batches>, buffering <N>, <overflow>:`: the batch
/// expression is awaited for each next batch, and each event is handled in
/// its own scope.
pub(super) fn whenever(feed: &Whenever) -> Result<TokenStream> {
    let script = runtime();
    let overflow = match feed.overflow() {
        Overflow::DropOldest => quote!(DropOldest),
        Overflow::DropNewest => quote!(DropNewest),
        Overflow::Refuse => quote!(Refuse),
        _ => {
            let refusal = newer_word("overflow policy");
            lgwks_std::trace::debug!(error = %refusal, "whenever: returning an error to the caller");
            return Err(refusal);
        }
    };
    let cap = bound(feed.cap())?;
    let batches = code(feed.batches())?;
    let pattern = feed.pattern();
    let label = feed.label();
    let site = site_of(feed.site());
    let inner = body(feed.body())?.into_ok_block();
    Ok(quote! {
        #script::stream_at(
            scope,
            #label,
            #site,
            #cap,
            #script::Overflow::#overflow,
            async || #batches,
            async |scope: #script::Scope, #pattern| {
                let scope = &scope;
                (#inner).map(|_handled| ())
            },
        ).await
    })
}

/// `justify with (<rules>):`: the block in a scope of its own that answers
/// to `rules`, asked first at the block's entry and again by every step,
/// node, retry attempt, `observe` and `act` inside.
pub(super) fn justify(block: &Justify) -> Result<TokenStream> {
    let script = runtime();
    let rules = block.rules();
    let entered = quote!(.with_justification(#script::Justify::clone(&(#rules))));
    let inner = body(block.governed())?.into_ok_block();
    Ok(scoped(block.label(), block.site(), &entered, &inner))
}

/// A block word's own scope, entered at `site` and shaped by `entered`: the
/// scope's justification asked first, then `inner`, the outcome settled and
/// located at the scope.
fn scoped(
    label: &str,
    site: &lgwks_ast::script::Site,
    entered: &TokenStream,
    inner: &TokenStream,
) -> TokenStream {
    let script = runtime();
    let site = site_of(site);
    quote! {
        {
            let scope = &scope.enter_at(#label, #site)? #entered;
            let outcome: ::core::result::Result<_, #script::FlowError> = async {
                scope.justified(0).await?;
                #inner
            }.await;
            scope.settle(&outcome);
            outcome.map_err(|error| error.located(scope))
        }
    }
}

/// `remember <name>:`: the block run in the step's own scope once per run,
/// its value recorded through the host's run store.
pub(super) fn remember(named: &Step) -> Result<TokenStream> {
    let script = runtime();
    let label = named.label();
    let site = site_of(named.site());
    let inner = body(named.body())?.into_ok_block();
    Ok(quote! {
        #script::remembered_at(scope, #label, #site, async |scope: #script::Scope| {
            let scope = &scope;
            scope.justified(0).await?;
            #inner
        }).await
    })
}

/// `admit from <producer> through (<gate>):`: the block's value is the
/// payload, admitted as a `Plan` in a step of its own.
pub(super) fn admit(admitted: &Admit) -> Result<TokenStream> {
    let script = runtime();
    let producer = match admitted.producer() {
        Producer::Model => quote!(Model),
        Producer::Tool => quote!(ToolOutput),
        Producer::Document => quote!(Document),
        _ => {
            let refusal = newer_word("producer");
            lgwks_std::trace::debug!(error = %refusal, "admit: returning an error to the caller");
            return Err(refusal);
        }
    };
    let gate = admitted.gate();
    let label = admitted.label();
    let site = site_of(admitted.site());
    let inner = body(admitted.payload())?.into_ok_block();
    Ok(quote! {
        {
            let lgwks_script_payload = async #inner.await?;
            #script::admit_at(
                scope,
                #label,
                #site,
                &(#gate),
                ::core::convert::AsRef::<[u8]>::as_ref(&lgwks_script_payload),
                ::lgwks_bot::proposal::Source::#producer,
            ).await
        }
    })
}

/// `ready <pattern> from (<readiness>) within <duration>:`: the wait in a
/// scope of its own, the released value bound, then the block.
pub(super) fn ready(waited: &Ready) -> Result<TokenStream> {
    let pattern = waited.pattern();
    let readiness = waited.readiness();
    let limit = duration(waited.limit())?;
    let block = body(waited.body())?.into_ok_block();
    let inner = quote! {
        let #pattern = (#readiness).wait(scope, #limit).await?;
        #block
    };
    Ok(scoped(
        waited.label(),
        waited.site(),
        &TokenStream::new(),
        &inner,
    ))
}
