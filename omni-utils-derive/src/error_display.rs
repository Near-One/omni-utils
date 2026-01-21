use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, LitStr, parse_macro_input};

fn build_format_string(field_names: &[String]) -> String {
    let mut fmt = String::from("{}");
    if !field_names.is_empty() {
        fmt.push_str(": ");
        for (index, name) in field_names.iter().enumerate() {
            if index > 0 {
                fmt.push_str(", ");
            }
            fmt.push_str(name);
            fmt.push_str("={}");
        }
    }
    fmt
}

pub fn derive_error_display(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let data = match input.data {
        Data::Enum(data) => data,
        _ => {
            return syn::Error::new_spanned(&name, "ErrorDisplay can only be derived for enums")
                .to_compile_error()
                .into();
        }
    };

    let as_ref_arms = data.variants.iter().map(|variant| {
        let variant_ident = &variant.ident;
        match &variant.fields {
            Fields::Unit => {
                quote! {
                    Self::#variant_ident => {
                        ::std::convert::AsRef::<str>::as_ref(self).to_string()
                    }
                }
            }
            Fields::Unnamed(fields) => {
                let field_idents: Vec<_> = (0..fields.unnamed.len())
                    .map(|index| format_ident!("field{}", index + 1))
                    .collect();
                let field_names: Vec<String> = (0..fields.unnamed.len())
                    .map(|index| format!("field{}", index + 1))
                    .collect();
                let fmt_string = build_format_string(&field_names);
                let fmt_lit = LitStr::new(&fmt_string, variant_ident.span());

                quote! {
                    Self::#variant_ident( #( #field_idents ),* ) => {
                        format!(
                            #fmt_lit,
                            ::std::convert::AsRef::<str>::as_ref(self),
                            #( #field_idents ),*
                        )
                    }
                }
            }
            Fields::Named(fields) => {
                let field_idents: Vec<_> = fields
                    .named
                    .iter()
                    .map(|field| field.ident.as_ref().expect("named field"))
                    .collect();
                let field_names: Vec<String> =
                    field_idents.iter().map(|ident| ident.to_string()).collect();
                let fmt_string = build_format_string(&field_names);
                let fmt_lit = LitStr::new(&fmt_string, variant_ident.span());

                quote! {
                    Self::#variant_ident { #( #field_idents ),* } => {
                        format!(
                            #fmt_lit,
                            ::std::convert::AsRef::<str>::as_ref(self),
                            #( #field_idents ),*
                        )
                    }
                }
            }
        }
    });

    let expanded = quote! {
        impl #impl_generics #name #ty_generics #where_clause {
            pub fn as_ref(&self) -> String {
                match self {
                    #( #as_ref_arms )*
                }
            }
        }

        impl #impl_generics std::fmt::Display for #name #ty_generics #where_clause {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.as_ref())
            }
        }
    };

    expanded.into()
}
