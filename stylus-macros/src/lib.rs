//! Proc-macro implementation of `#[traceable]`.
//!
//! Uses an `#[in_span]`-style argument surface (`name`, `tracer`,
//! `fields(key = expr, ...)`), but additionally gates span creation behind a
//! per-function bitmask discovered at link time via `linkme` (one bit per
//! `stylus::instrumentation` slot), so tracing can be toggled per function --
//! and per instrumentation -- at runtime without recompiling.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, quote};
use syn::{
    Expr, Ident, ItemFn, LitStr, Token,
    parse::{Parse, ParseStream},
    parse_macro_input,
    punctuated::Punctuated,
};

enum FieldKey {
    Lit(LitStr),
    Path(syn::Path),
}

impl ToTokens for FieldKey {
    fn to_tokens(&self, tokens: &mut TokenStream2) {
        match self {
            FieldKey::Lit(lit) => lit.to_tokens(tokens),
            FieldKey::Path(path) => path.to_tokens(tokens),
        }
    }
}

struct Field {
    key: FieldKey,
    value: Expr,
}

impl Parse for Field {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key = if input.peek(LitStr) {
            FieldKey::Lit(input.parse()?)
        } else {
            FieldKey::Path(input.parse()?)
        };
        input.parse::<Token![=]>()?;
        let value: Expr = input.parse()?;
        Ok(Field { key, value })
    }
}

#[derive(Default)]
struct TraceableArgs {
    name: Option<LitStr>,
    tracer: Option<LitStr>,
    fields: Option<Vec<Field>>,
}

impl Parse for TraceableArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = TraceableArgs::default();
        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            match ident.to_string().as_str() {
                "name" => {
                    input.parse::<Token![=]>()?;
                    let lit: LitStr = input.parse()?;
                    if args.name.replace(lit).is_some() {
                        return Err(syn::Error::new(ident.span(), "duplicate `name` argument"));
                    }
                }
                "tracer" => {
                    input.parse::<Token![=]>()?;
                    let lit: LitStr = input.parse()?;
                    if args.tracer.replace(lit).is_some() {
                        return Err(syn::Error::new(ident.span(), "duplicate `tracer` argument"));
                    }
                }
                "fields" => {
                    let content;
                    syn::parenthesized!(content in input);
                    let parsed: Punctuated<Field, Token![,]> =
                        content.parse_terminated(Field::parse, Token![,])?;
                    if args.fields.replace(parsed.into_iter().collect()).is_some() {
                        return Err(syn::Error::new(ident.span(), "duplicate `fields` argument"));
                    }
                }
                other => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown argument `{other}`"),
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(args)
    }
}

/// Marks a `fn` or `async fn` as a candidate for tracing.
///
/// Every call checks a per-function, link-time-registered flag before doing
/// any span/context work, so disabled functions cost a single atomic load.
/// Enable/disable a function by its registry key at runtime via
/// `stylus::config` (default key: `module_path!() + "::" + fn_name`, or the
/// `name` argument if given — note this key is not qualified by the
/// surrounding `impl` type, so two methods with the same name in the same
/// module share a key unless `name` disambiguates them).
///
/// ```ignore
/// #[traceable]
/// fn process() { }
///
/// #[traceable(name = "kafka.fetch", tracer = "my-service")]
/// async fn fetch() { }
///
/// #[traceable(fields("component" = "proxy", request_id = id))]
/// async fn handle(id: String) { }
/// ```
///
/// Whether a function is allowed to root a trace or is *child-only* (only ever
/// creates a span when called from within an already-active span, never as a
/// root) is not a source annotation — it's decided at runtime via
/// `stylus::config`, since whether a shared function should root a trace
/// depends on what's being traced.
#[proc_macro_attribute]
pub fn traceable(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as TraceableArgs);
    let func = parse_macro_input!(item as ItemFn);
    expand(args, func).into()
}

fn expand(args: TraceableArgs, func: ItemFn) -> TokenStream2 {
    let ItemFn {
        attrs,
        vis,
        sig,
        block,
    } = func;
    let fn_ident_str = sig.ident.to_string();
    let is_async = sig.asyncness.is_some();

    let span_name = match &args.name {
        Some(lit) => quote! { #lit },
        None => quote! { #fn_ident_str },
    };

    let registry_key = match &args.name {
        Some(lit) => quote! { #lit },
        None => quote! { ::std::concat!(::std::module_path!(), "::", #fn_ident_str) },
    };

    let tracer_name = match &args.tracer {
        Some(lit) => quote! { #lit },
        None => quote! { ::std::env!("CARGO_PKG_NAME") },
    };

    // The `fields(...)` as `KeyValue` expressions, reused by both the
    // default-only fast branch and the multi-instrumentation branch.
    let kvs: Vec<TokenStream2> = match &args.fields {
        None => Vec::new(),
        Some(fields) => fields
            .iter()
            .map(|f| {
                let key = &f.key;
                let value = &f.value;
                quote! { ::opentelemetry::KeyValue::new(#key, #value) }
            })
            .collect(),
    };

    let span_init = if kvs.is_empty() {
        quote! { ::opentelemetry::trace::Tracer::start(&__stylus_tracer, #span_name) }
    } else {
        quote! {
            ::opentelemetry::trace::SpanBuilder::from_name(#span_name)
                .with_attributes([#(#kvs),*])
                .start(&__stylus_tracer)
        }
    };

    // The default-only (`mask == 1`) branch is byte-for-byte what stylus has
    // always generated: one span from the process-global tracer, nested under
    // the single ambient context. No instrumentation-slot machinery, so
    // existing callers pay exactly what they always have.
    let default_enabled_branch = if is_async {
        quote! {
            let __stylus_tracer = ::opentelemetry::global::tracer(#tracer_name);
            let __stylus_span = #span_init;
            let __stylus_cx = <::opentelemetry::Context as ::opentelemetry::trace::TraceContextExt>::current_with_span(__stylus_span);
            ::opentelemetry::trace::FutureExt::with_context(async #block, __stylus_cx).await
        }
    } else {
        quote! {
            let __stylus_tracer = ::opentelemetry::global::tracer(#tracer_name);
            let __stylus_span = #span_init;
            let __stylus_cx = <::opentelemetry::Context as ::opentelemetry::trace::TraceContextExt>::current_with_span(__stylus_span);
            let __stylus_guard = __stylus_cx.attach();
            let __stylus_ret = #block;
            ::std::mem::drop(__stylus_guard);
            __stylus_ret
        }
    };

    // The multi-instrumentation branch: `start_spans` builds one child span per
    // active slot (with that slot's own tracer/parent) and hands back the single
    // context to attach, or `None` if every active slot was child-only-suppressed.
    let multi_wrapped = if is_async {
        quote! {
            ::opentelemetry::trace::FutureExt::with_context(async #block, __stylus_cx).await
        }
    } else {
        quote! {
            let __stylus_guard = __stylus_cx.attach();
            let __stylus_ret = #block;
            ::std::mem::drop(__stylus_guard);
            __stylus_ret
        }
    };

    // Fast-path dispatch on the enabled bitmask:
    //   0 -> nobody's tracing; run the body raw (a single atomic load).
    //   1 -> only the default instrumentation; today's exact code path.
    //   _ -> at least one named instrumentation is active; go wide.
    quote! {
        #(#attrs)*
        #vis #sig {
            #[::linkme::distributed_slice(::stylus::registry::REGISTRY)]
            static __STYLUS_SITE: ::stylus::registry::TraceSite =
                ::stylus::registry::TraceSite::new(#registry_key);

            let __stylus_mask =
                __STYLUS_SITE.enabled_mask.load(::std::sync::atomic::Ordering::Relaxed);
            if __stylus_mask == 0u64 {
                #block
            } else if __stylus_mask == 1u64 {
                if __STYLUS_SITE.child_only_mask.load(::std::sync::atomic::Ordering::Relaxed) & 1u64 != 0
                    && !<::opentelemetry::Context as ::opentelemetry::trace::TraceContextExt>::span(
                        &::opentelemetry::Context::current(),
                    )
                    .is_recording()
                {
                    #block
                } else {
                    #default_enabled_branch
                }
            } else {
                match ::stylus::instrumentation::start_spans(
                    __stylus_mask,
                    __STYLUS_SITE.child_only_mask.load(::std::sync::atomic::Ordering::Relaxed),
                    #span_name,
                    #tracer_name,
                    ::std::vec![#(#kvs),*],
                ) {
                    ::std::option::Option::None => #block,
                    ::std::option::Option::Some(__stylus_cx) => { #multi_wrapped }
                }
            }
        }
    }
}
