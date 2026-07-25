use std::collections::{BTreeMap, BTreeSet};

use syn::{
    spanned::Spanned, visit::Visit, Attribute, FnArg, ItemImpl, ItemMod, ItemTrait, ReturnType,
    TraitItem, TraitItemFn,
};

use crate::{
    model::{Finding, Related, Review, Severity},
    rule::Rule,
    source::{requires_test, Source, Workspace},
    Result,
};

#[cfg(test)]
mod tests;

const MINIMUM_METHODS: usize = 8;
const MINIMUM_CLUSTERS: usize = 3;
const MINIMUM_METHODS_PER_CLUSTER: usize = 2;

/// Reviews large traits whose methods provide several unrelated capabilities.
pub struct BroadTrait;

impl Rule for BroadTrait {
    fn id(&self) -> &'static str {
        "broad-trait-responsibilities"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut definitions = Vec::new();
        let mut implementations = BTreeMap::<(String, String), Vec<Implementation>>::new();

        for source in workspace.production() {
            let mut collector = Collector {
                source,
                modules: Vec::new(),
                test_scope: false,
                definitions: Vec::new(),
                implementations: Vec::new(),
            };
            collector.visit_file(&source.syntax);
            definitions.extend(collector.definitions);
            for implementation in collector.implementations {
                implementations
                    .entry((source.package.clone(), implementation.trait_name.clone()))
                    .or_default()
                    .push(implementation);
            }
        }

        Ok(definitions
            .into_iter()
            .filter_map(|definition| {
                let implementation = implementations
                    .get(&(definition.package.clone(), definition.name.clone()))
                    .cloned()
                    .unwrap_or_default();
                definition.finding(self.id(), implementation)
            })
            .collect())
    }
}

#[derive(Clone)]
struct Method {
    name: String,
    signature: String,
    types: BTreeSet<String>,
    location: crate::Location,
    cluster: Option<&'static str>,
}

struct Definition {
    package: String,
    name: String,
    location: crate::Location,
    methods: Vec<Method>,
    weak_name_evidence: bool,
}

#[derive(Clone)]
struct Implementation {
    trait_name: String,
    owner: String,
    location: crate::Location,
}

impl Definition {
    fn finding(self, rule: &'static str, implementations: Vec<Implementation>) -> Option<Finding> {
        if self.methods.len() < MINIMUM_METHODS {
            return None;
        }

        let mut clusters = BTreeMap::<&'static str, Vec<&Method>>::new();
        for method in &self.methods {
            if let Some(cluster) = method.cluster {
                clusters.entry(cluster).or_default().push(method);
            }
        }
        clusters.retain(|_, methods| methods.len() >= MINIMUM_METHODS_PER_CLUSTER);
        if clusters.len() < MINIMUM_CLUSTERS {
            return None;
        }

        let clustered = clusters.values().map(Vec::len).sum::<usize>();
        if clustered * 2 < self.methods.len()
            || cohesive_contract(&self.name, &self.methods)
            || separation_evidence(&clusters) < MINIMUM_CLUSTERS - 1
        {
            return None;
        }

        let summary = clusters
            .iter()
            .map(|(cluster, methods)| {
                format!(
                    "{cluster}: {}",
                    methods
                        .iter()
                        .map(|method| method.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let signatures = clusters
            .iter()
            .map(|(cluster, methods)| {
                format!(
                    "{cluster}: {}",
                    signature_profile(methods)
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");

        let mut finding = Finding::warning(rule, self.name.clone(), self.location);
        finding.message = format!(
            "`{}` has {} methods spanning {} distinct capability clusters",
            self.name,
            self.methods.len(),
            clusters.len()
        );
        finding.help = "split the contract into the smallest real capabilities implemented and consumed independently; retain a composition trait only when callers require the complete set".into();
        finding.related = self
            .methods
            .iter()
            .map(|method| Related {
                label: format!("method `{}`: {}", method.name, method.signature),
                location: method.location.clone(),
            })
            .chain(implementations.iter().map(|implementation| Related {
                label: format!("implemented by `{}`", implementation.owner),
                location: implementation.location.clone(),
            }))
            .collect();

        let mut review = Review::error();
        review
            .metadata
            .push(("Method count".into(), self.methods.len().to_string()));
        review
            .metadata
            .push(("Capability clusters".into(), summary));
        review
            .metadata
            .push(("Signature type evidence".into(), signatures));
        review.metadata.push((
            "Implementors".into(),
            implementations
                .iter()
                .map(|implementation| implementation.owner.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(", "),
        ));
        if self.weak_name_evidence {
            review.metadata.push((
                "Weak naming evidence".into(),
                "trait name uses Repository, Manager, or Service".into(),
            ));
        }
        review.questions.push(
            "Do consumers need every cluster, or can they depend on smaller capability traits?"
                .into(),
        );
        review.questions.push(
            "Are these operations one cohesive protocol/state machine despite their different verbs and types?"
                .into(),
        );
        finding.review = Some(review);
        Some(finding)
    }
}

struct Collector<'a> {
    source: &'a Source,
    modules: Vec<String>,
    test_scope: bool,
    definitions: Vec<Definition>,
    implementations: Vec<Implementation>,
}

impl Visit<'_> for Collector<'_> {
    fn visit_item_mod(&mut self, item: &ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= requires_test(&item.attrs);
        if let Some((_, items)) = &item.content {
            self.modules.push(item.ident.to_string());
            for item in items {
                self.visit_item(item);
            }
            self.modules.pop();
        }
        self.test_scope = previous;
    }

    fn visit_item_trait(&mut self, item: &ItemTrait) {
        if self.test_scope || requires_test(&item.attrs) || excluded_trait(item, self.source) {
            return;
        }
        let methods = item
            .items
            .iter()
            .filter_map(|item| match item {
                TraitItem::Fn(item) => Some(method(item, self.source)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let name = item.ident.to_string();
        self.definitions.push(Definition {
            package: self.source.package.clone(),
            weak_name_evidence: weak_name(&name),
            name,
            location: self.source.location(item.span()),
            methods,
        });
    }

    fn visit_item_impl(&mut self, item: &ItemImpl) {
        if self.test_scope || requires_test(&item.attrs) {
            return;
        }
        let Some((_, path, _)) = &item.trait_ else {
            return;
        };
        let Some(trait_name) = path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
        else {
            return;
        };
        self.implementations.push(Implementation {
            trait_name,
            owner: normalized(self.source, item.self_ty.span()),
            location: self.source.location(item.span()),
        });
    }
}

fn method(method: &TraitItemFn, source: &Source) -> Method {
    let name = method.sig.ident.to_string();
    let mut types = method
        .sig
        .inputs
        .iter()
        .filter_map(|argument| match argument {
            FnArg::Receiver(_) => None,
            FnArg::Typed(argument) => Some(type_words(source, argument.ty.span())),
        })
        .flatten()
        .collect::<BTreeSet<_>>();
    if let ReturnType::Type(_, output) = &method.sig.output {
        types.extend(type_words(source, output.span()));
    }
    Method {
        cluster: capability(&name),
        signature: normalized(source, method.sig.span()),
        types,
        location: source.location(method.span()),
        name,
    }
}

fn normalized(source: &Source, span: proc_macro2::Span) -> String {
    source
        .excerpt(span)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn excluded_trait(item: &ItemTrait, source: &Source) -> bool {
    item.unsafety.is_some()
        || generated(&item.attrs, &source.text)
        || item.supertraits.iter().any(|bound| {
            let bound = normalized(source, bound.span());
            bound == "Sealed" || bound.ends_with("::Sealed")
        })
}

fn generated(attributes: &[Attribute], source: &str) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("automatically_derived")
            || attribute.path().is_ident("proc_macro_derive")
    }) || source
        .lines()
        .take(8)
        .any(|line| line.contains("@generated") || line.contains("automatically generated"))
}

fn weak_name(name: &str) -> bool {
    ["Repository", "Manager", "Service"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

fn capability(name: &str) -> Option<&'static str> {
    let words = words(name).collect::<Vec<_>>();
    for noun in words.iter().skip(1) {
        match noun.as_str() {
            "event" | "events" | "notification" | "notifications" => return Some("events"),
            "clipboard" => return Some("clipboard"),
            "window" | "windows" | "surface" | "interaction" => return Some("window"),
            "auth" | "permission" | "permissions" | "credential" | "credentials" => {
                return Some("authorization");
            }
            "metric" | "metrics" | "stat" | "stats" | "status" | "health" => {
                return Some("observation");
            }
            _ => {}
        }
    }
    let verb = words.first()?;
    match verb.as_str() {
        "create" | "open" | "read" | "write" | "save" | "load" | "delete" | "remove" | "list"
        | "find" | "get" | "put" => Some("persistence"),
        "start" | "stop" | "pause" | "resume" | "restart" | "kill" | "launch" | "terminate" => {
            Some("lifecycle")
        }
        "inspect" | "status" | "stats" | "health" | "metrics" | "describe" | "query" => {
            Some("observation")
        }
        "configure" | "set" | "update" | "apply" | "reset" | "enable" | "disable" => {
            Some("configuration")
        }
        "subscribe" | "unsubscribe" | "watch" | "emit" | "notify" | "poll" => Some("events"),
        "upload" | "download" | "push" | "pull" | "import" | "export" | "copy" => Some("transfer"),
        "login" | "logout" | "authenticate" | "authorize" | "grant" | "revoke" => {
            Some("authorization")
        }
        "connect" | "disconnect" | "bind" | "listen" | "accept" | "send" | "receive" => {
            Some("connection")
        }
        "render" | "draw" | "present" | "commit" | "frame" | "paint" => Some("rendering"),
        "visit" | "walk" | "fold" | "traverse" => Some("traversal"),
        "encode" | "decode" | "serialize" | "deserialize" | "parse" | "format" => Some("codec"),
        _ => None,
    }
}

fn separation_evidence(clusters: &BTreeMap<&'static str, Vec<&Method>>) -> usize {
    let profiles = clusters
        .values()
        .map(|methods| {
            (
                signature_profile(methods),
                methods
                    .iter()
                    .flat_map(|method| words(&method.name).skip(1))
                    .collect::<BTreeSet<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let mut separated = BTreeSet::new();
    for left in 0..profiles.len() {
        for right in left + 1..profiles.len() {
            let type_difference = profiles[left].0 != profiles[right].0;
            let noun_difference = !profiles[left].1.is_empty()
                && !profiles[right].1.is_empty()
                && profiles[left].1.is_disjoint(&profiles[right].1);
            if type_difference || noun_difference {
                separated.insert(left);
                separated.insert(right);
            }
        }
    }
    separated.len()
}

fn signature_profile(methods: &[&Method]) -> BTreeSet<String> {
    methods
        .iter()
        .flat_map(|method| signature_types(method))
        .collect()
}

fn signature_types(method: &Method) -> BTreeSet<String> {
    method
        .types
        .iter()
        .filter(|word| {
            !matches!(
                word.as_str(),
                "result"
                    | "option"
                    | "vec"
                    | "box"
                    | "arc"
                    | "dyn"
                    | "impl"
                    | "where"
                    | "send"
                    | "sync"
                    | "static"
                    | "error"
                    | "bool"
                    | "str"
                    | "string"
                    | "usize"
                    | "isize"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "i128"
            )
        })
        .cloned()
        .collect()
}

fn type_words(source: &Source, span: proc_macro2::Span) -> impl Iterator<Item = String> {
    words(&source.excerpt(span))
}

fn cohesive_contract(name: &str, methods: &[Method]) -> bool {
    if !["Protocol", "Codec", "Visitor", "Renderer", "Commands"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return false;
    }
    let mut occurrences = BTreeMap::<String, usize>::new();
    for method in methods {
        for ty in signature_types(method) {
            *occurrences.entry(ty).or_default() += 1;
        }
    }
    occurrences
        .values()
        .any(|occurrences| occurrences * 4 >= methods.len() * 3)
}

fn words(name: &str) -> impl Iterator<Item = String> {
    let mut output = Vec::new();
    let mut current = String::new();
    for character in name.chars() {
        if !character.is_ascii_alphanumeric() {
            if !current.is_empty() {
                output.push(std::mem::take(&mut current));
            }
            continue;
        }
        if character.is_ascii_uppercase()
            && current
                .chars()
                .last()
                .is_some_and(|previous| previous.is_ascii_lowercase())
        {
            output.push(std::mem::take(&mut current));
        }
        current.push(character.to_ascii_lowercase());
    }
    if !current.is_empty() {
        output.push(current);
    }
    output.into_iter()
}
