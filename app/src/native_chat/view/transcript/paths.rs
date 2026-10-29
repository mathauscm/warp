//! Files and folders Claude names in its answers, as inline code or links to local paths that
//! exist, drawn as chips that open them.

use std::ops::Range;
use std::path::{Path, PathBuf};

use std::sync::Arc;

use markdown_parser::{
    FormattedText, FormattedTextFragment, FormattedTextInline, FormattedTextLine, Hyperlink,
};

use super::markdown::Block;

/// The room before a chip's name that its icon is painted over. Figure spaces don't let a line
/// break between the icon and the name.
pub(super) const CHIP_LEAD: &str = "\u{2007}\u{2007}\u{202f}";
/// The room after a chip's name, so its border doesn't touch the last letter.
pub(super) const CHIP_TAIL: &str = "\u{202f}";
/// Longer spans of inline code are prose, not paths.
const MAX_CANDIDATE_CHARS: usize = 512;

const CODE_EXTENSIONS: &[&str] = &[
    "rs",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "py",
    "go",
    "java",
    "kt",
    "kts",
    "swift",
    "c",
    "h",
    "cc",
    "cpp",
    "hpp",
    "cs",
    "rb",
    "php",
    "sh",
    "bash",
    "zsh",
    "fish",
    "toml",
    "yaml",
    "yml",
    "json",
    "jsonc",
    "json5",
    "md",
    "mdx",
    "markdown",
    "txt",
    "log",
    "csv",
    "tsv",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "xml",
    "sql",
    "graphql",
    "gql",
    "vue",
    "svelte",
    "lua",
    "dart",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "scala",
    "r",
    "pl",
    "ini",
    "cfg",
    "conf",
    "env",
    "lock",
    "wgsl",
    "glsl",
    "proto",
    "tf",
    "nix",
    "zig",
    "gradle",
    "plist",
    "properties",
    "diff",
    "patch",
];
/// Files without an extension that are text.
const CODE_FILE_NAMES: &[&str] = &[
    "makefile",
    "dockerfile",
    "gemfile",
    "rakefile",
    "procfile",
    "license",
    "readme",
    "changelog",
    "codeowners",
];
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "tif", "tiff", "heic", "heif", "ico", "avif",
];
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "mkv", "webm", "avi", "wmv", "flv", "mpg", "mpeg", "3gp",
];
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "m4a", "aac", "flac", "ogg", "oga", "opus", "aiff", "aif", "wma",
];
const ARCHIVE_EXTENSIONS: &[&str] = &[
    "zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "zst", "dmg", "jar", "whl",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathKind {
    Folder,
    Image,
    Video,
    Audio,
    Pdf,
    /// Code and other text, which opens in the app's editor.
    Code,
    Archive,
    File,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PathChip {
    pub path: PathBuf,
    pub kind: PathKind,
    /// The prose line the chip is on.
    pub line: usize,
    /// The chip's characters in its line, icon room included.
    pub chars: Range<usize>,
}

/// What kind of file `path` is, from its extension.
pub(super) fn kind_of(path: &Path, is_dir: bool) -> PathKind {
    if is_dir {
        return PathKind::Folder;
    }
    let Some(extension) = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
    else {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        // Dotfiles like `.env` and `.gitignore` have no extension to Rust.
        return if name.starts_with('.') || CODE_FILE_NAMES.contains(&name.as_str()) {
            PathKind::Code
        } else {
            PathKind::File
        };
    };
    let extension = extension.as_str();
    if extension == "pdf" {
        PathKind::Pdf
    } else if IMAGE_EXTENSIONS.contains(&extension) {
        PathKind::Image
    } else if VIDEO_EXTENSIONS.contains(&extension) {
        PathKind::Video
    } else if AUDIO_EXTENSIONS.contains(&extension) {
        PathKind::Audio
    } else if ARCHIVE_EXTENSIONS.contains(&extension) {
        PathKind::Archive
    } else if CODE_EXTENSIONS.contains(&extension) {
        PathKind::Code
    } else {
        PathKind::File
    }
}

/// The path `text` names, without a `file://` scheme or a trailing `:line[:column]`, when it reads
/// like one. Whether something is there is for the caller to check.
pub(super) fn path_candidate(text: &str) -> Option<&str> {
    let text = text.trim();
    let text = text.strip_prefix("file://").unwrap_or(text);
    let text = strip_line_suffix(text);
    if text.is_empty()
        || text.chars().count() > MAX_CANDIDATE_CHARS
        || text == "."
        || text == ".."
        || text.starts_with('-')
        || text.contains("://")
    {
        return None;
    }
    if text.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '(' | ')'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '<'
                    | '>'
                    | '='
                    | ';'
                    | '|'
                    | '$'
                    | '*'
                    | '?'
                    | '"'
                    | '\''
                    | '`'
                    | ','
            )
    }) {
        return None;
    }
    // A bare word like `renders` can name a folder; words with spaces only count with a
    // separator or an extension, so commands like `cargo build` aren't looked up.
    let looks_like_path = text.contains('/')
        || text.starts_with('~')
        || text.contains('.')
        || !text.contains(char::is_whitespace);
    looks_like_path.then_some(text)
}

/// `text` without a trailing `:12` or `:12:3`, as in `src/main.rs:12`.
fn strip_line_suffix(text: &str) -> &str {
    let mut rest = text;
    for _ in 0..2 {
        match rest.rsplit_once(':') {
            Some((head, tail))
                if !head.is_empty()
                    && !tail.is_empty()
                    && tail.chars().all(|c| c.is_ascii_digit()) =>
            {
                rest = head;
            }
            Some(_) | None => break,
        }
    }
    rest
}

/// Whether a link's target is a local path rather than a web address.
fn is_local_link(url: &str) -> bool {
    url.starts_with('/')
        || url.starts_with("~/")
        || url.starts_with("./")
        || url.starts_with("../")
        || url.starts_with("file://")
}

/// `candidate` as an absolute path: under `home` for `~/`, under `cwd` when relative.
pub(super) fn resolve(candidate: &str, cwd: &Path, home: Option<&Path>) -> PathBuf {
    if candidate == "~" {
        if let Some(home) = home {
            return home.to_path_buf();
        }
    }
    if let (Some(rest), Some(home)) = (candidate.strip_prefix("~/"), home) {
        return home.join(rest);
    }
    let path = Path::new(candidate);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// A path's last component, which the chip shows.
pub(super) fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Removes the room chips keep around their names from copied text.
pub(super) fn strip_chip_room(text: &str) -> String {
    text.chars()
        .filter(|character| !matches!(character, '\u{2007}' | '\u{202f}'))
        .collect()
}

/// Turns the paths that `blocks`' prose names into chips, quotes included.
pub(super) fn add_chips(
    blocks: &mut [Block],
    cwd: &Path,
    home: Option<&Path>,
    lookup: &mut impl FnMut(&Path) -> Option<bool>,
) {
    for block in blocks {
        match block {
            Block::Prose(text, chips) => {
                if let Some((chipped, found)) = with_chips(text, cwd, home, &mut *lookup) {
                    *text = Arc::new(chipped);
                    *chips = found;
                }
            }
            Block::Quote(blocks) => add_chips(blocks, cwd, home, lookup),
            Block::Code { .. } | Block::Table(_) => {}
        }
    }
}

/// `text` with its inline code and local links that name existing files or folders turned into
/// chips, and where those chips are; `None` when nothing in it names one. `lookup` says whether
/// an absolute path is a folder, or `None` when nothing is there.
pub(super) fn with_chips(
    text: &FormattedText,
    cwd: &Path,
    home: Option<&Path>,
    mut lookup: impl FnMut(&Path) -> Option<bool>,
) -> Option<(FormattedText, Vec<PathChip>)> {
    let mut chips = Vec::new();
    let mut text = text.clone();
    for (line_index, line) in text.lines.iter_mut().enumerate() {
        let Some(fragments) = inline_fragments_mut(line) else {
            continue;
        };
        let mut chars = 0;
        for fragment in fragments.iter_mut() {
            let start = chars;
            if let Some((path, kind, label)) = chip_target(fragment, cwd, home, &mut lookup) {
                *fragment = FormattedTextFragment {
                    text: format!("{CHIP_LEAD}{label}{CHIP_TAIL}"),
                    styles: Default::default(),
                };
                let end = start + fragment.text.chars().count();
                chips.push(PathChip {
                    path,
                    kind,
                    line: line_index,
                    chars: start..end,
                });
            }
            chars += fragment.text.chars().count();
        }
    }
    (!chips.is_empty()).then_some((text, chips))
}

/// The path a fragment names, its kind and the label its chip shows.
fn chip_target(
    fragment: &FormattedTextFragment,
    cwd: &Path,
    home: Option<&Path>,
    lookup: &mut impl FnMut(&Path) -> Option<bool>,
) -> Option<(PathBuf, PathKind, String)> {
    let (candidate, label) = match &fragment.styles.hyperlink {
        Some(Hyperlink::Url(url)) if is_local_link(url) => {
            let label = fragment.text.trim();
            (
                path_candidate(url)?,
                (label != url.as_str()).then(|| label.to_owned()),
            )
        }
        Some(Hyperlink::Url(_) | Hyperlink::Action(_)) => return None,
        None if fragment.styles.inline_code => (path_candidate(&fragment.text)?, None),
        None => return None,
    };
    let path = resolve(candidate, cwd, home);
    let is_dir = lookup(&path)?;
    let label = label
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| display_name(&path));
    Some((path.clone(), kind_of(&path, is_dir), label))
}

fn inline_fragments_mut(line: &mut FormattedTextLine) -> Option<&mut FormattedTextInline> {
    match line {
        FormattedTextLine::Heading(header) => Some(&mut header.text),
        FormattedTextLine::Line(fragments) => Some(fragments),
        FormattedTextLine::OrderedList(list) => Some(&mut list.indented_text.text),
        FormattedTextLine::UnorderedList(list) => Some(&mut list.text),
        FormattedTextLine::TaskList(list) => Some(&mut list.text),
        FormattedTextLine::CodeBlock(_)
        | FormattedTextLine::LineBreak
        | FormattedTextLine::HorizontalRule
        | FormattedTextLine::Embedded(_)
        | FormattedTextLine::Image(_)
        | FormattedTextLine::Table(_) => None,
    }
}
