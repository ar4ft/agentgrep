use crate::{corpus::Source, model::Chunk};
use std::path::Path;
use tree_sitter::{Language, Node, Parser};

fn language(path: &str) -> Option<Language> {
    Some(match Path::new(path).extension()?.to_str()? {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        _ => return None,
    })
}

fn declaration(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "impl_item"
            | "function_definition"
            | "class_definition"
            | "function_declaration"
            | "method_definition"
            | "class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "method_declaration"
            | "type_declaration"
            | "protocol_declaration"
            | "init_declaration"
            | "deinit_declaration"
    )
}

fn collect(node: Node<'_>, source: &Source, lines: &[&str], output: &mut Vec<Chunk>) {
    if declaration(node.kind()) {
        let name = node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("type"));
        let symbol = name
            .and_then(|n| n.utf8_text(source.content.as_bytes()).ok())
            .map(str::to_owned);
        // Line-based source slices include indentation and are directly verifiable.
        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + usize::from(node.end_position().column > 0);
        let end_line = end_line.max(start_line);
        let content = lines
            .iter()
            .skip(start_line - 1)
            .take(end_line - start_line + 1)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        output.push(Chunk {
            path: source.path.clone(),
            start_line,
            end_line,
            symbol,
            kind: node.kind().into(),
            content,
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(child, source, lines, output);
    }
}

pub fn symbols(source: &Source) -> Vec<Chunk> {
    let Some(language) = language(&source.path) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(&source.content, None) else {
        return Vec::new();
    };
    let mut chunks = Vec::new();
    let lines: Vec<_> = source.content.lines().collect();
    collect(tree.root_node(), source, &lines, &mut chunks);
    chunks
}

pub fn chunks(source: &Source) -> Vec<Chunk> {
    let mut chunks = symbols(source);
    // Index windows as well: imports, configuration, and top-level calls matter.
    let lines: Vec<_> = source.content.lines().collect();
    for start in (0..lines.len()).step_by(60) {
        let end = (start + 80).min(lines.len());
        chunks.push(Chunk {
            path: source.path.clone(),
            start_line: start + 1,
            end_line: end,
            symbol: None,
            kind: "window".into(),
            content: lines[start..end].join("\n"),
        });
    }
    chunks
}
