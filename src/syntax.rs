//! Basic, language-agnostic syntax highlighting.
//!
//! A small hand-rolled single-pass tokenizer: good enough for "basic"
//! highlighting (keywords, strings, numbers, # and // comments) without
//! pulling in a full lexer per language. Mirrors the Python original.

use std::collections::HashSet;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorPair {
    Default,
    Keyword,
    String,
    Comment,
    Number,
}

const KEYWORD_SRC: &str = "
    def class return if elif else for while import from as try except
    finally with lambda yield break continue pass True False None and or
    not in is global nonlocal raise assert del async await self cls print

    function var let const new this typeof instanceof null undefined
    export default extends super static get set of

    int float double char void long short unsigned signed struct enum
    typedef include define public private protected virtual namespace
    template using package interface implements throws throw catch
    switch case do goto
";

fn keywords() -> &'static HashSet<&'static str> {
    static KEYWORDS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    KEYWORDS.get_or_init(|| KEYWORD_SRC.split_whitespace().collect())
}

/// Return one color pair per character in `line` (line given as chars).
pub fn get_line_colors(line: &[char]) -> Vec<ColorPair> {
    let n = line.len();
    let mut colors = vec![ColorPair::Default; n];
    let mut i = 0usize;

    while i < n {
        let c = line[i];

        // Comments run to the end of the line.
        if c == '#' || (c == '/' && i + 1 < n && line[i + 1] == '/') {
            for j in i..n {
                colors[j] = ColorPair::Comment;
            }
            break;
        }

        // Strings: single or double quoted, naive escape handling.
        if c == '\'' || c == '"' {
            let quote = c;
            let start = i;
            i += 1;
            while i < n {
                if line[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if line[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            for j in start..i.min(n) {
                colors[j] = ColorPair::String;
            }
            continue;
        }

        // Numbers.
        if c.is_ascii_digit() {
            let start = i;
            while i < n && (line[i].is_ascii_digit() || line[i] == '.') {
                i += 1;
            }
            for j in start..i {
                colors[j] = ColorPair::Number;
            }
            continue;
        }

        // Words: identifiers / keywords.
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < n && (line[i].is_alphanumeric() || line[i] == '_') {
                i += 1;
            }
            let word: String = line[start..i].iter().collect();
            if keywords().contains(word.as_str()) {
                for j in start..i {
                    colors[j] = ColorPair::Keyword;
                }
            }
            continue;
        }

        i += 1;
    }

    colors
}
