//! Stable display tokens for pasted local files.

use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    Image,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Attachment {
    pub token: String,
    pub path: String,
    pub kind: Kind,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Insertion {
    pub text: String,
    pub attachments: Vec<Attachment>,
}

impl Kind {
    fn token(self, number: u64) -> String {
        match self {
            Self::Image => crate::i18n::attachments::image(number),
            Self::File => crate::i18n::attachments::file(number),
        }
    }
}

pub(crate) fn parse_paste(
    text: &str,
    cwd: &Path,
    next_image: u64,
    next_file: u64,
) -> Option<Insertion> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let paths = candidate_paths(&normalized)?;
    let mut image = next_image;
    let mut file = next_file;
    let mut attachments = Vec::with_capacity(paths.len());
    for raw in paths {
        let path = resolve_file(&raw, cwd)?;
        let kind = if is_image(&path) {
            Kind::Image
        } else {
            Kind::File
        };
        let number = match kind {
            Kind::Image => {
                let number = image;
                image = image.saturating_add(1);
                number
            }
            Kind::File => {
                let number = file;
                file = file.saturating_add(1);
                number
            }
        };
        attachments.push(Attachment {
            token: kind.token(number),
            path: path.to_string_lossy().into_owned(),
            kind,
        });
    }
    (!attachments.is_empty()).then(|| Insertion {
        text: attachments
            .iter()
            .map(|attachment| attachment.token.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        attachments,
    })
}

pub(crate) fn next_number(text: &str, kind: Kind) -> u64 {
    token_ranges(text)
        .into_iter()
        .filter(|range| range.kind == kind)
        .map(|range| range.number)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

pub(crate) fn expand(text: &str, attachments: &[Attachment]) -> String {
    let mut expanded = text.to_owned();
    for attachment in attachments {
        let target = format!("{}(<{}>)", attachment.token, attachment.path);
        expanded = expanded.replace(&attachment.token, &target);
    }
    expanded
}

pub(crate) fn display(text: &str) -> String {
    let ranges = token_ranges(text);
    let mut output = String::with_capacity(text.len());
    let mut copied = 0;
    for range in ranges {
        let link = &text[range.end..];
        let Some(destination) = link.strip_prefix("(<") else {
            continue;
        };
        let Some(close) = destination.find(">)") else {
            continue;
        };
        output.push_str(&text[copied..range.start]);
        output.push_str(&text[range.start..range.end]);
        copied = range.end + 2 + close + 2;
    }
    output.push_str(&text[copied..]);
    output
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TokenRange {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
    number: u64,
}

pub(crate) fn token_ranges(text: &str) -> Vec<TokenRange> {
    let mut ranges = Vec::new();
    for (prefix, kind) in [
        (crate::i18n::attachments::IMAGE_PREFIX, Kind::Image),
        (crate::i18n::attachments::FILE_PREFIX, Kind::File),
    ] {
        for (start, _) in text.match_indices(prefix) {
            let digits = &text[start + prefix.len()..];
            let length = digits
                .bytes()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            if length == 0 || digits.as_bytes().get(length) != Some(&b']') {
                continue;
            }
            let Ok(number) = digits[..length].parse() else {
                continue;
            };
            ranges.push(TokenRange {
                start,
                end: start + prefix.len() + length + 1,
                kind,
                number,
            });
        }
    }
    ranges.sort_by_key(|range| range.start);
    ranges
}

fn candidate_paths(text: &str) -> Option<Vec<String>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(path) = single_path(trimmed) {
        return Some(vec![path]);
    }
    let lines = trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(single_path)
        .collect::<Option<Vec<_>>>()
        .filter(|paths| paths.len() > 1);
    if lines.is_some() {
        return lines;
    }
    shell_words(trimmed).filter(|paths| !paths.is_empty())
}

fn single_path(text: &str) -> Option<String> {
    let words = shell_words(text.trim())?;
    if words.len() != 1 {
        return None;
    }
    let word = words.into_iter().next()?;
    let unescaped = if word.starts_with("file://") {
        file_uri(&word)?
    } else {
        word
    };
    Some(unescaped)
}

fn resolve_file(raw: &str, cwd: &Path) -> Option<PathBuf> {
    if raw.contains(['\0', '\n', '\r', '<', '>']) {
        return None;
    }
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    fs::metadata(&path).ok()?.is_file().then_some(path)
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "avif"
                    | "bmp"
                    | "gif"
                    | "heic"
                    | "heif"
                    | "jpeg"
                    | "jpg"
                    | "png"
                    | "svg"
                    | "tif"
                    | "tiff"
                    | "webp"
            )
        })
}

fn file_uri(text: &str) -> Option<String> {
    let mut path = text.strip_prefix("file://")?;
    if let Some(rest) = path.strip_prefix("localhost") {
        path = rest;
    }
    if !path.starts_with('/') {
        return None;
    }
    percent_decode(path)
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex(*bytes.get(index + 1)?)?;
            let low = hex(*bytes.get(index + 2)?)?;
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn shell_words(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in text.chars() {
        if escaped {
            word.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                word.push(character);
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(character);
        }
    }
    if escaped || quote.is_some() {
        return None;
    }
    if !word.is_empty() {
        words.push(word);
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_paths_become_stable_image_and_file_tokens() {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("reference image.png");
        let file = root.path().join("notes.md");
        fs::write(&image, b"png").unwrap();
        fs::write(&file, b"notes").unwrap();
        let pasted = format!("'{}' '{}'", image.display(), file.display());
        let insertion = parse_paste(&pasted, root.path(), 1, 1).unwrap();
        assert_eq!(insertion.text, "[Image 1] File: [File 1]");
        assert_eq!(insertion.attachments[0].path, image.to_string_lossy());
        assert_eq!(insertion.attachments[1].path, file.to_string_lossy());

        let expanded = expand(&insertion.text, &insertion.attachments);
        assert_eq!(token_ranges(&expanded).len(), 2);
        assert!(expanded.contains("[Image 1](<"));
        assert!(expanded.contains("[File 1](<"));
        assert_eq!(display(&expanded), insertion.text);
    }

    #[test]
    fn file_uris_and_shell_escaped_paths_are_understood() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("with space.png");
        fs::write(&path, b"png").unwrap();
        let escaped = path.to_string_lossy().replace(' ', "\\ ");
        assert_eq!(
            parse_paste(&escaped, root.path(), 2, 1).unwrap().text,
            "[Image 2]"
        );
        let uri = format!("file://{}", path.to_string_lossy().replace(' ', "%20"));
        assert_eq!(
            parse_paste(&uri, root.path(), 3, 1).unwrap().text,
            "[Image 3]"
        );
    }

    #[test]
    fn ordinary_text_and_missing_paths_remain_plain() {
        let root = tempfile::tempdir().unwrap();
        assert!(parse_paste("please inspect this", root.path(), 1, 1).is_none());
        assert!(parse_paste("/missing/file.png", root.path(), 1, 1).is_none());
    }
}
