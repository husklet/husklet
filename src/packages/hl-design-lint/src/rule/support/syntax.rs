use syn::{Attribute, Item, Type, spanned::Spanned, visit::Visit};

use crate::source::Source;

/// Returns the terminal name of a plain, grouped, or parenthesized type.
pub fn type_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(path) if path.qself.is_none() => path.path.segments.last().map(|segment| segment.ident.to_string()),
        Type::Group(group) => type_name(&group.elem),
        Type::Paren(paren) => type_name(&paren.elem),
        _ => None,
    }
}

/// Reports, per one-based line, whether the line belongs to a `#[cfg(test)]` or `#[test]` item.
///
/// Rules that read a file as text cannot otherwise tell a production line from a test one, and a
/// test is allowed to reach where production is not: a differential test that reads another
/// owner's source to pin the two against each other is the architecture working, not a violation.
pub(crate) fn test_only_lines(source: &Source) -> Vec<bool> {
    let physical = source.text.lines().count();
    let mut lines = vec![false; physical.saturating_add(1)];
    TestOnlyLines {
        physical,
        lines: &mut lines,
    }
    .visit_file(&source.syntax);
    lines
}

struct TestOnlyLines<'a> {
    physical: usize,
    lines: &'a mut [bool],
}

impl<'ast> Visit<'ast> for TestOnlyLines<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        if !test_only(item) {
            syn::visit::visit_item(self, item);
            return;
        }
        let span = item.span();
        let start = item_attributes(item)
            .and_then(<[Attribute]>::first)
            .map_or_else(|| span.start().line, |attribute| attribute.span().start().line)
            .max(1);
        for line in start..=span.end().line.min(self.physical) {
            self.lines[line] = true;
        }
    }
}

/// Reports whether an item is compiled only for tests.
pub(crate) fn test_only(item: &Item) -> bool {
    item_attributes(item).is_some_and(|attributes| {
        crate::source::requires_test(attributes) || attributes.iter().any(|attribute| attribute.path().is_ident("test"))
    })
}

/// Returns an item's attributes, absent for item forms that carry none this linter reads.
pub(crate) fn item_attributes(item: &Item) -> Option<&[Attribute]> {
    Some(match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        _ => return None,
    })
}
