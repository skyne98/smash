//! Syntax highlighting for fenced code blocks.
//!
//! syntect parses with bat's grammar set (two-face: ~200 languages, incl. Nix,
//! TOML, TypeScript). Scopes are folded into a dozen [`Token`] classes so each
//! frontend only maps classes to colours, never TextMate scopes.

use std::sync::OnceLock;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Plain,
    Keyword,
    String,
    Comment,
    Number,
    Constant,
    Function,
    Type,
    Macro,
    Punct,
    Added,
    Removed,
}

impl Token {
    /// Night palette, tuned against the code panel surface (0x262838).
    pub fn color(self) -> u32 {
        match self {
            Token::Plain => 0xd4d6e0,
            Token::Keyword => 0xc49cf0,
            Token::String => 0xa9d18e,
            Token::Comment => 0x7c8091,
            Token::Number | Token::Constant => 0xf2a86f,
            Token::Function => 0x82aaff,
            Token::Type => 0xe8c77d,
            Token::Macro => 0x7fd6e8,
            Token::Punct => 0x9ba0b2,
            Token::Added => 0x8fd18f,
            Token::Removed => 0xf07882,
        }
    }
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// Scope prefix → token, most specific first. The innermost scope that
/// matches anything wins, so `"…"` quotes stay string-coloured.
fn classes() -> &'static [(Scope, Token)] {
    static CLASSES: OnceLock<Vec<(Scope, Token)>> = OnceLock::new();
    CLASSES.get_or_init(|| {
        [
            ("comment", Token::Comment),
            ("punctuation.definition.comment", Token::Comment),
            ("string", Token::String),
            ("punctuation.definition.string", Token::String),
            ("constant.character.escape", Token::Macro),
            ("constant.numeric", Token::Number),
            ("constant", Token::Constant),
            ("variable.language", Token::Keyword),
            ("keyword.operator", Token::Punct),
            ("keyword", Token::Keyword),
            ("storage.type.numeric", Token::Type),
            ("storage", Token::Keyword),
            ("support.macro", Token::Macro),
            ("entity.name.macro", Token::Macro),
            ("meta.annotation", Token::Macro),
            ("meta.attribute", Token::Macro),
            ("entity.name.function", Token::Function),
            ("support.function", Token::Function),
            ("variable.function", Token::Function),
            ("meta.diff.range", Token::Function),
            ("meta.diff.header", Token::Comment),
            ("entity.name.tag", Token::Keyword),
            ("entity.other.attribute-name", Token::Type),
            ("entity.name", Token::Type),
            ("entity.other.inherited-class", Token::Type),
            ("support.type", Token::Type),
            ("support.class", Token::Type),
            ("markup.inserted", Token::Added),
            ("markup.deleted", Token::Removed),
            ("markup.heading", Token::Function),
            ("punctuation", Token::Punct),
        ]
        .into_iter()
        .filter_map(|(scope, token)| Some((Scope::new(scope).ok()?, token)))
        .collect()
    })
}

/// Load grammars off the UI thread so the first code block doesn't stall.
pub fn warm() {
    std::thread::spawn(|| {
        syntaxes();
        classes();
    });
}

fn find(lang: &str) -> Option<&'static SyntaxReference> {
    let lower = lang.to_ascii_lowercase();
    let token = match lower.as_str() {
        "" | "text" | "txt" | "plain" | "plaintext" | "output" => return None,
        "shell" | "console" | "sh" | "zsh" | "shellsession" | "terminal" => "bash",
        "jsonc" | "json5" => "json",
        "golang" => "go",
        "python3" | "py3" => "py",
        "c++" => "cpp",
        "objc" | "objective-c" => "m",
        "patch" => "diff",
        other => other,
    };
    syntaxes().find_syntax_by_token(token)
}

/// Pick the token for the current scope stack.
fn class(stack: &ScopeStack) -> Token {
    for scope in stack.as_slice().iter().rev() {
        for (prefix, token) in classes() {
            if prefix.is_prefix_of(*scope) {
                return *token;
            }
        }
    }
    Token::Plain
}

/// `code` split into lines of (token, text) runs. Unknown languages and
/// grammar errors fall back to plain text: never lose a character.
pub fn highlight(lang: &str, code: &str) -> Vec<Vec<(Token, String)>> {
    let code = code.strip_suffix('\n').unwrap_or(code);
    let plain = || {
        code.split('\n')
            .map(|line| vec![(Token::Plain, line.replace('\t', "    "))])
            .collect()
    };
    let Some(syntax) = find(lang) else {
        return plain();
    };
    let set = syntaxes();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Vec::new();
    let mut buffer = String::new();
    for line in code.split('\n') {
        // Grammars from the "newlines" set expect the line ending.
        buffer.clear();
        buffer.push_str(line);
        buffer.push('\n');
        let Ok(ops) = state.parse_line(&buffer, set) else {
            return plain();
        };
        let mut runs: Vec<(Token, String)> = Vec::new();
        let mut emit = |token: Token, text: &str| {
            if text.is_empty() {
                return;
            }
            // Tabs have no terminal width; expand so columns stay aligned.
            let text = text.replace('\t', "    ");
            match runs.last_mut() {
                Some((last, run)) if *last == token => run.push_str(&text),
                _ => runs.push((token, text)),
            }
        };
        let mut start = 0;
        for (at, op) in ops {
            let at = at.min(line.len());
            if at > start {
                emit(class(&stack), &line[start..at]);
                start = at;
            }
            if stack.apply(&op).is_err() {
                return plain();
            }
        }
        emit(class(&stack), &line[start..]);
        out.push(runs);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_gets_real_tokens() {
        let lines = highlight("rust", "// hi\nfn main() { let x = \"s\"; 42 }\n");
        let tokens: Vec<Token> = lines.iter().flatten().map(|(token, _)| *token).collect();
        for want in [Token::Comment, Token::Keyword, Token::String, Token::Number] {
            assert!(tokens.contains(&want), "missing {want:?} in {lines:?}");
        }
        let text: String = lines[1].iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(
            text, "fn main() { let x = \"s\"; 42 }",
            "no characters lost"
        );
    }

    #[test]
    fn unknown_language_is_plain() {
        assert_eq!(
            highlight("nope", "a\n\nb"),
            [
                vec![(Token::Plain, "a".into())],
                vec![(Token::Plain, "".into())],
                vec![(Token::Plain, "b".into())],
            ]
        );
    }
}
