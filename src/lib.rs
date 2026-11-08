//! SQL parser plugin - full-parse mode.
//!
//! Handles generic SQL (tree-sitter-sql grammar).
//! Delegates all CST processing to the shared `sql-parser-lib` crate.

use intentumdiff_plugin_sdk::cst::CstNode;
use sql_parser_lib;

wit_bindgen::generate!({
    path: "wit/plugin.wit",
    world: "parser-plugin",
});

use crate::exports::intentdiff::plugin::parser::ExamplePair;
use crate::exports::intentdiff::plugin::parser::Guest;
use crate::exports::intentdiff::plugin::parser::LanguageInfoRecord;
use crate::exports::intentdiff::plugin::parser::ParserMode;

const PLUGIN_METADATA: &str = include_str!("../plugin_metadata.info");

fn language_info_for(ids: Vec<String>) -> Vec<LanguageInfoRecord> {
    let metadata = intentumdiff_plugin_sdk::metadata::parse_plugin_metadata(PLUGIN_METADATA);
    ids.into_iter()
        .map(|language_id| {
            let info = metadata.language_or_default(&language_id);
            LanguageInfoRecord {
                language_id: info.language_id,
                language_name: info.language_name,
                language_short_name: info.language_short_name,
                monaco_language: info.monaco_language,
                default_filename: info.default_filename,
                language_file_extensions: info.language_file_extensions,
                author: metadata.author().to_string(),
                plugin_version: metadata.plugin_version().to_string(),
                last_updated: metadata.last_updated().to_string(),
            }
        })
        .collect()
}
struct SqlParser;

fn node_to_cst(node: tree_sitter::Node<'_>, source: &[u8]) -> CstNode {
    let children: Vec<CstNode> = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .map(|child| node_to_cst(child, source))
        .collect();

    let text = if children.is_empty() {
        // Semantic hashes must include the complete leaf, including long literal tails.
        Some(node.utf8_text(source).unwrap_or("").to_owned())
    } else {
        None
    };

    CstNode {
        node_type: node.kind().to_string(),
        named: node.is_named(),
        text,
        start_line: node.start_position().row as u32,
        start_col: node.start_position().column as u32,
        end_line: node.end_position().row as u32,
        end_col: node.end_position().column as u32,
        children,
    }
}

fn process_impl(source: &str) -> String {
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_sequel::LANGUAGE.into();
    if parser.set_language(&lang).is_err() {
        return r#"{"error":"Failed to load SQL grammar"}"#.to_string();
    }
    let tree = match parser.parse(source, None) {
        Some(tree) => tree,
        None => return r#"{"error":"Parse failed"}"#.to_string(),
    };
    let root = node_to_cst(tree.root_node(), source.as_bytes());
    match serde_json::to_string(&root) {
        Ok(cst_json) => sql_parser_lib::process_impl(&cst_json),
        Err(e) => format!(r#"{{"error":"CST serialisation failed: {}"}}"#, e),
    }
}

impl Guest for SqlParser {
    fn get_parser_mode() -> ParserMode {
        ParserMode::FullParse
    }
    fn grammar_id() -> String {
        "sql".to_string()
    }
    fn detect_language(filename: String, _content: String) -> String {
        let f = filename.to_lowercase();
        if f.ends_with(".sql") || f.ends_with(".ddl") || f.ends_with(".dml") {
            "sql".to_string()
        } else {
            String::new()
        }
    }
    fn preprocess_source(source: String) -> String {
        source
    }
    fn example(_language: String) -> ExamplePair {
        ExamplePair {
            old: "SELECT id, name, email\nFROM users\nWHERE active = 1;\n".to_string(),
            new: "SELECT\n    u.id,\n    u.name,\n    u.email,\n    o.total AS order_total\nFROM users u\nLEFT JOIN orders o ON o.user_id = u.id\nWHERE u.active = 1\nORDER BY u.name;\n".to_string(),
        }
    }
    fn process(input: String, _language: String, _filename: String) -> String {
        process_impl(&input)
    }
    fn trivia_node_types() -> Vec<String> {
        sql_parser_lib::TRIVIA
            .iter()
            .map(|s| s.to_string())
            .collect()
    }
    fn language_ids() -> Vec<String> {
        vec!["sql".to_string()]
    }
    fn language_info() -> Vec<LanguageInfoRecord> {
        language_info_for(Self::language_ids())
    }
    fn priority() -> i32 {
        0
    }
}

export!(SqlParser);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exports::intentdiff::plugin::parser::Guest;
    use intentumdiff_plugin_sdk::testing as t;

    fn parse_cst(source: &str) -> CstNode {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_sequel::LANGUAGE.into())
            .expect("SQL grammar loads");
        let tree = parser.parse(source, None).expect("SQL parses");
        assert!(!tree.root_node().has_error(), "valid test SQL: {source}");
        node_to_cst(tree.root_node(), source.as_bytes())
    }

    fn semantic_tree(source: &str) -> serde_json::Value {
        let result: serde_json::Value =
            serde_json::from_str(&process_impl(source)).expect("semantic JSON");
        assert!(result.get("error").is_none(), "SQL parses: {result}");
        assert!(result["structural_hash"].is_string());
        result
    }

    #[test]
    fn long_literal_suffix_changes_survive_semantic_hashing() {
        for length in [4095, 4096, 4097, 4200, 32768] {
            let prefix = "a".repeat(length);
            let old = format!("SELECT '{prefix}x';");
            let new = format!("SELECT '{prefix}y';");
            let old_tree = semantic_tree(&old);
            let new_tree = semantic_tree(&new);
            assert_ne!(
                old_tree["structural_hash"], new_tree["structural_hash"],
                "suffix after {length} characters must affect semantic hash"
            );
            assert_eq!(old_tree, semantic_tree(&old), "deterministic parse");
        }
    }

    #[test]
    fn long_literal_retains_full_text_and_byte_span() {
        for prefix in ["a".repeat(4200), "é界🦀".repeat(1400)] {
            let literal = format!("'{prefix}x'");
            let source = format!("SELECT {literal};");
            let cst = parse_cst(&source);
            let leaf = cst
                .walk()
                .find(|node| node.node_type == "literal" && node.is_leaf())
                .expect("SQL string literal leaf");
            assert_eq!(leaf.text.as_deref(), Some(literal.as_str()));
            assert_eq!((leaf.start_line, leaf.start_col), (0, 7));
            assert_eq!((leaf.end_line, leaf.end_col), (0, 7 + literal.len() as u32));
            assert!(source.is_char_boundary(leaf.start_col as usize));
            assert!(source.is_char_boundary(leaf.end_col as usize));
        }
    }

    #[test]
    fn unicode_literal_suffix_changes_after_character_limit_survive() {
        let prefix = "é界🦀".repeat(1400);
        let old = semantic_tree(&format!("SELECT '{prefix}x';"));
        let new = semantic_tree(&format!("SELECT '{prefix}y';"));
        assert_ne!(old["structural_hash"], new["structural_hash"]);
    }

    #[test]
    fn unchanged_long_literal_keeps_hash_across_spacing() {
        let literal = format!("'{}x'", "é界🦀".repeat(1400));
        let old = semantic_tree(&format!("SELECT {literal};"));
        let new = semantic_tree(&format!("SELECT\n    {literal}\n;"));
        assert_eq!(old["structural_hash"], new["structural_hash"]);
    }

    #[test]
    fn grammar_id_nonempty() {
        assert!(!SqlParser::grammar_id().is_empty());
    }

    #[test]
    fn language_ids_contain_grammar_id() {
        let gid = SqlParser::grammar_id();
        let ids = SqlParser::language_ids();
        assert!(
            ids.contains(&gid),
            "language_ids {:?} must contain {:?}",
            ids,
            gid
        );
    }

    #[test]
    fn detect_language_known_ext() {
        let r = SqlParser::detect_language("test.sql".to_string(), "".to_string());
        assert_eq!(r.as_str(), "sql");
    }

    #[test]
    fn detect_language_unknown_ext() {
        let r =
            SqlParser::detect_language("test.xyz_notareal_ext_9z8y".to_string(), "".to_string());
        assert_eq!(r.as_str(), "");
    }

    #[test]
    fn process_impl_empty_returns_valid_json() {
        let out = SqlParser::process("".to_string(), "sql".to_string(), "test.sql".to_string());
        t::assert_valid_json(&out, "process(empty)");
    }

    #[test]
    fn process_impl_whitespace_returns_valid_json() {
        let out = SqlParser::process(
            "   \n  ".to_string(),
            "sql".to_string(),
            "test.sql".to_string(),
        );
        t::assert_valid_json(&out, "process(whitespace)");
    }
}
