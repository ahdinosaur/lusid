//! Procedural derive macro for [`rimu_interop::FromRimu`].
//!
//! Generates `FromRimu` impls for plain structs and for enums tagged with a
//! string discriminant field. The generated code unifies all errors under
//! `rimu_interop::FromRimuError`.
//!
//! # Container attributes (`#[rimu(...)]`)
//!
//! - `tag = "name"`: enum is internally tagged on `name` (the matched variant
//!   is the one whose name — after `rename_all` / variant-level `rename` —
//!   equals the string at that key).
//! - `untagged`: enum is untagged; variants are tried in declaration order and
//!   the first to parse wins.
//! - `rename_all = "..."`: applied to variant names before matching the tag.
//!   Currently supports `kebab-case` only — the only style used in lusid.
//!
//! # Variant / field attributes
//!
//! - `#[rimu(rename = "...")]` overrides the matched name for an enum variant
//!   or a struct field.
//!
//! # Field types
//!
//! `Option<T>` fields are detected syntactically: a missing key or `Value::Null`
//! becomes `None`; otherwise the value is parsed as `T`. Every other field
//! type is required and parsed via the field type's own `FromRimu` impl. Vec,
//! IndexMap, primitives, and user-defined types all flow through the trait.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    Attribute, Data, DataEnum, DataStruct, DeriveInput, Error, Expr, ExprLit, Field, Fields,
    FieldsNamed, GenericArgument, Ident, Lit, Meta, MetaNameValue, PathArguments, Token, Type,
    TypePath, Variant, parse_macro_input, punctuated::Punctuated,
};

#[proc_macro_derive(FromRimu, attributes(rimu))]
pub fn derive_from_rimu(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    match &input.data {
        Data::Struct(data) => expand_struct(input, data),
        Data::Enum(data) => expand_enum(input, data),
        Data::Union(_) => Err(Error::new_spanned(
            &input.ident,
            "FromRimu cannot be derived for unions",
        )),
    }
}

// ---------------------------------------------------------------------------
// Container / variant / field attribute parsing
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ContainerAttrs {
    tag: Option<String>,
    untagged: bool,
    rename_all: Option<RenameAll>,
}

#[derive(Default)]
struct VariantAttrs {
    rename: Option<String>,
}

#[derive(Default)]
struct FieldAttrs {
    rename: Option<String>,
}

#[derive(Clone, Copy)]
enum RenameAll {
    KebabCase,
}

impl RenameAll {
    fn apply(self, name: &str) -> String {
        match self {
            RenameAll::KebabCase => to_kebab_case(name),
        }
    }
}

/// Convert a PascalCase / camelCase identifier to `kebab-case`.
///
/// Only ASCII letters and digits are recognised — the lusid resource enum
/// variants are all simple ASCII identifiers.
fn to_kebab_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index != 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn parse_container_attrs(attrs: &[Attribute]) -> syn::Result<ContainerAttrs> {
    let mut out = ContainerAttrs::default();
    for attr in attrs.iter().filter(|a| a.path().is_ident("rimu")) {
        let metas: Punctuated<Meta, Token![,]> =
            attr.parse_args_with(Punctuated::parse_terminated)?;
        for meta in metas {
            match &meta {
                Meta::Path(path) if path.is_ident("untagged") => {
                    out.untagged = true;
                }
                Meta::NameValue(MetaNameValue {
                    path,
                    value: Expr::Lit(ExprLit { lit: Lit::Str(s), .. }),
                    ..
                }) if path.is_ident("tag") => {
                    out.tag = Some(s.value());
                }
                Meta::NameValue(MetaNameValue {
                    path,
                    value: Expr::Lit(ExprLit { lit: Lit::Str(s), .. }),
                    ..
                }) if path.is_ident("rename_all") => {
                    let value = s.value();
                    out.rename_all = Some(match value.as_str() {
                        "kebab-case" => RenameAll::KebabCase,
                        other => {
                            return Err(Error::new_spanned(
                                s,
                                format!(
                                    "unsupported rename_all style: \"{other}\" (only \"kebab-case\" is supported)"
                                ),
                            ));
                        }
                    });
                }
                other => {
                    return Err(Error::new_spanned(
                        other,
                        "unrecognised rimu attribute",
                    ));
                }
            }
        }
    }

    if out.untagged && out.tag.is_some() {
        return Err(Error::new_spanned(
            attrs.first().unwrap(),
            "rimu(tag) and rimu(untagged) are mutually exclusive",
        ));
    }

    Ok(out)
}

fn parse_variant_attrs(attrs: &[Attribute]) -> syn::Result<VariantAttrs> {
    let mut out = VariantAttrs::default();
    for attr in attrs.iter().filter(|a| a.path().is_ident("rimu")) {
        let metas: Punctuated<Meta, Token![,]> =
            attr.parse_args_with(Punctuated::parse_terminated)?;
        for meta in metas {
            if let Meta::NameValue(MetaNameValue {
                path,
                value: Expr::Lit(ExprLit { lit: Lit::Str(s), .. }),
                ..
            }) = &meta
                && path.is_ident("rename")
            {
                out.rename = Some(s.value());
                continue;
            }
            return Err(Error::new_spanned(
                &meta,
                "unrecognised rimu variant attribute",
            ));
        }
    }
    Ok(out)
}

fn parse_field_attrs(attrs: &[Attribute]) -> syn::Result<FieldAttrs> {
    let mut out = FieldAttrs::default();
    for attr in attrs.iter().filter(|a| a.path().is_ident("rimu")) {
        let metas: Punctuated<Meta, Token![,]> =
            attr.parse_args_with(Punctuated::parse_terminated)?;
        for meta in metas {
            if let Meta::NameValue(MetaNameValue {
                path,
                value: Expr::Lit(ExprLit { lit: Lit::Str(s), .. }),
                ..
            }) = &meta
                && path.is_ident("rename")
            {
                out.rename = Some(s.value());
                continue;
            }
            return Err(Error::new_spanned(
                &meta,
                "unrecognised rimu field attribute",
            ));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Field-level helpers
// ---------------------------------------------------------------------------

/// Detect `Option<T>`. Returns the inner type `T` if matched.
///
/// Recognises `Option<T>`, `std::option::Option<T>`, and `core::option::Option<T>`.
fn unwrap_option(ty: &Type) -> Option<&Type> {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return None;
    };
    let last = path.segments.last()?;
    if last.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    if args.args.len() != 1 {
        return None;
    }
    if let Some(GenericArgument::Type(inner)) = args.args.first() {
        Some(inner)
    } else {
        None
    }
}

/// Identifier used as the name of a field in matching / error messages.
///
/// Defaults to the field's Rust identifier. A `#[rimu(rename = "...")]`
/// overrides it.
fn field_key(field: &Field) -> syn::Result<String> {
    let attrs = parse_field_attrs(&field.attrs)?;
    Ok(attrs.rename.unwrap_or_else(|| {
        field
            .ident
            .as_ref()
            .expect("named field has identifier")
            .to_string()
    }))
}

// ---------------------------------------------------------------------------
// Struct codegen
// ---------------------------------------------------------------------------

fn expand_struct(input: &DeriveInput, data: &DataStruct) -> syn::Result<TokenStream2> {
    let Fields::Named(fields) = &data.fields else {
        return Err(Error::new_spanned(
            &input.ident,
            "FromRimu only supports structs with named fields",
        ));
    };

    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let body = expand_named_fields(fields, &quote!(Self))?;

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::rimu_interop::FromRimu for #name #ty_generics #where_clause {
            type Error = ::rimu_interop::FromRimuError;

            fn from_rimu(
                __value: ::rimu::Value,
            ) -> ::std::result::Result<Self, Self::Error> {
                let ::rimu::Value::Object(mut __object) = __value else {
                    return ::std::result::Result::Err(
                        ::rimu_interop::FromRimuError::WrongType {
                            expected: "an object",
                            got: ::std::boxed::Box::new(__value),
                        },
                    );
                };
                #body
            }
        }
    })
}

/// Generate the per-field reads + struct construction expression for a set
/// of named fields. `constructor` is the prefix (e.g. `Self` for a struct, or
/// `Self::Variant` for an enum variant).
///
/// Assumes a `let mut __object: rimu::ValueObject` is in scope. After running,
/// `__object` is fully drained (any remaining entry produces an UnknownField
/// error).
fn expand_named_fields(
    fields: &FieldsNamed,
    constructor: &TokenStream2,
) -> syn::Result<TokenStream2> {
    let mut field_reads: Vec<TokenStream2> = Vec::with_capacity(fields.named.len());
    let mut field_idents: Vec<&Ident> = Vec::with_capacity(fields.named.len());

    for field in &fields.named {
        let ident = field.ident.as_ref().expect("named field");
        field_idents.push(ident);
        let key = field_key(field)?;
        let key_lit = key.as_str();
        let inner_ty = unwrap_option(&field.ty);

        let read = if let Some(inner_ty) = inner_ty {
            quote! {
                let #ident: ::std::option::Option<#inner_ty> = match __object.swap_remove(#key_lit) {
                    ::std::option::Option::None => ::std::option::Option::None,
                    ::std::option::Option::Some(__field_value) => {
                        if matches!(__field_value.inner(), ::rimu::Value::Null) {
                            ::std::option::Option::None
                        } else {
                            let __spanned = <#inner_ty as ::rimu_interop::FromRimu>::from_rimu_spanned(__field_value)
                                .map_err(|__error| ::rimu_interop::FromRimuError::Field {
                                    name: ::std::string::String::from(#key_lit),
                                    error: ::std::boxed::Box::new(__error),
                                })?;
                            ::std::option::Option::Some(::rimu::Spanned::into_inner(__spanned))
                        }
                    }
                };
            }
        } else {
            let ty = &field.ty;
            quote! {
                let #ident: #ty = {
                    let __field_value = __object
                        .swap_remove(#key_lit)
                        .ok_or(::rimu_interop::FromRimuError::MissingField { name: #key_lit })?;
                    let __spanned = <#ty as ::rimu_interop::FromRimu>::from_rimu_spanned(__field_value)
                        .map_err(|__error| ::rimu_interop::FromRimuError::Field {
                            name: ::std::string::String::from(#key_lit),
                            error: ::std::boxed::Box::new(__error),
                        })?;
                    ::rimu::Spanned::into_inner(__spanned)
                };
            }
        };
        field_reads.push(read);
    }

    Ok(quote! {
        #(#field_reads)*
        if let ::std::option::Option::Some((__unknown_key, __unknown_value)) = __object.into_iter().next() {
            return ::std::result::Result::Err(::rimu_interop::FromRimuError::UnknownField {
                span: ::rimu::Spanned::span(&__unknown_value),
                name: __unknown_key,
            });
        }
        ::std::result::Result::Ok(#constructor { #(#field_idents),* })
    })
}

// ---------------------------------------------------------------------------
// Enum codegen
// ---------------------------------------------------------------------------

fn expand_enum(input: &DeriveInput, data: &DataEnum) -> syn::Result<TokenStream2> {
    let attrs = parse_container_attrs(&input.attrs)?;

    if attrs.untagged {
        expand_untagged_enum(input, data)
    } else if let Some(tag) = attrs.tag {
        expand_tagged_enum(input, data, &tag, attrs.rename_all)
    } else {
        Err(Error::new_spanned(
            &input.ident,
            "enum must have either #[rimu(tag = \"...\")] or #[rimu(untagged)]",
        ))
    }
}

/// Compute the discriminant string that selects this variant, applying
/// variant-level `rename` (highest priority) or container-level `rename_all`.
fn variant_discriminant(
    variant: &Variant,
    rename_all: Option<RenameAll>,
) -> syn::Result<String> {
    let attrs = parse_variant_attrs(&variant.attrs)?;
    if let Some(name) = attrs.rename {
        return Ok(name);
    }
    let raw = variant.ident.to_string();
    Ok(match rename_all {
        Some(rule) => rule.apply(&raw),
        None => raw,
    })
}

fn expand_tagged_enum(
    input: &DeriveInput,
    data: &DataEnum,
    tag: &str,
    rename_all: Option<RenameAll>,
) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let mut arms: Vec<TokenStream2> = Vec::with_capacity(data.variants.len());
    for variant in &data.variants {
        let Fields::Named(fields) = &variant.fields else {
            return Err(Error::new_spanned(
                variant,
                "tagged FromRimu enum variants must have named fields",
            ));
        };
        let variant_ident = &variant.ident;
        let discriminant = variant_discriminant(variant, rename_all)?;
        let body = expand_named_fields(fields, &quote!(Self::#variant_ident))?;
        arms.push(quote! {
            #discriminant => { #body }
        });
    }

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::rimu_interop::FromRimu for #name #ty_generics #where_clause {
            type Error = ::rimu_interop::FromRimuError;

            fn from_rimu(
                __value: ::rimu::Value,
            ) -> ::std::result::Result<Self, Self::Error> {
                let ::rimu::Value::Object(mut __object) = __value else {
                    return ::std::result::Result::Err(
                        ::rimu_interop::FromRimuError::WrongType {
                            expected: "an object",
                            got: ::std::boxed::Box::new(__value),
                        },
                    );
                };

                let __tag_spanned = __object
                    .swap_remove(#tag)
                    .ok_or(::rimu_interop::FromRimuError::MissingDiscriminant { tag: #tag })?;
                let (__tag_inner, __tag_span) = ::rimu::Spanned::take(__tag_spanned);
                let ::rimu::Value::String(__tag) = __tag_inner else {
                    return ::std::result::Result::Err(
                        ::rimu_interop::FromRimuError::DiscriminantNotAString {
                            tag: #tag,
                            span: __tag_span,
                            got: ::std::boxed::Box::new(__tag_inner),
                        },
                    );
                };

                match __tag.as_str() {
                    #(#arms)*
                    __other => ::std::result::Result::Err(
                        ::rimu_interop::FromRimuError::UnknownVariant {
                            tag: #tag,
                            span: __tag_span,
                            value: ::std::string::String::from(__other),
                        },
                    ),
                }
            }
        }
    })
}

fn expand_untagged_enum(input: &DeriveInput, data: &DataEnum) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let mut attempts: Vec<TokenStream2> = Vec::with_capacity(data.variants.len());
    for variant in &data.variants {
        let Fields::Named(fields) = &variant.fields else {
            return Err(Error::new_spanned(
                variant,
                "untagged FromRimu enum variants must have named fields",
            ));
        };
        let variant_ident = &variant.ident;
        let body = expand_named_fields(fields, &quote!(Self::#variant_ident))?;

        // Each attempt clones the outer `Spanned<Value>` so a partial drain by
        // a failed earlier variant doesn't leak into the next one. The clone
        // preserves the *real* outer span so the per-variant top-level error
        // points at the value the user wrote, not a synthetic empty source.
        attempts.push(quote! {
            {
                let __value_clone = ::rimu::Spanned::clone(&__value_spanned);
                let (__value, __value_span) = ::rimu::Spanned::take(__value_clone);
                let __attempt: ::std::result::Result<Self, ::rimu_interop::FromRimuError> = (|| {
                    let ::rimu::Value::Object(mut __object) = __value else {
                        return ::std::result::Result::Err(
                            ::rimu_interop::FromRimuError::WrongType {
                                expected: "an object",
                                got: ::std::boxed::Box::new(__value),
                            },
                        );
                    };
                    #body
                })();
                match __attempt {
                    ::std::result::Result::Ok(__ok) => {
                        return ::std::result::Result::Ok(::rimu::Spanned::new(__ok, __span));
                    }
                    ::std::result::Result::Err(__err) => {
                        __case_errors.push(::rimu::Spanned::new(__err, __value_span));
                    }
                }
            }
        });
    }

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::rimu_interop::FromRimu for #name #ty_generics #where_clause {
            type Error = ::rimu_interop::FromRimuError;

            // Untagged dispatch needs the outer span so per-variant errors
            // can carry the real source location. We override
            // `from_rimu_spanned` to keep that span in scope; the bare
            // `from_rimu` falls back to a synthetic source.
            fn from_rimu_spanned(
                __value_spanned: ::rimu::Spanned<::rimu::Value>,
            ) -> ::std::result::Result<
                ::rimu::Spanned<Self>,
                ::rimu::Spanned<Self::Error>,
            > {
                let __span = ::rimu::Spanned::span(&__value_spanned);
                let mut __case_errors: ::std::vec::Vec<
                    ::rimu::Spanned<::rimu_interop::FromRimuError>,
                > = ::std::vec::Vec::new();
                #(#attempts)*
                ::std::result::Result::Err(::rimu::Spanned::new(
                    ::rimu_interop::FromRimuError::NoVariantMatched {
                        case_errors: __case_errors,
                    },
                    __span,
                ))
            }

            fn from_rimu(
                __value: ::rimu::Value,
            ) -> ::std::result::Result<Self, Self::Error> {
                let __spanned = ::rimu::Spanned::new(
                    __value,
                    ::rimu::Span::new(::rimu::SourceId::empty(), 0, 0),
                );
                Self::from_rimu_spanned(__spanned)
                    .map(::rimu::Spanned::into_inner)
                    .map_err(::rimu::Spanned::into_inner)
            }
        }
    })
}
