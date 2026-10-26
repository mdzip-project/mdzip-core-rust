//! Glob filters for choosing which files to pack (the spec's create-filter
//! fixture).

/// Markdown and common image types: the default for
/// [`create_from_directory`](crate::create_from_directory).
pub const DEFAULT_FILTERS: [&str; 9] = [
    "**/*.md",
    "**/*.markdown",
    "**/*.png",
    "**/*.jpg",
    "**/*.jpeg",
    "**/*.gif",
    "**/*.webp",
    "**/*.svg",
    "**/*.avif",
];

#[derive(Debug)]
enum Token {
    /// `**/`: zero or more whole directories.
    AnyDirs,
    /// `**`: anything, including `/`.
    AnyAll,
    /// `*`: anything within one segment.
    Star,
    /// `?`: one character within a segment.
    One,
    Literal(char),
}

fn tokenize(pattern: &str) -> Vec<Token> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            if chars.get(i + 2) == Some(&'/') {
                out.push(Token::AnyDirs);
                i += 3;
            } else {
                out.push(Token::AnyAll);
                i += 2;
            }
        } else {
            out.push(match chars[i] {
                '*' => Token::Star,
                '?' => Token::One,
                c => Token::Literal(c),
            });
            i += 1;
        }
    }
    out
}

fn matches(tokens: &[Token], path: &[char]) -> bool {
    let Some((first, rest)) = tokens.split_first() else {
        return path.is_empty();
    };
    match first {
        Token::Literal(c) => path.first().is_some_and(|p| p.eq_ignore_ascii_case(c)) && matches(rest, &path[1..]),
        Token::One => path.first().is_some_and(|&p| p != '/') && matches(rest, &path[1..]),
        Token::Star => {
            let limit = path.iter().position(|&c| c == '/').unwrap_or(path.len());
            (0..=limit).any(|n| matches(rest, &path[n..]))
        }
        Token::AnyAll => (0..=path.len()).any(|n| matches(rest, &path[n..])),
        Token::AnyDirs => {
            matches(rest, path)
                || path
                    .iter()
                    .enumerate()
                    .any(|(i, &c)| c == '/' && matches(rest, &path[i + 1..]))
        }
    }
}

/// True if `path` (`/`-separated, relative) matches any of the glob `filters`.
///
/// `**/` matches zero or more directories, `**` anything, `*` and `?` within
/// one segment. Letters match ASCII case-insensitively, so `*.PNG` is an image.
pub fn matches_filters<S: AsRef<str>>(path: &str, filters: &[S]) -> bool {
    let path: Vec<char> = path.chars().collect();
    filters.iter().any(|f| matches(&tokenize(f.as_ref()), &path))
}
