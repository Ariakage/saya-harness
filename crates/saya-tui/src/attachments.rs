//! Stable display tokens for pasted local files.

use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    Image,
    Audio,
    Video,
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
            Self::Audio => crate::i18n::attachments::audio(number),
            Self::Video => crate::i18n::attachments::video(number),
            Self::File => crate::i18n::attachments::file(number),
        }
    }
}

pub(crate) fn image_attachment(path: &Path, number: u64) -> Option<Attachment> {
    let path = path.to_string_lossy();
    (!path.contains(['\0', '\n', '\r', '<', '>'])).then(|| Attachment {
        token: Kind::Image.token(number),
        path: path.into_owned(),
        kind: Kind::Image,
    })
}

pub(crate) fn parse_paste(text: &str, cwd: &Path, current: &str) -> Option<Insertion> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let paths = candidate_paths(&normalized)?;
    let mut next = [
        next_number(current, Kind::Image),
        next_number(current, Kind::Audio),
        next_number(current, Kind::Video),
        next_number(current, Kind::File),
    ];
    let mut attachments = Vec::with_capacity(paths.len());
    for raw in paths {
        let path = resolve_file(&raw, cwd)?;
        let kind = classify(&path);
        let index = kind as usize;
        let number = next[index];
        next[index] = next[index].saturating_add(1);
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
    for range in atomic_ranges(text).into_iter().rev() {
        let token = &text[range.start..range.end];
        if let Some(attachment) = attachments.iter().find(|item| item.token == token) {
            expanded.replace_range(
                range.start..range.end,
                &format!("{}(<{}>)", attachment.token, attachment.path),
            );
        }
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
        (crate::i18n::attachments::AUDIO_PREFIX, Kind::Audio),
        (crate::i18n::attachments::VIDEO_PREFIX, Kind::Video),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AtomicRange {
    pub start: usize,
    pub end: usize,
}

pub(crate) fn atomic_ranges(text: &str) -> Vec<AtomicRange> {
    token_ranges(text)
        .into_iter()
        .map(|range| {
            let owner = match range.kind {
                Kind::Image => None,
                Kind::Audio => Some(crate::i18n::attachments::AUDIO_OWNER),
                Kind::Video => Some(crate::i18n::attachments::VIDEO_OWNER),
                Kind::File => Some(crate::i18n::attachments::FILE_OWNER),
            };
            let start = if let Some(owner) = owner {
                if text[..range.start].ends_with(owner) {
                    range.start - owner.len()
                } else {
                    range.start
                }
            } else {
                range.start
            };
            AtomicRange {
                start,
                end: range.end,
            }
        })
        .collect()
}

pub(crate) fn backspace_range(text: &str, cursor: usize) -> Option<AtomicRange> {
    atomic_ranges(text)
        .into_iter()
        .find(|range| range.start < cursor && cursor <= range.end)
}

pub(crate) fn delete_range(text: &str, cursor: usize) -> Option<AtomicRange> {
    atomic_ranges(text)
        .into_iter()
        .find(|range| range.start <= cursor && cursor < range.end)
}

pub(crate) fn move_left_target(text: &str, cursor: usize) -> Option<usize> {
    atomic_ranges(text)
        .into_iter()
        .find(|range| range.start < cursor && cursor <= range.end)
        .map(|range| range.start)
}

pub(crate) fn move_right_target(text: &str, cursor: usize) -> Option<usize> {
    atomic_ranges(text)
        .into_iter()
        .find(|range| range.start <= cursor && cursor < range.end)
        .map(|range| range.end)
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

fn classify(path: &Path) -> Kind {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .map_or(Kind::File, |extension| match extension.as_str() {
            "avif" | "bmp" | "gif" | "heic" | "heif" | "jpeg" | "jpg" | "png" | "svg" | "tif"
            | "tiff" | "webp" => Kind::Image,
            "aac" | "flac" | "m4a" | "mp3" | "ogg" | "opus" | "wav" => Kind::Audio,
            "avi" | "m4v" | "mkv" | "mov" | "mp4" | "webm" => Kind::Video,
            _ => Kind::File,
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
        let insertion = parse_paste(&pasted, root.path(), "").unwrap();
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
            parse_paste(&escaped, root.path(), "[Image 1]")
                .unwrap()
                .text,
            "[Image 2]"
        );
        let uri = format!("file://{}", path.to_string_lossy().replace(' ', "%20"));
        assert_eq!(
            parse_paste(&uri, root.path(), "[Image 1] [Image 2]")
                .unwrap()
                .text,
            "[Image 3]"
        );
    }

    #[test]
    fn ordinary_text_and_missing_paths_remain_plain() {
        let root = tempfile::tempdir().unwrap();
        assert!(parse_paste("please inspect this", root.path(), "").is_none());
        assert!(parse_paste("/missing/file.png", root.path(), "").is_none());
    }

    #[test]
    fn audio_and_video_files_use_distinct_tokens() {
        let root = tempfile::tempdir().unwrap();
        let audio = root.path().join("audio.mp3");
        let video = root.path().join("video.mp4");
        fs::write(&audio, b"mp3").unwrap();
        fs::write(&video, b"mp4").unwrap();
        let insertion = parse_paste(
            &format!("'{}' '{}'", audio.display(), video.display()),
            root.path(),
            "",
        )
        .unwrap();
        assert_eq!(insertion.text, "Audio: [Audio 1] Video: [Video 1]");
        assert_eq!(insertion.attachments[0].kind, Kind::Audio);
        assert_eq!(insertion.attachments[1].kind, Kind::Video);
    }

    #[test]
    fn expansion_does_not_replace_a_shorter_token_inside_a_longer_one() {
        let attachments = vec![
            Attachment {
                token: "[Image 1]".into(),
                path: "/one.png".into(),
                kind: Kind::Image,
            },
            Attachment {
                token: "[Image 10]".into(),
                path: "/ten.png".into(),
                kind: Kind::Image,
            },
        ];
        assert_eq!(
            expand("[Image 1] [Image 10]", &attachments),
            "[Image 1](</one.png>) [Image 10](</ten.png>)"
        );
    }

    #[test]
    fn attachment_tokens_are_atomic_for_navigation_and_deletion() {
        let text = "before [Image 1] File: [File 2] after";
        let image = text.find("[Image 1]").unwrap();
        let file = text.find("File: [File 2]").unwrap();
        assert_eq!(
            atomic_ranges(text),
            vec![
                AtomicRange {
                    start: image,
                    end: image + "[Image 1]".len(),
                },
                AtomicRange {
                    start: file,
                    end: file + "File: [File 2]".len(),
                },
            ]
        );
        assert_eq!(move_left_target(text, image + 4), Some(image));
        assert_eq!(
            move_right_target(text, file + 2),
            Some(file + "File: [File 2]".len())
        );
        assert_eq!(
            backspace_range(text, image + 4),
            Some(AtomicRange {
                start: image,
                end: image + "[Image 1]".len(),
            })
        );
        assert_eq!(
            delete_range(text, file + 2),
            Some(AtomicRange {
                start: file,
                end: file + "File: [File 2]".len(),
            })
        );
    }
}
