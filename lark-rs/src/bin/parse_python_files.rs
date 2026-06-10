use lark_rs::{Indenter, LarkOptions, LexerType, ParserAlgorithm};
use std::path::Path;

fn main() {
    let grammar = include_str!("../grammars/python.lark");
    let parser = lark_rs::Lark::new(
        grammar,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["file_input".to_string()],
            maybe_placeholders: true,
            postlex: Some(Indenter {
                nl_type: "_NEWLINE".to_string(),
                open_paren_types: vec!["LPAR".into(), "LSQB".into(), "LBRACE".into()],
                close_paren_types: vec!["RPAR".into(), "RSQB".into(), "RBRACE".into()],
                indent_type: "_INDENT".to_string(),
                dedent_type: "_DEDENT".to_string(),
                tab_len: 8,
            }),
            ..Default::default()
        },
    )
    .expect("python.lark must build");

    let lark_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("lark");

    let mut files: Vec<_> = walkdir(&lark_dir);
    files.sort();

    let total = files.len();
    let mut errors = 0usize;

    for path in &files {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("READ ERROR  {}: {}", path.display(), e);
                errors += 1;
                continue;
            }
        };
        // Ensure the file ends with a newline — the grammar requires it
        let src = if src.ends_with('\n') {
            src
        } else {
            format!("{src}\n")
        };

        if let Err(e) = parser.parse(&src) {
            let rel = path
                .strip_prefix(lark_dir.parent().unwrap())
                .unwrap_or(path);
            eprintln!("PARSE ERROR {}: {}", rel.display(), e);
            errors += 1;
        }
    }

    let ok = total - errors;
    println!("\n{ok}/{total} files parsed successfully, {errors} error(s).");
    if errors > 0 {
        std::process::exit(1);
    }
}

fn walkdir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walkdir(&path));
            } else if path.extension().map_or(false, |e| e == "py") {
                out.push(path);
            }
        }
    }
    out
}
