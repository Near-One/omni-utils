use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Expr, ImplItem, ItemFn, ItemImpl, Token, parenthesized, parse_macro_input};

struct RoleExpr {
    expr: Expr,
}

impl Parse for RoleExpr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let expr: Expr = input.parse()?;
        Ok(RoleExpr { expr })
    }
}

enum MacroParam {
    RoleList { name: syn::Ident, roles: Vec<Expr> },
    Flag(syn::Ident),
}

impl Parse for MacroParam {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: syn::Ident = input.parse()?;
        if input.peek(syn::token::Paren) {
            let content;
            parenthesized!(content in input);
            let roles: Punctuated<RoleExpr, Token![,]> =
                content.parse_terminated(RoleExpr::parse, Token![,])?;
            Ok(MacroParam::RoleList {
                name,
                roles: roles.into_iter().map(|r| r.expr).collect(),
            })
        } else {
            Ok(MacroParam::Flag(name))
        }
    }
}

struct TrustedRelayerImplArgs {
    bypass_roles: Option<Vec<Expr>>,
    manager_roles: Vec<Expr>,
    /// Optional separate roles for `set_relayer_config`. Falls back to `manager_roles` if absent.
    config_roles: Option<Vec<Expr>>,
    custom_is_trusted_relayer: bool,
}

impl Parse for TrustedRelayerImplArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut bypass_roles = None;
        let mut manager_roles = None;
        let mut config_roles = None;
        let mut custom_is_trusted_relayer = false;

        let items: Punctuated<MacroParam, Token![,]> =
            input.parse_terminated(MacroParam::parse, Token![,])?;

        for item in items {
            match item {
                MacroParam::RoleList { name, roles } => match name.to_string().as_str() {
                    "bypass_roles" => {
                        if bypass_roles.is_some() {
                            return Err(syn::Error::new(name.span(), "duplicate `bypass_roles`"));
                        }
                        bypass_roles = Some(roles);
                    }
                    "manager_roles" => {
                        if manager_roles.is_some() {
                            return Err(syn::Error::new(name.span(), "duplicate `manager_roles`"));
                        }
                        manager_roles = Some(roles);
                    }
                    "config_roles" => {
                        if config_roles.is_some() {
                            return Err(syn::Error::new(name.span(), "duplicate `config_roles`"));
                        }
                        config_roles = Some(roles);
                    }
                    other => {
                        return Err(syn::Error::new(
                            name.span(),
                            format!(
                                "unknown parameter `{other}`, expected \
                                 `bypass_roles`, `manager_roles`, `config_roles`, \
                                 or `custom_is_trusted_relayer`"
                            ),
                        ));
                    }
                },
                MacroParam::Flag(name) => match name.to_string().as_str() {
                    "custom_is_trusted_relayer" => {
                        custom_is_trusted_relayer = true;
                    }
                    other => {
                        return Err(syn::Error::new(
                            name.span(),
                            format!("unknown flag `{other}`, expected `custom_is_trusted_relayer`"),
                        ));
                    }
                },
            }
        }

        let manager_roles = manager_roles.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "`manager_roles(...)` is required for `#[trusted_relayer]` on impl blocks",
            )
        })?;

        if custom_is_trusted_relayer && bypass_roles.is_some() {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "`custom_is_trusted_relayer` and `bypass_roles` are mutually exclusive",
            ));
        }

        Ok(TrustedRelayerImplArgs {
            bypass_roles,
            manager_roles,
            config_roles,
            custom_is_trusted_relayer,
        })
    }
}

fn gen_bypass_is_trusted(bypass_roles: &[Expr]) -> TokenStream2 {
    let role_into: Vec<TokenStream2> = bypass_roles
        .iter()
        .map(|role| {
            quote! { <_ as ::core::convert::Into<String>>::into(#role) }
        })
        .collect();

    quote! {
        fn is_trusted_relayer(&self, account_id: &::near_sdk::AccountId) -> bool {
            if ::near_plugins::AccessControllable::acl_has_any_role(
                self,
                ::std::vec![#(#role_into),*],
                account_id.clone(),
            ) {
                return true;
            }

            ::omni_utils::trusted_relayer::tr_relayers_map()
                .get(account_id)
                .is_some_and(|state| {
                    ::near_sdk::env::block_timestamp() >= state.activate_at.0
                })
        }
    }
}

fn gen_trait_impl(
    self_ty: &syn::Type,
    generics: &syn::Generics,
    bypass_roles: &Option<Vec<Expr>>,
    custom_is_trusted_relayer: bool,
) -> TokenStream2 {
    if custom_is_trusted_relayer {
        return quote! {};
    }

    let (impl_generics, _, where_clause) = generics.split_for_impl();

    let override_method = bypass_roles
        .as_ref()
        .map(|roles| gen_bypass_is_trusted(roles));

    quote! {
        impl #impl_generics ::omni_utils::trusted_relayer::TrustedRelayer for #self_ty #where_clause {
            #override_method
        }
    }
}

fn gen_public_methods(
    self_ty: &syn::Type,
    generics: &syn::Generics,
    manager_roles: &[Expr],
    config_roles: &[Expr],
) -> TokenStream2 {
    let (impl_generics, _, where_clause) = generics.split_for_impl();
    quote! {
        #[::near_sdk::near]
        impl #impl_generics #self_ty #where_clause {
            pub fn is_trusted_relayer(
                &self,
                account_id: &::near_sdk::AccountId,
            ) -> bool {
                <Self as ::omni_utils::trusted_relayer::TrustedRelayer>::is_trusted_relayer(
                    self,
                    account_id,
                )
            }

            #[payable]
            pub fn apply_for_trusted_relayer(&mut self) {
                <Self as ::omni_utils::trusted_relayer::TrustedRelayer>::_tr_apply(self);
            }

            pub fn resign_trusted_relayer(&mut self) -> ::near_sdk::Promise {
                <Self as ::omni_utils::trusted_relayer::TrustedRelayer>::_tr_resign(self)
            }

            #[::near_plugins::access_control_any(roles(#(#manager_roles),*))]
            pub fn reject_relayer_application(
                &mut self,
                account_id: ::near_sdk::AccountId,
            ) -> ::near_sdk::Promise {
                <Self as ::omni_utils::trusted_relayer::TrustedRelayer>::_tr_reject(
                    self,
                    account_id,
                )
            }

            #[::near_plugins::access_control_any(roles(#(#config_roles),*))]
            pub fn set_relayer_config(
                &mut self,
                stake_required: ::near_sdk::NearToken,
                waiting_period_ns: ::near_sdk::json_types::U64,
            ) {
                <Self as ::omni_utils::trusted_relayer::TrustedRelayer>::_tr_set_config(
                    self,
                    stake_required,
                    waiting_period_ns,
                );
            }

            #[must_use]
            pub fn get_relayer_application(
                &self,
                account_id: &::near_sdk::AccountId,
            ) -> Option<::omni_utils::trusted_relayer::RelayerState> {
                <Self as ::omni_utils::trusted_relayer::TrustedRelayer>::_tr_get_application(
                    self,
                    account_id,
                )
            }

            #[must_use]
            pub fn get_relayer_stake(
                &self,
                account_id: &::near_sdk::AccountId,
            ) -> Option<::near_sdk::json_types::U128> {
                <Self as ::omni_utils::trusted_relayer::TrustedRelayer>::_tr_get_stake(
                    self,
                    account_id,
                )
            }

            #[must_use]
            pub fn get_relayer_config(
                &self,
            ) -> ::omni_utils::trusted_relayer::RelayerConfig {
                <Self as ::omni_utils::trusted_relayer::TrustedRelayer>::_tr_get_config(self)
            }
        }
    }
}

/// Check if an attribute path matches `trusted_relayer`.
fn is_trusted_relayer_attr(attr: &syn::Attribute) -> bool {
    attr.path().is_ident("trusted_relayer")
}

/// Parse method-level `#[trusted_relayer(bypass_roles(R1, R2, ...))]` arguments.
/// Returns `None` for bare `#[trusted_relayer]`, `Some(roles)` for bypass_roles override.
fn parse_method_bypass_roles(attr: &syn::Attribute) -> Option<Vec<Expr>> {
    match &attr.meta {
        syn::Meta::Path(_) => None, // bare #[trusted_relayer]
        syn::Meta::List(list) => {
            let parsed: syn::Result<Punctuated<MacroParam, Token![,]>> =
                list.parse_args_with(Punctuated::parse_terminated);
            let params = parsed.expect(
                "`#[trusted_relayer]` on methods only accepts `bypass_roles(...)` argument",
            );
            let mut bypass = None;
            for param in params {
                match param {
                    MacroParam::RoleList { name, roles } if name == "bypass_roles" => {
                        bypass = Some(roles);
                    }
                    _ => panic!(
                        "`#[trusted_relayer]` on methods only accepts `bypass_roles(...)` argument"
                    ),
                }
            }
            bypass
        }
        _ => panic!("`#[trusted_relayer]` does not support this syntax"),
    }
}

/// Generate an inline guard that checks method-specific bypass roles,
/// falling back to the staking map (completely replacing impl-level bypass roles).
fn gen_method_bypass_guard(bypass_roles: &[Expr]) -> syn::Stmt {
    let role_into: Vec<TokenStream2> = bypass_roles
        .iter()
        .map(|role| {
            quote! { <_ as ::core::convert::Into<String>>::into(#role) }
        })
        .collect();

    syn::parse2(quote! {
        {
            let __caller = ::near_sdk::env::predecessor_account_id();
            if !::near_plugins::AccessControllable::acl_has_any_role(
                self,
                ::std::vec![#(#role_into),*],
                __caller.clone(),
            ) {
                ::near_sdk::require!(
                    ::omni_utils::trusted_relayer::tr_relayers_map()
                        .get(&__caller)
                        .is_some_and(|state| {
                            ::near_sdk::env::block_timestamp() >= state.activate_at.0
                        }),
                    "Relayer is not active"
                );
            }
        }
    })
    .expect("failed to parse method bypass guard")
}

/// Inject the guard into method bodies that have `#[trusted_relayer]`,
/// and strip the attribute so downstream macros don't see it.
///
/// - `#[trusted_relayer]` (no args): injects a guard that calls `is_trusted_relayer`
///   (uses impl-level bypass roles).
/// - `#[trusted_relayer(bypass_roles(R1, R2))]`: injects an inline guard that checks
///   the specified roles via ACL, falling back to the staking map directly. This
///   completely replaces the impl-level bypass roles for that method.
fn inject_guards(item_impl: &mut ItemImpl) {
    let default_guard: syn::Stmt = syn::parse2(quote! {
        ::near_sdk::require!(
            <Self as ::omni_utils::trusted_relayer::TrustedRelayer>::is_trusted_relayer(
                self,
                &::near_sdk::env::predecessor_account_id(),
            ),
            "Relayer is not active"
        );
    })
    .expect("failed to parse trusted_relayer guard statement");

    for item in &mut item_impl.items {
        if let ImplItem::Fn(method) = item {
            let tr_attr = method.attrs.iter().find(|a| is_trusted_relayer_attr(a));
            if let Some(attr) = tr_attr {
                let guard = match parse_method_bypass_roles(attr) {
                    Some(roles) => gen_method_bypass_guard(&roles),
                    None => default_guard.clone(),
                };
                method.attrs.retain(|a| !is_trusted_relayer_attr(a));
                method.block.stmts.insert(0, guard);
            }
        }
    }
}

/// Guard-only mode: `#[trusted_relayer]` on an impl block without arguments.
/// Only injects guards into methods annotated with `#[trusted_relayer]`.
/// Does NOT generate public methods or trait impl — those are emitted by
/// the "full" mode (with `manager_roles(...)` etc.).
///
/// This allows multiple impl blocks to use `#[trusted_relayer]` method guards
/// while only one block carries the full configuration.
fn process_impl_block_guard_only(input: TokenStream) -> TokenStream {
    let mut item_impl = parse_macro_input!(input as ItemImpl);
    inject_guards(&mut item_impl);
    quote! { #item_impl }.into()
}

fn process_impl_block(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as TrustedRelayerImplArgs);
    let mut item_impl = parse_macro_input!(input as ItemImpl);

    // Process method-level #[trusted_relayer] before passing to #[near]
    inject_guards(&mut item_impl);

    let self_ty = &item_impl.self_ty;
    let generics = &item_impl.generics;
    let trait_impl = gen_trait_impl(
        self_ty,
        generics,
        &args.bypass_roles,
        args.custom_is_trusted_relayer,
    );
    let config_roles = args.config_roles.as_deref().unwrap_or(&args.manager_roles);
    let public_methods = gen_public_methods(self_ty, generics, &args.manager_roles, config_roles);

    let output = quote! {
        #item_impl
        #trait_impl
        #public_methods
    };

    output.into()
}

fn process_fn(input: TokenStream) -> TokenStream {
    let mut item_fn = parse_macro_input!(input as ItemFn);

    let guard = syn::parse2::<syn::Stmt>(quote! {
        ::near_sdk::require!(
            <Self as ::omni_utils::trusted_relayer::TrustedRelayer>::is_trusted_relayer(
                self,
                &::near_sdk::env::predecessor_account_id(),
            ),
            "Relayer is not active"
        );
    })
    .expect("failed to parse trusted_relayer guard statement");

    item_fn.block.stmts.insert(0, guard);

    quote! { #item_fn }.into()
}

pub fn trusted_relayer(args: TokenStream, input: TokenStream) -> TokenStream {
    let input_clone: proc_macro2::TokenStream = input.clone().into();

    if is_impl_block(&input_clone) {
        if args.is_empty() {
            // Guard-only mode: inject guards into #[trusted_relayer] methods
            // without generating public methods or trait impl.
            process_impl_block_guard_only(input)
        } else {
            // Full mode: inject guards, generate trait impl + public methods.
            process_impl_block(args, input)
        }
    } else {
        if !args.is_empty() {
            return syn::Error::new(
                proc_macro2::Span::call_site(),
                "`#[trusted_relayer]` on methods does not accept arguments",
            )
            .to_compile_error()
            .into();
        }
        process_fn(input)
    }
}

fn is_impl_block(tokens: &proc_macro2::TokenStream) -> bool {
    for tt in tokens.clone() {
        match &tt {
            proc_macro2::TokenTree::Ident(ident) => {
                return ident == "impl";
            }
            proc_macro2::TokenTree::Punct(p) if p.as_char() == '#' => continue,
            proc_macro2::TokenTree::Group(_) => continue,
            _ => return false,
        }
    }
    false
}
