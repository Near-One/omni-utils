use proc_macro::{self, TokenStream};

mod error_display;
mod trusted_relayer;

#[proc_macro_derive(ErrorDisplay)]
pub fn derive_error_display(input: TokenStream) -> TokenStream {
    error_display::derive_error_display(input)
}

#[proc_macro_attribute]
pub fn trusted_relayer(args: TokenStream, input: TokenStream) -> TokenStream {
    trusted_relayer::trusted_relayer(args, input)
}
