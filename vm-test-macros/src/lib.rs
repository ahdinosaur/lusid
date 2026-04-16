//! `#[lusid_vm_test]` attribute macro.
//!
//! Wraps an `async fn(driver: Driver)` test body into a regular `#[test]`
//! that hands the future off to [`lusid_vm_test::__test_runner::run`]. The
//! runtime crate owns the actual tokio runtime + driver construction; this
//! macro is intentionally tiny.
//!
//! ## Shape
//!
//! ```ignore
//! #[lusid_vm_test]
//! async fn apt_repo_installs_docker(driver: Driver) {
//!     let node = driver.node("host", presets::debian_13("host")).await.unwrap();
//!     node.apply_plan("examples/docker.lusid").await.assert_succeeded();
//! }
//! ```
//!
//! Expands (roughly) to:
//!
//! ```ignore
//! #[test]
//! fn apt_repo_installs_docker() {
//!     async fn __body(driver: ::lusid_vm_test::Driver) { /* original body */ }
//!     ::lusid_vm_test::__test_runner::run(
//!         env!("CARGO_PKG_NAME"),
//!         "apt_repo_installs_docker",
//!         __body,
//!     );
//! }
//! ```
//!
//! Keeping the original body inside a nested `async fn __body` (rather than
//! an inlined closure) means compile errors on the user's code point at the
//! original source span, not at generated closure boilerplate.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Error, ItemFn, parse_macro_input};

#[proc_macro_attribute]
pub fn lusid_vm_test(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    if let Err(err) = validate(&input) {
        return err.to_compile_error().into();
    }

    let ItemFn {
        attrs,
        vis,
        sig,
        block,
    } = input;

    let fn_name = &sig.ident;
    let fn_name_str = fn_name.to_string();
    let inputs = &sig.inputs;
    let output = &sig.output;

    let expanded = quote! {
        #(#attrs)*
        #[test]
        #vis fn #fn_name() {
            async fn __lusid_vm_test_body(#inputs) #output #block
            ::lusid_vm_test::__test_runner::run(
                env!("CARGO_PKG_NAME"),
                #fn_name_str,
                __lusid_vm_test_body,
            );
        }
    };

    expanded.into()
}

fn validate(input: &ItemFn) -> Result<(), Error> {
    if input.sig.asyncness.is_none() {
        return Err(Error::new_spanned(
            input.sig.fn_token,
            "#[lusid_vm_test] requires `async fn`",
        ));
    }
    if input.sig.inputs.len() != 1 {
        return Err(Error::new_spanned(
            &input.sig.inputs,
            "#[lusid_vm_test] requires exactly one argument: the Driver",
        ));
    }
    Ok(())
}
