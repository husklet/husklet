use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use tree_sitter::Node;

use super::{parse, source_files};
use crate::{
    Finding, LintError, Location, Related, Result, Severity, policy::CTestOnlyStatePolicy, rule::Rule,
    source::Workspace,
};

const RULE: &str = "c-test-only-state";

/// Rejects production predicates that read state only a test-only writer sets.
///
/// A production build compiles neither the body of `#if defined(HL_NATIVE_TEST_HOOKS)`
/// nor anything reachable only from it. State assigned exclusively there therefore
/// holds its initial value forever in the shipped engine, while every test build
/// observes the assigned value. A production branch keyed on that state is green in
/// the entire suite and wrong in every shipped run.
pub struct TestOnlyState {
    policy: CTestOnlyStatePolicy,
}

impl TestOnlyState {
    /// Creates the rule from the conditional-compilation macros that mark test-only code.
    #[must_use]
    pub fn new(policy: CTestOnlyStatePolicy) -> Self {
        Self { policy }
    }
}

impl Rule for TestOnlyState {
    fn id(&self) -> &'static str {
        RULE
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        if self.policy.macros.is_empty() {
            return Ok(Vec::new());
        }
        let macros = self.policy.macros.iter().cloned().collect::<BTreeSet<_>>();
        let mut corpus = Corpus::default();
        for path in source_files(workspace)? {
            let source = fs::read_to_string(&path).map_err(|error| LintError::io("read", &path, error))?;
            let tree = parse(&path, &source)?;
            let mut file = FileScan {
                path: &path,
                source: source.as_bytes(),
                macros: &macros,
                corpus: &mut corpus,
            };
            file.walk(tree.root_node(), &Context::default());
        }
        Ok(corpus.findings())
    }
}

/// One source position recorded during the scan.
#[derive(Clone, Debug)]
struct Site {
    path: PathBuf,
    line: usize,
    column: usize,
    excerpt: String,
    /// Function containing the site, absent at file scope.
    function: Option<String>,
    /// The site itself sits inside test-only conditional compilation.
    test_only: bool,
}

impl Site {
    fn location(&self) -> Location {
        Location {
            path: self.path.clone(),
            line: self.line,
            column: self.column,
            source: self.excerpt.clone(),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Context<'a> {
    test_only: bool,
    predicate: bool,
    function: Option<&'a str>,
}

#[derive(Default)]
struct Corpus {
    /// Functions defined only inside test-only conditional compilation.
    test_only_definitions: BTreeSet<String>,
    /// Functions with at least one definition a production build compiles.
    production_definitions: BTreeSet<String>,
    /// Every definition site of a function name, used to skip undefined externals.
    defined: BTreeSet<String>,
    /// Call sites keyed by callee.
    calls: BTreeMap<String, Vec<Site>>,
    /// Assignments to file-scope state, keyed by the assigned name.
    writes: BTreeMap<String, Vec<Site>>,
    /// Predicate reads of file-scope state outside test-only compilation.
    predicate_reads: BTreeMap<String, Vec<Site>>,
    /// Names declared at file scope, the only names this rule tracks.
    state: BTreeSet<String>,
}

impl Corpus {
    /// Returns the functions no production call site can reach.
    ///
    /// A function is test-only when every one of its call sites is test-only, and a
    /// call site is test-only when the conditional compilation around it is, or when
    /// the calling function is itself unreachable from production. Functions with no
    /// call site in the corpus are entry points called across the FFI boundary and
    /// are treated as production.
    fn test_only_functions(&self) -> BTreeSet<String> {
        let mut unreachable = self
            .test_only_definitions
            .difference(&self.production_definitions)
            .cloned()
            .collect::<BTreeSet<_>>();
        loop {
            let mut grown = false;
            for name in &self.defined {
                if unreachable.contains(name) {
                    continue;
                }
                let Some(sites) = self.calls.get(name) else {
                    continue;
                };
                if sites.iter().all(|site| Self::site_is_test_only(site, &unreachable)) {
                    unreachable.insert(name.clone());
                    grown = true;
                }
            }
            if !grown {
                return unreachable;
            }
        }
    }

    fn site_is_test_only(site: &Site, unreachable: &BTreeSet<String>) -> bool {
        site.test_only
            || site
                .function
                .as_ref()
                .is_some_and(|function| unreachable.contains(function))
    }

    fn findings(&self) -> Vec<Finding> {
        let unreachable = self.test_only_functions();
        let mut findings = Vec::new();
        for (name, reads) in &self.predicate_reads {
            if !self.state.contains(name) {
                continue;
            }
            let Some(writes) = self.writes.get(name) else {
                continue;
            };
            if !writes.iter().all(|site| Self::site_is_test_only(site, &unreachable)) {
                continue;
            }
            for read in reads {
                findings.push(finding(name, read, writes));
            }
        }
        findings.sort_by(|left, right| {
            (&left.location.path, left.location.line, &left.subject).cmp(&(
                &right.location.path,
                right.location.line,
                &right.subject,
            ))
        });
        findings
    }
}

fn finding(name: &str, read: &Site, writes: &[Site]) -> Finding {
    let mut finding = Finding::error(RULE, name, read.location());
    finding.message = format!("production predicate reads `{name}`, which only test-only code writes");
    finding.help = format!(
        "give `{name}` a writer on the production path before any production branch depends on it, \
         or move this predicate behind the same conditional compilation as its writer"
    );
    finding.related = writes
        .iter()
        .map(|write| Related {
            label: match &write.function {
                Some(function) => format!("test-only write in `{function}`"),
                None => "test-only write".to_owned(),
            },
            location: write.location(),
        })
        .collect();
    finding
}

struct FileScan<'a> {
    path: &'a Path,
    source: &'a [u8],
    macros: &'a BTreeSet<String>,
    corpus: &'a mut Corpus,
}

impl FileScan<'_> {
    fn text(&self, node: Node<'_>) -> String {
        node.utf8_text(self.source).unwrap_or_default().to_owned()
    }

    fn site(&self, node: Node<'_>, context: &Context<'_>) -> Site {
        let point = node.start_position();
        Site {
            path: self.path.to_owned(),
            line: point.row + 1,
            column: point.column + 1,
            excerpt: self.text(node),
            function: context.function.map(ToOwned::to_owned),
            test_only: context.test_only,
        }
    }

    fn walk(&mut self, node: Node<'_>, context: &Context<'_>) {
        match node.kind() {
            "preproc_if" | "preproc_ifdef" => {
                self.walk_conditional(node, context);
                return;
            }
            "function_definition" => {
                self.walk_definition(node, context);
                return;
            }
            "declaration" if context.function.is_none() => {
                self.record_file_scope_declaration(node, context);
            }
            "assignment_expression" => {
                self.record_assignment(node, context);
            }
            "update_expression" => {
                if let Some(argument) = node.child_by_field_name("argument")
                    && argument.kind() == "identifier"
                {
                    let name = self.text(argument);
                    let site = self.site(argument, context);
                    self.corpus.writes.entry(name).or_default().push(site);
                }
            }
            "call_expression" => {
                if let Some(function) = node.child_by_field_name("function")
                    && function.kind() == "identifier"
                {
                    let name = self.text(function);
                    let site = self.site(function, context);
                    self.corpus.calls.entry(name).or_default().push(site);
                }
            }
            "identifier" if context.predicate && !context.test_only => {
                let name = self.text(node);
                let site = self.site(node, context);
                self.corpus.predicate_reads.entry(name).or_default().push(site);
            }
            _ => {}
        }
        let inherited = Context {
            predicate: context.predicate || is_predicate(node),
            ..*context
        };
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_context = Context {
                predicate: inherited.predicate || condition_field(node, child),
                ..inherited
            };
            self.walk(child, &child_context);
        }
    }

    fn record_assignment(&mut self, node: Node<'_>, context: &Context<'_>) {
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        if left.kind() != "identifier" {
            return;
        }
        let name = self.text(left);
        let site = self.site(left, context);
        self.corpus.writes.entry(name).or_default().push(site);
    }

    fn walk_conditional(&mut self, node: Node<'_>, context: &Context<'_>) {
        let guarded = context.test_only || self.names_test_macro(node);
        let alternative = node.child_by_field_name("alternative");
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_context = if Some(child) == alternative {
                *context
            } else {
                Context {
                    test_only: guarded,
                    ..*context
                }
            };
            self.walk(child, &child_context);
        }
    }

    /// Reports whether the directive selects code on a test-only macro being defined.
    ///
    /// `#ifndef` is deliberately not treated as test-only: its body is the branch a
    /// production build compiles.
    fn names_test_macro(&self, node: Node<'_>) -> bool {
        if node.kind() == "preproc_ifdef" {
            let directive = node.child(0).map(|child| self.text(child)).unwrap_or_default();
            return directive == "#ifdef"
                && node
                    .child_by_field_name("name")
                    .is_some_and(|name| self.macros.contains(&self.text(name)));
        }
        node.child_by_field_name("condition")
            .is_some_and(|condition| self.condition_requires_test_macro(condition))
    }

    /// Reports whether the condition cannot hold unless a test-only macro is defined.
    ///
    /// Only conjunction is traversed. A disjunction is satisfiable without the macro,
    /// so its body is not test-only.
    fn condition_requires_test_macro(&self, node: Node<'_>) -> bool {
        match node.kind() {
            "parenthesized_expression" => node
                .named_child(0)
                .is_some_and(|inner| self.condition_requires_test_macro(inner)),
            "preproc_defined" => node
                .named_child(0)
                .is_some_and(|name| self.macros.contains(&self.text(name))),
            "identifier" => self.macros.contains(&self.text(node)),
            "binary_expression" => {
                let operator = node
                    .child_by_field_name("operator")
                    .map(|operator| self.text(operator))
                    .unwrap_or_default();
                operator == "&&"
                    && [node.child_by_field_name("left"), node.child_by_field_name("right")]
                        .into_iter()
                        .flatten()
                        .any(|side| self.condition_requires_test_macro(side))
            }
            _ => false,
        }
    }

    fn walk_definition(&mut self, node: Node<'_>, context: &Context<'_>) {
        let Some(name) = node
            .child_by_field_name("declarator")
            .and_then(declared_identifier)
            .map(|identifier| self.text(identifier))
        else {
            return;
        };
        self.corpus.defined.insert(name.clone());
        if context.test_only {
            self.corpus.test_only_definitions.insert(name.clone());
        } else {
            self.corpus.production_definitions.insert(name.clone());
        }
        let body_context = Context {
            function: Some(&name),
            predicate: false,
            test_only: context.test_only,
        };
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk(child, &body_context);
        }
    }

    /// Records a file-scope object and any initializer that establishes real state.
    ///
    /// A zero initializer is the absence of a writer, not a production one: it is the
    /// value the shipped build is stuck with when every assignment is test-only.
    fn record_file_scope_declaration(&mut self, node: Node<'_>, context: &Context<'_>) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "identifier" | "pointer_declarator" | "array_declarator" => {
                    if let Some(identifier) = declared_identifier(child)
                        && !is_function_declarator(child)
                    {
                        self.corpus.state.insert(self.text(identifier));
                    }
                }
                "init_declarator" => {
                    let Some(identifier) = child.child_by_field_name("declarator").and_then(declared_identifier) else {
                        continue;
                    };
                    let name = self.text(identifier);
                    self.corpus.state.insert(name.clone());
                    let initializer = child.child_by_field_name("value");
                    if initializer.is_some_and(|value| !self.is_zero(value)) {
                        let site = self.site(identifier, context);
                        self.corpus.writes.entry(name).or_default().push(site);
                    }
                }
                _ => {}
            }
        }
    }

    fn is_zero(&self, node: Node<'_>) -> bool {
        matches!(
            self.text(node).trim(),
            "0" | "0u" | "0U" | "NULL" | "false" | "{0}" | "{}"
        )
    }
}

fn is_function_declarator(node: Node<'_>) -> bool {
    node.kind() == "function_declarator"
        || node
            .child_by_field_name("declarator")
            .is_some_and(is_function_declarator)
}

fn declared_identifier(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "identifier" => Some(node),
        _ => node.child_by_field_name("declarator").and_then(declared_identifier),
    }
}

/// Reports whether the node is itself a test of a value.
fn is_predicate(node: Node<'_>) -> bool {
    match node.kind() {
        "unary_expression" => node
            .child_by_field_name("operator")
            .is_some_and(|operator| operator.kind() == "!"),
        "binary_expression" => node
            .child_by_field_name("operator")
            .is_some_and(|operator| matches!(operator.kind(), "==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||")),
        _ => false,
    }
}

/// Reports whether the child occupies the condition position of its parent.
fn condition_field(parent: Node<'_>, child: Node<'_>) -> bool {
    matches!(
        parent.kind(),
        "if_statement" | "while_statement" | "do_statement" | "for_statement" | "conditional_expression"
    ) && parent.child_by_field_name("condition") == Some(child)
}

#[cfg(test)]
#[path = "hook_test.rs"]
mod test;
