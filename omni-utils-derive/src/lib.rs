use proc_macro::{self, TokenStream};

mod error_display;

#[proc_macro_derive(ErrorDisplay)]
pub fn derive_error_display(input: TokenStream) -> TokenStream {
    error_display::derive_error_display(input)
}
