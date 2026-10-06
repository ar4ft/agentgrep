use crate::{corpus::Source, model::Chunk};
use std::path::Path;
use tree_sitter::{Language, Node, Parser};

/// Worker-only bounded parse. Standalone CLI keeps its existing declaration behavior.
pub fn bounded_symbols(
    source: &Source,
    cancel: &std::sync::atomic::AtomicBool,
    byte_limit: usize,
) -> anyhow::Result<(Vec<Chunk>, bool)> {
    use std::sync::atomic::Ordering;
    let Some(language) = language(&source.path) else {
        return Ok((vec![], false));
    };
    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let mut progress = |_: &tree_sitter::ParseState| cancel.load(Ordering::Relaxed);
    let bytes = source.content.as_bytes();
    let tree = parser
        .parse_with_options(
            &mut |offset, _| &bytes[offset..],
            None,
            Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
        )
        .ok_or_else(|| anyhow::anyhow!("cancelled"))?;
    let lines: Vec<_> = source.content.lines().collect();
    let mut output = Vec::new();
    let mut used = 0;
    let mut clipped = false;
    // Iterative walk avoids recursion on adversarially deep syntax.
    let mut cursor = tree.walk();
    loop {
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "cancelled");
        let node = cursor.node();
        if declaration(node.kind()) {
            let start = node.start_position().row + 1;
            let end =
                (node.end_position().row + usize::from(node.end_position().column > 0)).max(start);
            let size = lines
                .iter()
                .skip(start - 1)
                .take(end - start + 1)
                .map(|l| l.len() + 1)
                .sum::<usize>();
            if output.len() >= 256 || used + size > byte_limit {
                clipped = true;
            } else {
                used += size;
                let symbol = node
                    .child_by_field_name("name")
                    .or_else(|| node.child_by_field_name("type"))
                    .and_then(|n| n.utf8_text(bytes).ok())
                    .map(str::to_owned);
                output.push(Chunk {
                    path: source.path.clone(),
                    start_line: start,
                    end_line: end,
                    symbol,
                    kind: node.kind().into(),
                    content: lines[start - 1..end].join("\n"),
                });
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok((output, clipped));
            }
        }
    }
}

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
