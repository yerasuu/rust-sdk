//! Client-mode expansion: for each handler, emit (under
//! `#[cfg(not(feature = "server"))]`) two fns with the handler's name and
//! **verbatim** return type:
//!
//! - `<name>_with(provider, args…)` — explicit `&dyn RequestProvider`.
//! - `<name>(args…)` — resolves the provider from the global registry by
//!   `env!("CARGO_PKG_NAME")` (evaluated in the *service crate's* build, so
//!   each service keys its own entry) and delegates to `<name>_with`.
//!
//! Args mirror the TS client: path args unwrapped (interpolated +
//! urlencoded), the `Query<Q>` struct, the `Json<B>`/`Form<B>` DTO; server
//! extractors are stripped. All serde happens in
//! `ts_server_fn_client::call`, whose `T`/`E` are *inferred* from the
//! declared return type — which is what keeps `*Result<T>` aliases working.
//!
//! `raw` handlers and non-`Result`-like returns are server-only: no client
//! fn is emitted (there is nowhere for transport errors to go).

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{ItemFn, LitStr, ReturnType};

use crate::route::{is_result_like, ArgKind, HandlerMeta, RouteAttr};

pub fn emit_client(
    method: &str,
    route: &RouteAttr,
    meta: &HandlerMeta,
    func: &ItemFn,
) -> TokenStream2 {
    if route.raw {
        return TokenStream2::new();
    }
    let result_like = match &func.sig.output {
        ReturnType::Type(_, ty) => is_result_like(ty),
        ReturnType::Default => false,
    };
    if !result_like {
        return TokenStream2::new();
    }
    // `Form<B>` needs an urlencoded body + content type the v1 transport
    // doesn't carry — like `raw`, Form handlers are server-only.
    let has_form = func.sig.inputs.iter().any(|input| {
        let syn::FnArg::Typed(pt) = input else { return false };
        if let syn::Type::Path(tp) = pt.ty.as_ref() {
            return tp.path.segments.last().is_some_and(|s| s.ident == "Form");
        }
        false
    });
    if has_form {
        return TokenStream2::new();
    }

    let vis = &func.vis;
    let name = &func.sig.ident;
    let with_name = format_ident!("{}_with", name);
    let output = &func.sig.output;

    let params: Vec<TokenStream2> = meta
        .client_args
        .iter()
        .map(|a| {
            let id = format_ident!("{}", a.name);
            let ty = &a.ty;
            quote! { #id: #ty }
        })
        .collect();
    let arg_names: Vec<_> = meta
        .client_args
        .iter()
        .map(|a| format_ident!("{}", a.name))
        .collect();

    // `path_template` already uses `{}` placeholders → direct `format!` fit.
    let template = LitStr::new(&meta.path_template, Span::call_site());
    let path_exprs: Vec<TokenStream2> = meta
        .path_names
        .iter()
        .map(|n| {
            let id = format_ident!("{}", n);
            quote! { ::ts_server_fn_client::urlencode(&#id.to_string()) }
        })
        .collect();

    let query = meta
        .client_args
        .iter()
        .find(|a| a.kind == ArgKind::Query)
        .map(|a| format_ident!("{}", a.name));
    let body = meta
        .client_args
        .iter()
        .find(|a| a.kind == ArgKind::Body)
        .map(|a| format_ident!("{}", a.name));
    let query_expr = match &query {
        Some(id) => quote! { ::core::option::Option::Some(&#id) },
        None => quote! { ::core::option::Option::<&()>::None },
    };
    let body_expr = match &body {
        Some(id) => quote! { ::core::option::Option::Some(&#id) },
        None => quote! { ::core::option::Option::<&()>::None },
    };

    let method_ident = format_ident!("{}", method);

    quote! {
        #[cfg(not(feature = "server"))]
        #vis async fn #with_name(
            __provider: &dyn ::ts_server_fn_client::RequestProvider,
            #(#params),*
        ) #output {
            let __path = ::std::format!(#template, #(#path_exprs),*);
            ::ts_server_fn_client::call(
                __provider,
                ::ts_server_fn_client::Method::#method_ident,
                &__path,
                #query_expr,
                #body_expr,
            ).await
        }

        #[cfg(not(feature = "server"))]
        #vis async fn #name(#(#params),*) #output {
            let __p = ::ts_server_fn_client::provider_for(::std::env!("CARGO_PKG_NAME"));
            #with_name(&*__p, #(#arg_names),*).await
        }
    }
}
