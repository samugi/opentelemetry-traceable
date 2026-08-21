//! Proc-macro implementation of `#[traceable]`.
//!
//! Uses an `#[in_span]`-style argument surface (`name`,
//! `fields(key = expr, ...)`), but gates span creation behind a per-function
//! bitmask discovered at link time via `linkme` (one bit per
//! `opentelemetry_traceable::instrumentation` slot), so tracing can be toggled per function --
//! and per instrumentation -- at runtime without recompiling. There is no
//! `tracer` argument: each instrumentation brings its own tracer, so a call site
//! has nothing to name.
//!
//! Important: expanded macros only depend on `opentelemetry` via the
//! re-exported ::opentelemetry_traceable::opentelemetry namespace.
//! Additional dependencies need to be handled appropriately by adapting manifest files.

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
/// Every call checks a per-function, link-time-registered bitmask before doing
/// any span/context work, so a function no instrumentation is tracing costs a
/// single atomic load. Enable it at runtime through a
/// `opentelemetry_traceable::instrumentation::Instrumentation`, which supplies the tracer the
/// span is created from — the macro itself never names or looks up a tracer.
///
/// The registry key used to enable a function defaults to
/// `module_path!() + "::" + fn_name`, or the `name` argument if given. Note the
/// key is not qualified by the surrounding `impl` type, so two methods with the
/// same name in the same module share a key unless `name` disambiguates them.
///
/// ```ignore
/// #[traceable]
/// fn process() { }
///
/// #[traceable(name = "kafka.fetch")]
/// async fn fetch() { }
///
/// #[traceable(fields("component" = "proxy", request_id = id))]
/// async fn handle(id: String) { }
/// ```
///
/// Where the span hangs is decided by the instrumentation, not here: an
/// in-process one parents within its own hierarchy, a distributed one parents to
/// whatever span is current (including one extracted from an incoming
/// `traceparent`).
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

    // The `fields(...)` as `KeyValue` expressions, handed to `start_spans` so
    // every active instrumentation stamps the same attributes on its own span.
    let kvs: Vec<TokenStream2> = args
        .fields
        .iter()
        .flatten()
        .map(|f| {
            let (key, value) = (&f.key, &f.value);
            quote! { ::opentelemetry_traceable::opentelemetry::KeyValue::new(#key, #value) }
        })
        .collect();

    // `start_spans` builds one child span per active slot (with that slot's own
    // tracer and parent) and hands back the single context to attach, or `None`
    // if no span was created -- every active slot's instrumentation was dropped
    // mid-flight.
    let traced = if is_async {
        quote! {
            ::opentelemetry_traceable::opentelemetry::trace::FutureExt::with_context(async #block, __traceable_cx).await
        }
    } else {
        quote! {
            let __traceable_guard = __traceable_cx.attach();
            let __traceable_ret = #block;
            ::std::mem::drop(__traceable_guard);
            __traceable_ret
        }
    };

    // Fast-path dispatch on the enabled bitmask:
    //   0 -> no instrumentation is tracing this function; run the body raw,
    //        having paid a single atomic load.
    //   _ -> at least one is; build a span per active slot.
    quote! {
        #(#attrs)*
        #vis #sig {
            #[::opentelemetry_traceable::__private::linkme::distributed_slice(::opentelemetry_traceable::registry::REGISTRY)]
            #[linkme(crate = ::opentelemetry_traceable::__private::linkme)]
            static __TRACEABLE_SITE: ::opentelemetry_traceable::registry::TraceSite =
                ::opentelemetry_traceable::registry::TraceSite::new(#registry_key);

            let __traceable_mask =
                __TRACEABLE_SITE.enabled_mask.load(::std::sync::atomic::Ordering::Relaxed);
            if __traceable_mask == 0u64 {
                #block
            } else {
                match ::opentelemetry_traceable::instrumentation::start_spans(
                    __traceable_mask,
                    #span_name,
                    ::std::vec![#(#kvs),*],
                ) {
                    ::std::option::Option::None => #block,
                    ::std::option::Option::Some(__traceable_cx) => { #traced }
                }
            }
        }
    }
}
