use crate::theme::MarkdownTheme;
use ratatui::text::Span;

const KEYWORDS_RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

const KEYWORDS_JS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const KEYWORDS_PYTHON: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while",
    "with", "yield",
];

const COMMON_TYPES: &[&str] = &[
    "String", "str", "bool", "usize", "isize", "u8", "u16", "u32", "u64", "u128", "i8", "i16",
    "i32", "i64", "i128", "f32", "f64", "Option", "Result", "Vec", "HashMap", "HashSet", "Arc",
    "Mutex", "RwLock", "Box", "Promise", "Array", "Record", "Error", "int", "float", "dict",
    "list", "set", "tuple",
];

/// Parse a single line of code and emit syntax-highlighted spans.
pub fn highlight_code_line(line: &str, lang: &str, theme: &MarkdownTheme) -> Vec<Span<'static>> {
    let lower_lang = lang.to_lowercase();
    let trimmed = line.trim_start();

    // Check single-line comment
    if (lower_lang.contains("rust")
        || lower_lang.contains("js")
        || lower_lang.contains("ts")
        || lower_lang.contains("c"))
        && trimmed.starts_with("//")
    {
        return vec![Span::styled(line.to_string(), theme.code_comment)];
    }
    if (lower_lang.contains("py")
        || lower_lang.contains("sh")
        || lower_lang.contains("bash")
        || lower_lang.contains("yaml")
        || lower_lang.contains("toml"))
        && trimmed.starts_with('#')
    {
        return vec![Span::styled(line.to_string(), theme.code_comment)];
    }

    let mut spans = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];

        // String literals
        if ch == '"' || ch == '\'' || ch == '`' {
            let quote = ch;
            let mut str_val = String::new();
            str_val.push(ch);
            i += 1;
            while i < chars.len() {
                let c = chars[i];
                str_val.push(c);
                if c == '\\' && i + 1 < chars.len() {
                    i += 1;
                    str_val.push(chars[i]);
                } else if c == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            spans.push(Span::styled(str_val, theme.code_string));
            continue;
        }

        // Inline comments starting midway
        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            let comment: String = chars[i..].iter().collect();
            spans.push(Span::styled(comment, theme.code_comment));
            break;
        }

        // Identifiers & Numbers
        if ch.is_alphanumeric() || ch == '_' {
            let mut word = String::new();
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                word.push(chars[i]);
                i += 1;
            }

            if word.chars().all(|c| c.is_ascii_digit() || c == '_') {
                spans.push(Span::styled(word, theme.code_number));
            } else if is_keyword(&word, &lower_lang) {
                spans.push(Span::styled(word, theme.code_keyword));
            } else if COMMON_TYPES.contains(&word.as_str()) {
                spans.push(Span::styled(word, theme.code_type));
            } else {
                spans.push(Span::styled(word, theme.code_text));
            }
            continue;
        }

        // Whitespace and symbols
        let mut sym = String::new();
        while i < chars.len()
            && !chars[i].is_alphanumeric()
            && chars[i] != '_'
            && chars[i] != '"'
            && chars[i] != '\''
            && chars[i] != '`'
            && !(chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '/')
        {
            sym.push(chars[i]);
            i += 1;
        }
        spans.push(Span::styled(sym, theme.code_text));
    }

    if spans.is_empty() {
        vec![Span::styled(line.to_string(), theme.code_text)]
    } else {
        spans
    }
}

fn is_keyword(word: &str, lang: &str) -> bool {
    if lang.contains("rust") || lang == "rs" {
        KEYWORDS_RUST.contains(&word)
    } else if lang.contains("js") || lang.contains("ts") || lang == "node" {
        KEYWORDS_JS.contains(&word)
    } else if lang.contains("py") {
        KEYWORDS_PYTHON.contains(&word)
    } else {
        KEYWORDS_RUST.contains(&word)
            || KEYWORDS_JS.contains(&word)
            || KEYWORDS_PYTHON.contains(&word)
    }
}
