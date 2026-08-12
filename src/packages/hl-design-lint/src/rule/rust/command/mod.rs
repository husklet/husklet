use std::collections::HashMap;

use syn::{
    Block, Expr, ExprCall, ExprMacro, ExprMethodCall, ImplItemFn, ItemFn, ItemMod, ItemUse, Local, Pat, UseTree,
    spanned::Spanned, visit::Visit,
};

use crate::{
    Result,
    model::{Finding, Related, Severity},
    rule::Rule,
    source::{Source, Workspace, requires_test},
};

/// Prevents host process execution from leaking outside platform boundaries.
#[derive(Default)]
pub struct PlatformCommand {
    policy: crate::policy::BoundaryPolicy,
}

impl PlatformCommand {
    /// Creates the rule with repository-provided boundary selectors.
    #[must_use]
    pub fn new(policy: crate::policy::BoundaryPolicy) -> Self {
        Self { policy }
    }
}

impl Rule for PlatformCommand {
    fn id(&self) -> &'static str {
        "platform-command-boundary"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.sources() {
            let mut visitor = Commands::new(source, &self.policy);
            visitor.visit_file(&source.syntax);
            findings.extend(visitor.findings);
        }
        Ok(findings)
    }
}

struct Commands<'a> {
    source: &'a Source,
    aliases: HashMap<String, Vec<String>>,
    modules: Vec<String>,
    test_depth: usize,
    staged: HashMap<String, Staged>,
    findings: Vec<Finding>,
    policy: &'a crate::policy::BoundaryPolicy,
}

impl<'a> Commands<'a> {
    fn new(source: &'a Source, policy: &'a crate::policy::BoundaryPolicy) -> Self {
        Self {
            source,
            aliases: HashMap::new(),
            modules: filesystem_modules(source),
            test_depth: 0,
            staged: HashMap::new(),
            findings: Vec::new(),
            policy,
        }
    }

    fn boundary(&self) -> bool {
        self.source.test
            || self.test_depth > 0
            || self.source.path.file_name().is_some_and(|name| name == "build.rs")
            || self
                .modules
                .iter()
                .any(|module| matches!(module.as_str(), "adapter" | "adapters" | "platform" | "host"))
            || self.policy.allow.iter().any(|selector| {
                selector.matches(
                    &self.source.package,
                    &self.source.domain,
                    &self.source.path,
                    &self.modules,
                )
            })
    }

    fn program_path(&self, call: &ExprCall) -> Option<String> {
        let Expr::Path(function) = call.func.as_ref() else {
            return None;
        };
        let mut path = function
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        if path.last().is_none_or(|segment| segment != "new") {
            return None;
        }
        path.pop();
        if let Some(prefix) = path.first().and_then(|first| self.aliases.get(first)) {
            path.splice(0..1, prefix.clone());
        }
        (path == ["std", "process", "Command"] || path == ["tokio", "process", "Command"]).then(|| path.join("::"))
    }

    // The finding owns the subject it reports.
    #[allow(clippy::needless_pass_by_value)]
    fn report_boundary(&mut self, call: &ExprCall, command: String) {
        if self.boundary() {
            return;
        }
        let mut finding = Finding::error(
            "platform-command-boundary",
            command.clone(),
            self.source.location(call.span()),
        );
        finding.message = format!(
            "host process construction `{command}::new` is outside an application composition or platform-adapter boundary"
        );
        finding.help = "define a domain-owned capability and move executable discovery, flags, and process lifecycle into an explicit adapter; keep guest process specifications as typed data".to_owned();
        finding.related.push(Related {
            label: format!(
                "resolved crate `{}`, module `{}`",
                self.source.package,
                self.modules.join("::")
            ),
            location: self.source.location(call.span()),
        });
        self.findings.push(finding);
    }

    fn report_shell(&mut self, expression: &ExprMethodCall) {
        let Some(shell) = shell_chain(expression, self) else {
            return;
        };
        let mut finding = Finding::error(
            "platform-command-boundary",
            format!("{shell} interpolated script"),
            self.source.location(expression.span()),
        );
        finding.message = format!(
            "shell `{shell}` receives a dynamically constructed program or script, allowing data to be interpreted as command syntax"
        );
        finding.help = "invoke the target executable directly with separately parameterized arguments, or use a native/typed API; never interpolate data into shell or interpreter source".to_owned();
        finding.related.push(Related {
            label: "dynamic shell invocation is unsafe even in tests and build scripts".to_owned(),
            location: self.source.location(expression.span()),
        });
        self.findings.push(finding);
    }

    fn inspect_staged(&mut self, expression: &ExprMethodCall) {
        let Expr::Path(receiver) = expression.receiver.as_ref() else {
            return;
        };
        let Some(name) = receiver.path.get_ident().map(ToString::to_string) else {
            return;
        };
        let Some(mut command) = self.staged.get(&name).cloned() else {
            return;
        };
        let unsafe_script = if expression.method == "arg" && expression.args.len() == 1 {
            let argument = &expression.args[0];
            let unsafe_script = command.armed && dynamic(argument);
            command.armed =
                !command.armed && string_literal(argument).is_some_and(|flag| matches!(flag.as_str(), "-c" | "-e"));
            unsafe_script
        } else if expression.method == "args" && expression.args.len() == 1 {
            let Expr::Array(arguments) = &expression.args[0] else {
                return;
            };
            let mut arguments = arguments.elems.iter();
            let shell_flag = arguments
                .next()
                .and_then(string_literal)
                .is_some_and(|flag| matches!(flag.as_str(), "-c" | "-e"));
            command.armed = false;
            shell_flag && arguments.next().is_some_and(dynamic)
        } else {
            false
        };
        self.staged.insert(name, command.clone());
        if unsafe_script {
            let shell = command.shell;
            let mut finding = Finding::error(
                "platform-command-boundary",
                format!("{shell} interpolated script"),
                self.source.location(expression.span()),
            );
            finding.message = format!(
                "staged shell `{shell}` receives a dynamically constructed program or script, allowing data to be interpreted as command syntax"
            );
            finding.help = "invoke the target executable directly with separately parameterized arguments, or use a native/typed API; never interpolate data into shell or interpreter source".to_owned();
            finding.related.push(Related {
                label: "resolved through a command binding in the same lexical scope".to_owned(),
                location: self.source.location(expression.span()),
            });
            self.findings.push(finding);
        }
    }
}

#[derive(Clone)]
struct Staged {
    shell: String,
    armed: bool,
}

impl<'ast> Visit<'ast> for Commands<'_> {
    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        collect_use(&item.tree, Vec::new(), &mut self.aliases);
        syn::visit::visit_item_use(self, item);
    }

    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        let aliases = self.aliases.clone();
        self.modules.push(module.ident.to_string());
        let test = requires_test(&module.attrs);
        self.test_depth += usize::from(test);
        syn::visit::visit_item_mod(self, module);
        self.test_depth -= usize::from(test);
        self.modules.pop();
        self.aliases = aliases;
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        let aliases = self.aliases.clone();
        let staged = std::mem::take(&mut self.staged);
        let test =
            requires_test(&function.attrs) || function.attrs.iter().any(|attribute| attribute.path().is_ident("test"));
        self.test_depth += usize::from(test);
        syn::visit::visit_item_fn(self, function);
        self.test_depth -= usize::from(test);
        self.aliases = aliases;
        self.staged = staged;
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        let aliases = self.aliases.clone();
        let staged = std::mem::take(&mut self.staged);
        let test =
            requires_test(&function.attrs) || function.attrs.iter().any(|attribute| attribute.path().is_ident("test"));
        self.test_depth += usize::from(test);
        syn::visit::visit_impl_item_fn(self, function);
        self.test_depth -= usize::from(test);
        self.aliases = aliases;
        self.staged = staged;
    }

    fn visit_block(&mut self, block: &'ast Block) {
        let staged = self.staged.clone();
        syn::visit::visit_block(self, block);
        self.staged = staged;
    }

    fn visit_local(&mut self, local: &'ast Local) {
        if let (Pat::Ident(binding), Some(initializer)) = (&local.pat, &local.init) {
            self.staged.remove(&binding.ident.to_string());
            if let Expr::Call(call) = initializer.expr.as_ref() {
                if self.program_path(call).is_some()
                    && let Some(shell) = call
                        .args
                        .first()
                        .and_then(string_literal)
                        .filter(|program| shell_program(program))
                {
                    self.staged
                        .insert(binding.ident.to_string(), Staged { shell, armed: false });
                }
            } else if let Expr::Path(alias) = initializer.expr.as_ref()
                && let Some(original) = alias.path.get_ident().map(ToString::to_string)
                && let Some(command) = self.staged.get(&original).cloned()
            {
                self.staged.insert(binding.ident.to_string(), command);
            }
        }
        syn::visit::visit_local(self, local);
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Some(command) = self.program_path(call) {
            self.report_boundary(call, command);
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, expression: &'ast ExprMethodCall) {
        self.report_shell(expression);
        self.inspect_staged(expression);
        syn::visit::visit_expr_method_call(self, expression);
    }
}

fn collect_use(tree: &UseTree, prefix: Vec<String>, aliases: &mut HashMap<String, Vec<String>>) {
    match tree {
        UseTree::Path(path) => {
            let mut prefix = prefix;
            prefix.push(path.ident.to_string());
            collect_use(&path.tree, prefix, aliases);
        }
        UseTree::Name(name) => {
            let mut path = prefix;
            path.push(name.ident.to_string());
            aliases.insert(name.ident.to_string(), path);
        }
        UseTree::Rename(rename) => {
            let mut path = prefix;
            path.push(rename.ident.to_string());
            aliases.insert(rename.rename.to_string(), path);
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use(tree, prefix.clone(), aliases);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn filesystem_modules(source: &Source) -> Vec<String> {
    let components = source
        .path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    let Some(src) = components.iter().rposition(|component| *component == "src") else {
        return Vec::new();
    };
    let mut modules = components[src + 1..components.len().saturating_sub(1)]
        .iter()
        .map(|component| (*component).to_owned())
        .collect::<Vec<_>>();
    if let Some(stem) = source.path.file_stem().and_then(|stem| stem.to_str())
        && !matches!(stem, "lib" | "main" | "mod")
    {
        modules.push(stem.to_owned());
    }
    modules
}

fn shell_chain(expression: &ExprMethodCall, commands: &Commands<'_>) -> Option<String> {
    let (receiver, dynamic_script) = if expression.method == "arg" && expression.args.len() == 1 {
        let Expr::MethodCall(flag) = expression.receiver.as_ref() else {
            return None;
        };
        let shell_flag = flag.method == "arg"
            && flag.args.len() == 1
            && string_literal(&flag.args[0]).is_some_and(|flag| matches!(flag.as_str(), "-c" | "-e"));
        (flag.receiver.as_ref(), shell_flag && dynamic(&expression.args[0]))
    } else if expression.method == "args" && expression.args.len() == 1 {
        let Expr::Array(arguments) = &expression.args[0] else {
            return None;
        };
        let mut arguments = arguments.elems.iter();
        let shell_flag = arguments
            .next()
            .and_then(string_literal)
            .is_some_and(|flag| matches!(flag.as_str(), "-c" | "-e"));
        (
            expression.receiver.as_ref(),
            shell_flag && arguments.next().is_some_and(dynamic),
        )
    } else {
        return None;
    };
    if !dynamic_script {
        return None;
    }
    let (call, program) = root(receiver)?;
    commands.program_path(call)?;
    let program = string_literal(program)?;
    shell_program(&program).then_some(program)
}

fn root(expression: &Expr) -> Option<(&ExprCall, &Expr)> {
    let Expr::Call(call) = expression else {
        return None;
    };
    Some((call, call.args.first()?))
}

fn string_literal(expression: &Expr) -> Option<String> {
    let Expr::Lit(literal) = expression else {
        return None;
    };
    let syn::Lit::Str(value) = &literal.lit else {
        return None;
    };
    Some(value.value())
}

fn dynamic(expression: &Expr) -> bool {
    match expression {
        Expr::Lit(literal) => !matches!(literal.lit, syn::Lit::Str(_)),
        Expr::Macro(ExprMacro { mac, .. }) => mac.path.segments.last().is_none_or(|segment| segment.ident != "concat"),
        _ => true,
    }
}

fn shell_program(program: &str) -> bool {
    matches!(
        program.rsplit('/').next(),
        Some("sh" | "bash" | "dash" | "zsh" | "ksh" | "osascript")
    )
}

#[cfg(test)]
#[path = "test.rs"]
mod tests;
