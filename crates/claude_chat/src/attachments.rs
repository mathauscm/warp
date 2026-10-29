//! Files attached to a user message, other than the images sent inline.
//!
//! PDFs go as base64 `document` blocks, which the API reads natively. Every other file is listed
//! in an `<attached_files>` block at the end of the message text: text files and folders as
//! Claude Code `@` mentions, which the CLI expands into the file's contents (or the folder's
//! listing) itself, and binary files (Office documents, archives, too-large PDFs, ...) as plain
//! absolute paths, which Claude opens with its tools. PDFs are listed too, so Claude knows where
//! the document it sees lives, and the block is what the transcript shows as file chips, for live
//! and reloaded messages alike.

use std::path::Path;

pub const ATTACHED_FILES_OPEN: &str = "<attached_files>";
pub const ATTACHED_FILES_CLOSE: &str = "</attached_files>";

/// Larger images and PDFs are referenced by path instead of being sent inline.
pub const MAX_INLINE_BYTES: u64 = 20 * 1024 * 1024;

/// What kind of file an attachment is, for its icon and for how it's sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Pdf,
    /// Plain text: prose, markup, code, data.
    Text,
    /// A word-processing document, like `.docx`.
    Document,
    Spreadsheet,
    Presentation,
    Folder,
    Other,
}

/// How an attached file reaches Claude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDelivery {
    /// A base64 `image` block.
    Image,
    /// A base64 `document` block.
    Document,
    /// An `@` mention, which the CLI expands.
    Mention,
    /// The absolute path, for Claude to open with its tools.
    Path,
}

#[rustfmt::skip]
const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "text", "md", "markdown", "mdx", "rst", "adoc", "org", "tex", "html", "htm", "xhtml",
    "css", "scss", "sass", "less", "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "json",
    "jsonc", "json5", "ndjson", "jsonl", "yaml", "yml", "toml", "xml", "svg", "csv", "tsv", "log",
    "ini", "cfg", "conf", "env", "properties", "sh", "bash", "zsh", "fish", "ps1", "bat", "cmd",
    "py", "pyi", "ipynb", "rb", "go", "rs", "java", "kt", "kts", "swift", "c", "h", "cpp", "hpp",
    "cc", "cxx", "hh", "m", "mm", "cs", "fs", "php", "pl", "lua", "r", "sql", "graphql", "gql",
    "proto", "vue", "svelte", "astro", "dart", "scala", "ex", "exs", "erl", "hs", "clj", "elm",
    "zig", "nim", "jl", "gradle", "tf", "hcl", "nix", "diff", "patch", "lock", "srt", "vtt",
];
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];
const DOCUMENT_EXTENSIONS: &[&str] = &["doc", "docx", "odt", "rtf", "pages", "epub"];
const SPREADSHEET_EXTENSIONS: &[&str] = &["xls", "xlsx", "xlsm", "ods", "numbers", "csv", "tsv"];
const PRESENTATION_EXTENSIONS: &[&str] = &["ppt", "pptx", "odp", "key"];

fn extension(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
}

/// The media type of an image Claude can see inline, by its extension.
pub fn image_media_type(path: &str) -> Option<&'static str> {
    match extension(path)?.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// The kind of the file at `path`, by its extension.
pub fn classify(path: &str, is_dir: bool) -> AttachmentKind {
    if is_dir {
        return AttachmentKind::Folder;
    }
    let Some(extension) = extension(path) else {
        return AttachmentKind::Other;
    };
    let extension = extension.as_str();
    if IMAGE_EXTENSIONS.contains(&extension) {
        AttachmentKind::Image
    } else if extension == "pdf" {
        AttachmentKind::Pdf
    } else if SPREADSHEET_EXTENSIONS.contains(&extension) {
        AttachmentKind::Spreadsheet
    } else if TEXT_EXTENSIONS.contains(&extension) {
        AttachmentKind::Text
    } else if DOCUMENT_EXTENSIONS.contains(&extension) {
        AttachmentKind::Document
    } else if PRESENTATION_EXTENSIONS.contains(&extension) {
        AttachmentKind::Presentation
    } else {
        AttachmentKind::Other
    }
}

/// Whether the file's extension says it's plain text.
pub fn has_text_extension(path: &str) -> bool {
    extension(path).is_some_and(|extension| TEXT_EXTENSIONS.contains(&extension.as_str()))
}

/// Whether the start of a file looks like text: valid UTF-8 (allowing a character cut at the end)
/// without NUL bytes.
pub fn looks_like_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(error) => error.error_len().is_none(),
    }
}

/// How a file of `kind` and `size` bytes is sent. `is_text` says whether its contents are plain
/// text, from its extension or a sniff of its first bytes.
pub fn delivery(kind: AttachmentKind, size: u64, is_text: bool) -> FileDelivery {
    let fits_inline = size <= MAX_INLINE_BYTES;
    match kind {
        AttachmentKind::Folder => FileDelivery::Mention,
        AttachmentKind::Image if fits_inline => FileDelivery::Image,
        AttachmentKind::Pdf if fits_inline => FileDelivery::Document,
        AttachmentKind::Image | AttachmentKind::Pdf => FileDelivery::Path,
        AttachmentKind::Text
        | AttachmentKind::Document
        | AttachmentKind::Spreadsheet
        | AttachmentKind::Presentation
        | AttachmentKind::Other => {
            if is_text {
                FileDelivery::Mention
            } else {
                FileDelivery::Path
            }
        }
    }
}

/// One line of the `<attached_files>` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedFile {
    /// Absolute path.
    pub path: String,
    pub is_dir: bool,
    /// Listed as an `@` mention, so the CLI attaches its contents.
    pub is_mention: bool,
}

/// How `file` is written in a message: an `@` mention (quoted when the path has whitespace) or
/// the plain path, with a folder's trailing `/`.
pub fn listed_line(file: &ListedFile) -> String {
    let mut path = file.path.trim_end_matches('/').to_owned();
    if file.is_dir || path.is_empty() {
        path.push('/');
    }
    // A mention can't hold a quote, so such a path is just listed.
    if !file.is_mention || path.contains('"') {
        return path;
    }
    if path.chars().any(char::is_whitespace) {
        format!("@\"{path}\"")
    } else {
        format!("@{path}")
    }
}

/// `text` followed by the `<attached_files>` block listing `files`, or `text` as is when there
/// are none.
pub fn message_with_files(text: &str, files: &[ListedFile]) -> String {
    if files.is_empty() {
        return text.to_owned();
    }
    let mut message = text.trim_end().to_owned();
    if !message.is_empty() {
        message.push_str("\n\n");
    }
    message.push_str(ATTACHED_FILES_OPEN);
    for file in files {
        message.push('\n');
        message.push_str(&listed_line(file));
    }
    message.push('\n');
    message.push_str(ATTACHED_FILES_CLOSE);
    message
}

/// A file listed in a message's `<attached_files>` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedFile {
    /// Absolute path, without a folder's trailing `/`.
    pub path: String,
    /// The last path component.
    pub name: String,
    pub kind: AttachmentKind,
}

/// Splits a user message into the text the user typed and the files listed in its trailing
/// `<attached_files>` block. A message without such a block comes back whole, with no files.
pub fn split_attached_files(text: &str) -> (&str, Vec<AttachedFile>) {
    let trimmed = text.trim_end();
    let Some(inner) = trimmed.strip_suffix(ATTACHED_FILES_CLOSE) else {
        return (text, Vec::new());
    };
    let Some(open) = inner.rfind(ATTACHED_FILES_OPEN) else {
        return (text, Vec::new());
    };
    let files = inner[open + ATTACHED_FILES_OPEN.len()..]
        .lines()
        .filter_map(parse_listed_line)
        .collect();
    (inner[..open].trim_end(), files)
}

/// Reads a path written by [`listed_line`].
pub fn parse_listed_line(line: &str) -> Option<AttachedFile> {
    let line = line.trim();
    let path = match line.strip_prefix('@') {
        Some(mention) => mention
            .strip_prefix('"')
            .and_then(|quoted| quoted.strip_suffix('"'))
            .unwrap_or(mention),
        None => line,
    };
    if path.is_empty() {
        return None;
    }
    let is_dir = path.len() > 1 && path.ends_with('/');
    let path = if is_dir {
        path.trim_end_matches('/')
    } else {
        path
    };
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_owned();
    Some(AttachedFile {
        kind: classify(path, is_dir),
        path: path.to_owned(),
        name,
    })
}

#[cfg(test)]
#[path = "attachments_tests.rs"]
mod tests;
