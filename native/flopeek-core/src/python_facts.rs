//! Python parser facts for the strict-native source authority.
use crate::js_facts::{
    NativeJsAnalysis, NativeJsCall, NativeJsCanonicalSymbolIdentity, NativeJsEndpoint,
    NativeJsEvidence, NativeJsFacts, NativeJsImport, NativeJsPosition, NativeJsRange,
    NativeJsStructuralFacts, NativeJsStructuralSymbol, NativeJsSymbol, NativeJsSymbolReference,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::{Node, Parser};

const PARSER: &str = "python-lezer";
fn kids(n: Node<'_>) -> Vec<Node<'_>> {
    let mut c = n.walk();
    n.named_children(&mut c).collect()
}
fn text(n: Node<'_>, s: &str) -> Option<String> {
    n.utf8_text(s.as_bytes()).ok().map(str::to_string)
}
fn top(n: Node<'_>) -> bool {
    n.parent().is_some_and(|p| p.kind() == "module")
        || n.parent().is_some_and(|p| {
            p.kind() == "decorated_definition" && p.parent().is_some_and(|r| r.kind() == "module")
        })
}
fn name(n: Node<'_>, s: &str) -> Option<String> {
    n.child_by_field_name("name")
        .or_else(|| kids(n).into_iter().find(|x| x.kind() == "identifier"))
        .and_then(|x| text(x, s))
}
fn ev(p: &str, s: &str, n: Node<'_>) -> NativeJsEvidence {
    let a = n.start_position();
    let b = n.end_position();
    let col = |i| {
        s[..i]
            .rsplit_once('\n')
            .map_or(i + 1, |(_, x)| x.chars().count() + 1)
    };
    let declaration = matches!(
        n.kind(),
        "class_definition" | "function_definition" | "decorator"
    );
    let trailing = &s[n.end_byte()..];
    let end = if declaration && (trailing.starts_with('\n') || trailing.starts_with("\r\n")) {
        NativeJsPosition {
            line: b.row + 2,
            column: 1,
        }
    } else {
        NativeJsPosition {
            line: b.row + 1,
            column: col(n.end_byte()),
        }
    };
    NativeJsEvidence {
        parser: PARSER.into(),
        file: p.into(),
        range: NativeJsRange {
            start: NativeJsPosition {
                line: a.row + 1,
                column: col(n.start_byte()),
            },
            end,
        },
    }
}
fn imports(n: Node<'_>, s: &str) -> Vec<String> {
    let v = text(n, s).unwrap_or_default();
    let v = v.trim();
    if let Some(r) = v.strip_prefix("from ") {
        return r
            .split_once(" import ")
            .map(|(m, _)| vec![m.trim().into()])
            .unwrap_or_default();
    }
    v.strip_prefix("import ")
        .map(|r| {
            r.split(',')
                .filter_map(|x| x.split_ascii_whitespace().next())
                .map(Into::into)
                .collect()
        })
        .unwrap_or_default()
}
fn bindings(n: Node<'_>, s: &str, o: &mut BTreeMap<String, (String, String)>) {
    let v = text(n, s).unwrap_or_default();
    if let Some(r) = v.trim().strip_prefix("from ")
        && let Some((m, ns)) = r.split_once(" import ")
    {
        for x in ns.split(',') {
            let w = x.split_ascii_whitespace().collect::<Vec<_>>();
            if let Some(e) = w.first() {
                let l = if w.len() >= 3 && w[1] == "as" {
                    w[2]
                } else {
                    e
                };
                o.insert(l.into(), (m.trim().into(), (*e).into()));
            }
        }
    }
}
fn methods(n: Node<'_>, s: &str) -> Vec<String> {
    n.child_by_field_name("body")
        .map(kids)
        .unwrap_or_default()
        .into_iter()
        .filter(|x| x.kind() == "function_definition")
        .filter_map(|x| name(x, s))
        .collect()
}
fn function_signature(n: Node<'_>, s: &str) -> String {
    let parameters = n
        .child_by_field_name("parameters")
        .map(kids)
        .unwrap_or_default()
        .into_iter()
        .map(|parameter| {
            parameter
                .child_by_field_name("type")
                .and_then(|kind| text(kind, s))
                .map(|kind| {
                    kind.chars()
                        .filter(|value| !value.is_whitespace())
                        .collect()
                })
                .unwrap_or_else(|| "unknown".to_string())
        })
        .collect::<Vec<_>>()
        .join(",");
    let return_type = n
        .child_by_field_name("return_type")
        .and_then(|kind| text(kind, s))
        .map(|kind| {
            kind.chars()
                .filter(|value| !value.is_whitespace())
                .collect()
        })
        .unwrap_or_else(|| "unknown".to_string());
    format!("({parameters}):{return_type}")
}
fn endpoint(n: Node<'_>, p: &str, s: &str) -> Option<NativeJsEndpoint> {
    let v = text(n, s)?;
    let (r, t) = v.trim().strip_prefix('@')?.split_once('.')?;
    if !["app", "api", "router", "server", "blueprint", "bp"]
        .contains(&r.to_ascii_lowercase().as_str())
    {
        return None;
    }
    let (m, a) = t.split_once('(')?;
    let m = m.to_ascii_uppercase();
    if !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&m.as_str()) {
        return None;
    }
    let q = a
        .split(')')
        .next()?
        .trim()
        .trim_matches(|c| c == '\'' || c == '\"');
    if q.is_empty() {
        return None;
    }
    Some(NativeJsEndpoint {
        method: m,
        route: q.into(),
        handler_name: None,
        handler_type: None,
        contract: None,
        confidence: Some("likely".into()),
        detected_responsibility: Some(
            "Possible HTTP endpoint detected from a Python framework decorator.".into(),
        ),
        evidence: ev(p, s, n),
    })
}
fn diag(n: Node<'_>) -> usize {
    let mut count = 0;
    let mut stack = vec![n];
    while let Some(current) = stack.pop() {
        count += usize::from(current.is_error() || current.is_missing());
        let mut cursor = current.walk();
        stack.extend(current.named_children(&mut cursor));
    }
    count
}
fn owner(n: Node<'_>, s: &str) -> Option<NativeJsSymbolReference> {
    let mut x = n.parent();
    while let Some(p) = x {
        if p.kind() == "module" {
            return None;
        }
        if p.kind() == "function_definition" && top(p) {
            return name(p, s).map(|name| NativeJsSymbolReference {
                symbol_type: "function".into(),
                name,
            });
        }
        if p.kind() == "class_definition" && top(p) {
            return name(p, s).map(|name| NativeJsSymbolReference {
                symbol_type: "class".into(),
                name,
            });
        }
        x = p.parent()
    }
    None
}

fn module_bindings(n: Node<'_>, s: &str, bindings: &mut BTreeMap<String, String>) {
    if n.kind() == "import_statement" {
        let statement = text(n, s).unwrap_or_default();
        if let Some(imports) = statement.trim().strip_prefix("import ") {
            for import in imports.split(',') {
                let words = import.split_ascii_whitespace().collect::<Vec<_>>();
                let Some(specifier) = words.first() else {
                    continue;
                };
                let local = if words.len() >= 3 && words[1] == "as" {
                    words[2]
                } else {
                    specifier.split('.').next().unwrap_or(specifier)
                };
                bindings.insert(local.into(), (*specifier).into());
            }
        }
    }
    for child in kids(n) {
        module_bindings(child, s, bindings);
    }
}

fn framework_receivers(
    source: &str,
    imported: &BTreeMap<String, (String, String)>,
    modules: &BTreeMap<String, String>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut typer = BTreeSet::new();
    let mut flask = BTreeSet::new();
    for line in source.lines() {
        let Some((target, expression)) = line.split_once('=') else {
            continue;
        };
        let target = target.trim();
        if !target
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        {
            continue;
        }
        let callee = expression.trim().split('(').next().unwrap_or("").trim();
        let typer_factory = callee.split_once('.').is_some_and(|(module, factory)| {
            modules.get(module).is_some_and(|value| value == "typer") && factory == "Typer"
        });
        let flask_factory = imported
            .get(callee)
            .is_some_and(|binding| binding.0 == "flask" && binding.1 == "Flask");
        if typer_factory {
            typer.insert(target.into());
        }
        if flask_factory {
            flask.insert(target.into());
        }
    }
    (typer, flask)
}

fn command_name(text: &str, target: &str) -> Result<String, String> {
    let Some((_, arguments)) = text.split_once('(') else {
        return Err("missing-command-decorator-call".into());
    };
    let arguments = arguments
        .rsplit_once(')')
        .map(|(value, _)| value)
        .unwrap_or(arguments)
        .trim();
    if arguments.is_empty() {
        return Ok(target.into());
    }
    let literal = arguments.trim_matches(|character| character == '\'' || character == '\"');
    if literal != arguments && !literal.is_empty() && !literal.contains('\\') {
        return Ok(literal.into());
    }
    Err("non-literal-or-unsupported-command-name".into())
}

fn collect_framework_commands(
    root: Node<'_>,
    path: &str,
    source: &str,
    imported: &BTreeMap<String, (String, String)>,
    modules: &BTreeMap<String, String>,
    facts: &mut NativeJsStructuralFacts,
) {
    let (typer_receivers, flask_receivers) = framework_receivers(source, imported, modules);
    for statement in kids(root) {
        let Some(definition) = (statement.kind() == "decorated_definition")
            .then(|| {
                kids(statement)
                    .into_iter()
                    .find(|child| child.kind() == "function_definition")
            })
            .flatten()
        else {
            continue;
        };
        let Some(target_name) = name(definition, source) else {
            continue;
        };
        for decorator in kids(statement)
            .into_iter()
            .filter(|child| child.kind() == "decorator")
        {
            let value = text(decorator, source).unwrap_or_default();
            let callee = value
                .trim()
                .trim_start_matches('@')
                .split('(')
                .next()
                .unwrap_or("")
                .trim();
            let parts = callee.split('.').collect::<Vec<_>>();
            let adapter = if parts.len() == 2
                && parts[1] == "command"
                && modules
                    .get(parts[0])
                    .is_some_and(|module| module == "click")
            {
                Some("click")
            } else if parts.len() == 2
                && parts[1] == "command"
                && typer_receivers.contains(parts[0])
            {
                Some("typer")
            } else if parts.len() == 3
                && parts[1] == "cli"
                && parts[2] == "command"
                && flask_receivers.contains(parts[0])
            {
                Some("flask")
            } else {
                None
            };
            let Some(adapter) = adapter else { continue };
            match command_name(&value, &target_name) {
                Ok(command_name) => facts.framework_commands.push(json!({
                    "adapter": adapter, "commandName": command_name, "targetName": target_name,
                    "targetType": "function", "path": path, "evidence": ev(path, source, decorator),
                })),
                Err(reason) => facts.unsupported_framework_commands.push(json!({
                    "path": path, "adapter": adapter, "targetName": target_name, "reason": reason,
                })),
            }
        }
    }

    let normalized_path = path.replace('\\', "/");
    let components = normalized_path.split('/').collect::<Vec<_>>();
    let Some(filename) = components.last().filter(|name| name.ends_with(".py")) else {
        return;
    };
    if components.len() < 3
        || components[components.len() - 3] != "management"
        || components[components.len() - 2] != "commands"
    {
        return;
    }
    let command_name = filename.trim_end_matches(".py");
    if command_name.is_empty() || command_name.starts_with('_') {
        return;
    }
    let command = kids(root).into_iter().find(|node| {
        node.kind() == "class_definition" && name(*node, source).as_deref() == Some("Command")
    });
    let Some(command) = command else {
        facts.unsupported_framework_commands.push(json!({"path":path,"commandName":command_name,"reason":"missing-top-level-command-class"}));
        return;
    };
    let directly_extends_base_command = text(command, source).is_some_and(|value| {
        value
            .lines()
            .next()
            .is_some_and(|line| line.contains("Command(BaseCommand"))
    }) && imported.get("BaseCommand").is_some_and(|binding| {
        binding.0 == "django.core.management.base" && binding.1 == "BaseCommand"
    });
    let has_handle = methods(command, source)
        .iter()
        .any(|method| method == "handle");
    if directly_extends_base_command && has_handle {
        facts.framework_commands.push(json!({
            "adapter":"django","commandName":command_name,"targetName":"Command","targetType":"class","path":path,"evidence":ev(path,source,command),
        }));
    } else {
        facts.unsupported_framework_commands.push(json!({"path":path,"commandName":command_name,"reason":if directly_extends_base_command {"missing-direct-handle-method"} else {"command-class-does-not-directly-extend-imported-base-command"}}));
    }
}
fn collect(
    n: Node<'_>,
    p: &str,
    s: &str,
    b: &BTreeMap<String, (String, String)>,
    f: &mut NativeJsStructuralFacts,
) {
    if n.kind() == "function_definition"
        && f.methods.len() < 12
        && let Some(method) = name(n, s)
        && !f.methods.contains(&method)
    {
        f.methods.push(method)
    }
    match n.kind() {
        "import_statement" | "import_from_statement" | "future_import_statement" => {
            for specifier in imports(n, s) {
                f.imports.push(NativeJsImport {
                    specifier,
                    standard: None,
                    evidence: ev(p, s, n),
                })
            }
        }
        "class_definition" if top(n) => {
            if let Some(class_name) = name(n, s) {
                let symbol = NativeJsStructuralSymbol {
                    symbol_type: "class".into(),
                    name: class_name.clone(),
                    methods: methods(n, s),
                    evidence: ev(p, s, n),
                    identity: Some(NativeJsCanonicalSymbolIdentity {
                        qualified_name: class_name.clone(),
                        lexical_owner: None,
                        signature: None,
                        discriminator: "type".into(),
                    }),
                };
                f.symbols.push(symbol.clone());
                f.canonical_symbols.push(symbol);
                let owner = NativeJsSymbolReference {
                    symbol_type: "class".into(),
                    name: class_name.clone(),
                };
                for method in n
                    .child_by_field_name("body")
                    .map(kids)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|child| child.kind() == "function_definition")
                {
                    if let Some(method_name) = name(method, s) {
                        f.canonical_symbols.push(NativeJsStructuralSymbol {
                            symbol_type: "method".into(),
                            name: method_name.clone(),
                            methods: vec![],
                            evidence: ev(p, s, method),
                            identity: Some(NativeJsCanonicalSymbolIdentity {
                                qualified_name: format!("{class_name}.{method_name}"),
                                lexical_owner: Some(owner.clone()),
                                signature: Some(function_signature(method, s)),
                                discriminator: if method_name == "__init__" {
                                    "constructor"
                                } else {
                                    "instance-method"
                                }
                                .into(),
                            }),
                        });
                    }
                }
            }
        }
        "function_definition" if top(n) => {
            if let Some(name) = name(n, s) {
                let symbol = NativeJsStructuralSymbol {
                    symbol_type: "function".into(),
                    name: name.clone(),
                    methods: vec![],
                    evidence: ev(p, s, n),
                    identity: Some(NativeJsCanonicalSymbolIdentity {
                        qualified_name: name,
                        lexical_owner: None,
                        signature: Some(function_signature(n, s)),
                        discriminator: "top-level-function".into(),
                    }),
                };
                f.symbols.push(symbol.clone());
                f.canonical_symbols.push(symbol);
            }
        }
        "decorator" => {
            if let Some(e) = endpoint(n, p, s) {
                f.endpoints.push(e)
            }
        }
        "call" => {
            if let Some(nm) = n
                .child_by_field_name("function")
                .filter(|x| x.kind() == "identifier")
                .and_then(|x| text(x, s))
            {
                let imported = b.get(&nm).map(|(specifier, exported_name)| {
                    crate::js_facts::NativeJsImportedReference {
                        specifier: specifier.clone(),
                        exported_name: exported_name.clone(),
                    }
                });
                f.calls.push(NativeJsCall {
                    name: nm,
                    source: owner(n, s),
                    imported,
                    evidence: ev(p, s, n),
                })
            }
        }
        _ => {}
    }
    for x in kids(n) {
        collect(x, p, s, b, f)
    }
}
pub fn parse_native_python_facts(path: &str, source: &str) -> Option<NativeJsFacts> {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_python::LANGUAGE.into()).ok()?;
    parse_native_python_facts_with_parser(path, source, &mut p)
}

/// Parse Python with a caller-owned parser so a cold multi-file scan can
/// retain the configured grammar across a bounded worker chunk. The fact
/// traversal remains identical to the fresh-parser path above.
pub fn parse_native_python_facts_with_parser(
    path: &str,
    source: &str,
    parser: &mut Parser,
) -> Option<NativeJsFacts> {
    let t = parser.parse(source, None)?;
    let r = t.root_node();
    let mut b = BTreeMap::new();
    fn walk(n: Node<'_>, s: &str, b: &mut BTreeMap<String, (String, String)>) {
        if matches!(n.kind(), "import_statement" | "import_from_statement") {
            bindings(n, s, b)
        }
        for x in kids(n) {
            walk(x, s, b)
        }
    }
    walk(r, source, &mut b);
    let mut imported_modules = BTreeMap::new();
    module_bindings(r, source, &mut imported_modules);
    let d = diag(r);
    let mut f = NativeJsStructuralFacts {
        imports: vec![],
        symbols: vec![],
        canonical_symbols: vec![],
        calls: vec![],
        endpoints: vec![],
        requests: vec![],
        integrations: vec![],
        framework_commands: vec![],
        unsupported_framework_commands: vec![],
        runtime_actions: vec![],
        schedules: vec![],
        unsupported_schedules: vec![],
        methods: vec![],
        analysis: NativeJsAnalysis {
            parser: PARSER.into(),
            status: if d > 0 {
                "parsed-with-diagnostics"
            } else {
                "parsed"
            }
            .into(),
            confidence: "exact".into(),
            diagnostics: d,
            reason: None,
        },
    };
    collect(r, path, source, &b, &mut f);
    collect_framework_commands(r, path, source, &b, &imported_modules, &mut f);
    // Python overload stubs share the same public symbol identity as their
    // implementation. The JavaScript compatibility oracle retains the first
    // declaration evidence for that identity, so later overloads must not
    // create conflicting contains/declares edges.
    let mut seen_symbols = BTreeSet::new();
    f.symbols
        .retain(|symbol| seen_symbols.insert((symbol.symbol_type.clone(), symbol.name.clone())));
    for z in &f.symbols {
        for m in &z.methods {
            if f.methods.len() < 12 && !f.methods.contains(m) {
                f.methods.push(m.clone())
            }
        }
    }
    let im = f
        .imports
        .iter()
        .map(|x| x.specifier.clone())
        .collect::<BTreeSet<_>>();
    let sy = f
        .symbols
        .iter()
        .map(|x| NativeJsSymbol {
            kind: x.symbol_type.clone(),
            name: x.name.clone(),
        })
        .collect();
    let ca = f
        .calls
        .iter()
        .map(|x| x.name.clone())
        .collect::<BTreeSet<_>>();
    Some(NativeJsFacts {
        schema_version: crate::js_facts::NATIVE_JS_FACTS_SCHEMA.into(),
        parser: "tree-sitter-python".into(),
        status: f.analysis.status.clone(),
        diagnostics: d,
        imports: im.into_iter().collect(),
        symbols: sy,
        direct_calls: ca.into_iter().collect(),
        structural: f,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_native_python_facts, parse_native_python_facts_with_parser};

    #[test]
    fn preserves_python_imports_symbols_calls_and_route_decorators() {
        let facts = parse_native_python_facts(
            "src/payments/routes.py",
            "from .service import PaymentService\nfrom fastapi import APIRouter\n\nrouter = APIRouter()\n\n@router.get(\"/payments/{payment_id}\")\ndef get_payment():\n    return PaymentService.find()\n",
        )
        .expect("Python files have a strict native parser");

        assert_eq!(facts.parser, "tree-sitter-python");
        assert_eq!(facts.imports, vec![".service", "fastapi"]);
        assert_eq!(facts.structural.symbols[0].name, "get_payment");
        assert_eq!(facts.structural.methods, vec!["get_payment"]);
        assert_eq!(facts.structural.endpoints.len(), 1);
        let endpoint = &facts.structural.endpoints[0];
        assert_eq!(endpoint.method, "GET");
        assert_eq!(endpoint.route, "/payments/{payment_id}");
        assert_eq!(endpoint.confidence.as_deref(), Some("likely"));
        assert_eq!(endpoint.evidence.range.end.line, 7);
        assert_eq!(endpoint.evidence.range.end.column, 1);
    }

    #[test]
    fn keeps_first_overload_declaration_for_one_public_symbol_identity() {
        let facts = parse_native_python_facts(
            "tests/serializer.py",
            "from typing import overload\n\n@overload\ndef coerce(value: str) -> str: ...\n\n@overload\ndef coerce(value: bytes) -> bytes: ...\n\ndef coerce(value):\n    return value\n",
        )
        .unwrap();
        let symbols = facts
            .structural
            .symbols
            .iter()
            .filter(|symbol| symbol.name == "coerce")
            .collect::<Vec<_>>();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].evidence.range.start.line, 4);
    }

    #[test]
    fn reused_tree_sitter_parser_preserves_python_facts() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        let source = "from .service import PaymentService\n\ndef load():\n    return PaymentService.find()\n";
        let fresh = parse_native_python_facts("src/service.py", source).unwrap();
        let reused =
            parse_native_python_facts_with_parser("src/service.py", source, &mut parser).unwrap();
        assert_eq!(reused, fresh);

        let second_source = "class PaymentService:\n    def find(self):\n        return None\n";
        let second_reused =
            parse_native_python_facts_with_parser("src/model.py", second_source, &mut parser)
                .unwrap();
        let second_fresh = parse_native_python_facts("src/model.py", second_source).unwrap();
        assert_eq!(second_reused, second_fresh);
    }
}
