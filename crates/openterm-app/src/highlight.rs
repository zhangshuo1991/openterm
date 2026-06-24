//! Syntax highlighting via syntect. Pure functions, no IO.
//!
//! Returns a flat list of `(Color, text)` spans suitable for iced `rich_text`.

use iced::Color;
use once_cell::sync::Lazy;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

static SS: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);
static TS: Lazy<ThemeSet> = Lazy::new(ThemeSet::load_defaults);

/// Detect syntax from file extension (falls back to plain text).
pub fn lang_from_ext(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "py" => "Python",
        "js" | "jsx" | "ts" | "tsx" => "JavaScript",
        "java" => "Java",
        "rs" => "Rust",
        "go" => "Go",
        "rb" => "Ruby",
        "sh" | "bash" | "zsh" => "Bash",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" => "C++",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "xml" | "html" | "htm" => "HTML",
        "css" | "scss" | "less" => "CSS",
        "sql" => "SQL",
        "md" | "markdown" => "Markdown",
        "dockerfile" => "Dockerfile",
        _ => "",
    }
}

/// Highlight `content` using the given language name (empty = plain text).
/// Returns colored spans: `(color, text_fragment)`.
pub fn highlight(content: &str, lang: &str) -> Vec<(Color, String)> {
    let syntax = if lang.is_empty() {
        SS.find_syntax_plain_text()
    } else {
        SS.find_syntax_by_name(lang)
            .or_else(|| SS.find_syntax_by_extension(lang))
            .unwrap_or_else(|| SS.find_syntax_plain_text())
    };

    let theme = TS
        .themes
        .get("base16-ocean.dark")
        .or_else(|| TS.themes.values().next())
        .unwrap();

    let mut h = HighlightLines::new(syntax, theme);
    let mut out = Vec::new();

    for line in LinesWithEndings::from(content) {
        let ranges = h.highlight_line(line, &SS).unwrap_or_default();
        for (style, text) in ranges {
            let fg = style.foreground;
            out.push((
                Color::from_rgb(
                    fg.r as f32 / 255.0,
                    fg.g as f32 / 255.0,
                    fg.b as f32 / 255.0,
                ),
                text.to_owned(),
            ));
        }
    }

    out
}
