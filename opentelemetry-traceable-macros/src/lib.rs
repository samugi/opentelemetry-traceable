//! Proc-macro implementation of `#[traceable]`.
//!
//! Instruments a function and makes it "traceable". Traceable
//! functions (aka trace sites) can be dynamically enabled or
//! disabled to turn tracing on and off on a per-function basis.
//!
//! Span creation is controlled via a per-function bitmask that enables
//! multiple instrumentations to coexist and produce different trace
//! shapes/hierarchies (a function may produce a span for a given
//! instrumentation and not for another). This way, tracing can be
//! controller per-function and per-instrumentation, at runtime.
//!
//! The `traceable` macro allows configuring certain parameters of
//! the trace site, such as the span name and attributes.
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
/// Every call checks a per-function bitmask before creating any span.
/// A disabled trace site has the cost of one atomic load.
///
/// Enable it at runtime through a
/// `opentelemetry_traceable::instrumentation::Instrumentation`.
///
/// The registry key used to enable a function defaults to
/// `module_path!() + "::" + fn_name`, or the `name` argument if given.
/// Note: two methods with the same name in the same module share a key
/// unless `name` is used to distinguish them.
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
    // if no span was created.
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

    // Check the bitmask to add zero overhead where possible:
    //   0 -> no instrumentation is tracing this function: run the raw body,
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
                __TRACEABLE_SITE.enabled_slots.load(::std::sync::atomic::Ordering::Relaxed);
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
