//! Temporary design classifications consumed by `hl-design-lint`.

use proc_macro::TokenStream;
use syn::{
    ext::IdentExt,
    parse::{Parse, ParseStream},
    parse_macro_input, Error, Ident, LitStr, Result, Token,
};

struct NamingException {
    reason: LitStr,
}

impl Parse for NamingException {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let key = Ident::parse_any(input)?;
        if key != "reason" {
            return Err(Error::new(key.span(), "expected `reason = \"...\"`"));
        }
        input.parse::<Token![=]>()?;
        let reason: LitStr = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("unexpected naming exception argument"));
        }
        if reason.value().trim().is_empty() {
            return Err(Error::new(
                reason.span(),
                "naming exception reason cannot be empty",
            ));
        }
        Ok(Self { reason })
    }
}

struct Classification {
    scope: Ident,
    value: Option<(Token![=], Kind)>,
}

enum Kind {
    Ident(Ident),
    String(LitStr),
}

impl Parse for Classification {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self {
            scope: Ident::parse_any(input)?,
            value: if input.is_empty() {
                None
            } else {
                let equals = input.parse()?;
                let kind = if input.peek(LitStr) {
                    Kind::String(input.parse()?)
                } else {
                    Kind::Ident(Ident::parse_any(input)?)
                };
                Some((equals, kind))
            },
        })
    }
}

/// Classifies a function whose final architectural owner is not mature yet.
#[proc_macro_attribute]
pub fn classify(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let classification = parse_macro_input!(attribute as Classification);
    let scope = classification.scope.to_string();
    if !matches!(scope.as_str(), "root" | "domain" | "pkg" | "struct") {
        return Error::new(
            classification.scope.span(),
            "scope must be root, domain, pkg, or struct",
        )
        .into_compile_error()
        .into();
    }
    if scope == "pkg" {
        if classification.value.is_some() {
            return Error::new(
                classification.scope.span(),
                "pkg is derived from Cargo.toml; use #[hl_design::classify(pkg)]",
            )
            .into_compile_error()
            .into();
        }
        return item;
    }
    let Some((_, kind)) = classification.value else {
        return Error::new(
            classification.scope.span(),
            "root, domain, and struct require a classification kind",
        )
        .into_compile_error()
        .into();
    };
    let kind = match kind {
        Kind::Ident(kind) => kind.to_string(),
        Kind::String(kind) => kind.value(),
    };
    if kind.trim().is_empty() {
        return Error::new(
            classification.scope.span(),
            "classification kind cannot be empty",
        )
        .into_compile_error()
        .into();
    }
    item
}

/// Documents a reviewed exception to the design naming rule.
#[proc_macro_attribute]
pub fn naming(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let exception = parse_macro_input!(attribute as NamingException);
    let _ = exception.reason;
    item
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse::Parser;

    fn parse(source: &str) -> Result<NamingException> {
        NamingException::parse.parse_str(source)
    }

    #[test]
    fn accepts_reasoned_naming_exception() {
        assert_eq!(
            parse("reason = \"external protocol term\"")
                .unwrap()
                .reason
                .value(),
            "external protocol term"
        );
    }

    #[test]
    fn rejects_empty_missing_and_unknown_reasons() {
        assert!(parse("reason = \"\"").is_err());
        assert!(parse("").is_err());
        assert!(parse("because = \"legacy\"").is_err());
        assert!(parse("reason = \"legacy\", extra = \"value\"").is_err());
    }
}
