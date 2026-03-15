use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Expr, ItemFn, ItemImpl, Token, parenthesized, parse_macro_input};

struct RoleExpr {
    expr: Expr,
}

impl Parse for RoleExpr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let expr: Expr = input.parse()?;
        Ok(RoleExpr { expr })
    }
}

struct NamedRoleList {
    name: syn::Ident,
    roles: Vec<Expr>,
}

impl Parse for NamedRoleList {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: syn::Ident = input.parse()?;
        let content;
        parenthesized!(content in input);
        let roles: Punctuated<RoleExpr, Token![,]> =
            content.parse_terminated(RoleExpr::parse, Token![,])?;
        Ok(NamedRoleList {
            name,
            roles: roles.into_iter().map(|r| r.expr).collect(),
        })
    }
}

struct TrustedRelayerImplArgs {
    bypass_roles: Option<Vec<Expr>>,
    manager_roles: Vec<Expr>,
}

impl Parse for TrustedRelayerImplArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut bypass_roles = None;
        let mut manager_roles = None;

        let items: Punctuated<NamedRoleList, Token![,]> =
            input.parse_terminated(NamedRoleList::parse, Token![,])?;

        for item in items {
            match item.name.to_string().as_str() {
                "bypass_roles" => {
                    if bypass_roles.is_some() {
                        return Err(syn::Error::new(
                            item.name.span(),
                            "duplicate `bypass_roles`",
                        ));
                    }
                    bypass_roles = Some(item.roles);
                }
                "manager_roles" => {
                    if manager_roles.is_some() {
                        return Err(syn::Error::new(
                            item.name.span(),
                            "duplicate `manager_roles`",
                        ));
                    }
                    manager_roles = Some(item.roles);
                }
                other => {
                    return Err(syn::Error::new(
                        item.name.span(),
                        format!(
                            "unknown parameter `{other}`, \
                             expected `bypass_roles` or `manager_roles`"
                        ),
                    ));
                }
            }
        }

        let manager_roles = manager_roles.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "`manager_roles(...)` is required for `#[trusted_relayer]` on impl blocks",
            )
        })?;

        Ok(TrustedRelayerImplArgs {
            bypass_roles,
            manager_roles,
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

fn gen_trait_impl(self_ty: &syn::Type, bypass_roles: &Option<Vec<Expr>>) -> TokenStream2 {
    let override_method = bypass_roles
        .as_ref()
        .map(|roles| gen_bypass_is_trusted(roles));

    quote! {
        impl ::omni_utils::trusted_relayer::TrustedRelayer for #self_ty {
            #override_method
        }
    }
}

fn gen_public_methods(self_ty: &syn::Type, manager_roles: &[Expr]) -> TokenStream2 {
    quote! {
        #[::near_sdk::near]
        impl #self_ty {
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

            #[::near_plugins::access_control_any(roles(#(#manager_roles),*))]
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

fn process_impl_block(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as TrustedRelayerImplArgs);
    let item_impl = parse_macro_input!(input as ItemImpl);

    let self_ty = &item_impl.self_ty;
    let trait_impl = gen_trait_impl(self_ty, &args.bypass_roles);
    let public_methods = gen_public_methods(self_ty, &args.manager_roles);

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
        process_impl_block(args, input)
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
