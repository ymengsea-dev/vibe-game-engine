//! Derive macros for the VGE editor.
//!
//! [`macro@Inspectable`] generates an `Inspectable` impl for a
//! named-field struct: one labelled row per field, drawn with that
//! field type's `InspectField` implementation, returning whether the
//! user changed anything. It replaces the hand-written per-field egui
//! code the inspector panel used to carry.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitFloat, LitStr, parse_macro_input};

/// Derives `engine_editor::inspect::Inspectable` for a struct with named
/// fields.
///
/// Each field is drawn as a labelled row via
/// `engine_editor::inspect::InspectField`. Per-field attributes:
///
/// - `#[inspect(skip)]` — leave the field out of the panel.
/// - `#[inspect(label = "Text")]` — override the row label (default: the
///   field name).
/// - `#[inspect(speed = 0.25)]` — drag sensitivity passed to
///   `InspectField::inspect_field` (default: `0.1`).
#[proc_macro_derive(Inspectable, attributes(inspect))]
pub fn derive_inspectable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let Data::Struct(data) = &input.data else {
        return syn::Error::new_spanned(
            &input.ident,
            "Inspectable can only be derived for structs",
        )
        .to_compile_error()
        .into();
    };
    let Fields::Named(fields) = &data.fields else {
        return syn::Error::new_spanned(
            &input.ident,
            "Inspectable can only be derived for structs with named fields",
        )
        .to_compile_error()
        .into();
    };

    let mut rows = Vec::new();
    for field in &fields.named {
        // `Fields::Named` guarantees every field has an ident; skip
        // defensively rather than unwrap.
        let Some(ident) = field.ident.as_ref() else {
            continue;
        };

        let mut skip = false;
        let mut label = ident.to_string();
        let mut speed: f32 = 0.1;
        let mut attr_error: Option<syn::Error> = None;

        for attr in &field.attrs {
            if !attr.path().is_ident("inspect") {
                continue;
            }
            let result = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("skip") {
                    skip = true;
                    Ok(())
                } else if meta.path.is_ident("label") {
                    let value: LitStr = meta.value()?.parse()?;
                    label = value.value();
                    Ok(())
                } else if meta.path.is_ident("speed") {
                    let value: LitFloat = meta.value()?.parse()?;
                    speed = value.base10_parse()?;
                    Ok(())
                } else {
                    Err(meta.error("unknown `inspect` attribute (expected skip, label, or speed)"))
                }
            });
            if let Err(err) = result {
                attr_error = Some(err);
            }
        }

        if let Some(err) = attr_error {
            return err.to_compile_error().into();
        }
        if skip {
            continue;
        }

        rows.push(quote! {
            ui.horizontal(|ui| {
                ui.label(#label);
                changed |= ::engine_editor::inspect::InspectField::inspect_field(
                    &mut self.#ident,
                    ui,
                    #speed,
                );
            });
        });
    }

    quote! {
        impl ::engine_editor::inspect::Inspectable for #name {
            fn inspect(&mut self, ui: &mut ::egui::Ui) -> bool {
                let mut changed = false;
                #(#rows)*
                changed
            }
        }
    }
    .into()
}
