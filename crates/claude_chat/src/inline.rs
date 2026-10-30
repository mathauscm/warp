//! Attachments placed inside a message's text.
//!
//! The composer marks where each attachment goes with a token like `[Imagem 1]`. Sending turns
//! the draft into content blocks in that order: an image or PDF follows its label, a file's path
//! (or the `@` mention the CLI expands) stays inside its label, a quoted mention becomes a Markdown
//! quote and a long paste a `<pasted_text>` block. The labels stay in the text as anchors, so the
//! transcript lays each attachment out where it was put, for live and reloaded messages alike.

use std::collections::{HashSet, VecDeque};
use std::ops::Range;

use crate::attachments::{self, AttachedFile, ListedFile};
use crate::protocol::{OutgoingBlock, OutgoingDocument, OutgoingImage};

/// Pastes with more characters or lines than these go in as a token instead of their text.
pub const LARGE_PASTE_CHARS: usize = 800;
pub const LARGE_PASTE_LINES: usize = 12;

const PASTED_OPEN: &str = "<pasted_text";
const PASTED_CLOSE: &str = "</pasted_text>";
/// Longer numbers in brackets are just text.
const MAX_NUMBER_DIGITS: usize = 4;
/// Joins the words of a token in the composer, so a line never wraps inside a short one.
const NO_BREAK_SPACE: char = '\u{a0}';
/// How many earlier states of a draft [`LinkHistory`] keeps.
const MAX_SAVED_LINKS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Image,
    File,
    Mention,
    Pasted,
}

impl TokenKind {
    const ALL: [TokenKind; 4] = [
        TokenKind::Image,
        TokenKind::File,
        TokenKind::Mention,
        TokenKind::Pasted,
    ];

    fn label(self) -> &'static str {
        match self {
            TokenKind::Image => "Imagem",
            TokenKind::File => "Arquivo",
            TokenKind::Mention => "Menção",
            TokenKind::Pasted => "Texto colado",
        }
    }

    /// What comes between the number and the detail, for kinds whose token has one.
    fn detail_separator(self) -> Option<&'static str> {
        match self {
            TokenKind::Image | TokenKind::Mention => None,
            TokenKind::File => Some(": "),
            TokenKind::Pasted => Some(" · "),
        }
    }
}

/// Which attachment a token stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenId {
    pub kind: TokenKind,
    /// The number in the token's label.
    pub number: usize,
}

/// A token found in a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub id: TokenId,
    /// Byte range, brackets included.
    pub range: Range<usize>,
}

/// The token of an attachment; `detail` is shown after the number for files and pastes.
pub fn token_text(id: TokenId, detail: &str) -> String {
    let mut token = format!("[{} {}", id.kind.label(), id.number);
    if let Some(separator) = id.kind.detail_separator() {
        token.push_str(separator);
        token.extend(detail.chars().map(|character| match character {
            '[' => '(',
            ']' => ')',
            '\n' | '\r' => ' ',
            character => character,
        }));
    }
    token.push(']');
    token
}

/// The detail of a pasted text's token, like `42 linhas`.
pub fn pasted_detail(text: &str) -> String {
    lines_detail(line_count(text))
}

fn lines_detail(lines: usize) -> String {
    match lines {
        1 => "1 linha".to_owned(),
        lines => format!("{lines} linhas"),
    }
}

pub fn line_count(text: &str) -> usize {
    text.trim_end_matches(['\n', '\r']).lines().count().max(1)
}

/// Whether a paste is long enough to go in as a token.
pub fn is_large_paste(text: &str) -> bool {
    text.chars().count() > LARGE_PASTE_CHARS || line_count(text) > LARGE_PASTE_LINES
}

/// The tokens in `text`, in order.
pub fn find_tokens(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut from = 0;
    while let Some(offset) = text[from..].find('[') {
        let start = from + offset;
        match parse_token(&text[start..]) {
            Some(token) => {
                tokens.push(Token {
                    id: token.id,
                    range: start..start + token.len,
                });
                from = start + token.len;
            }
            None => from = start + 1,
        }
    }
    tokens
}

/// The token in the composer's editor: [`token_text`] with no-break spaces, so it wraps whole.
pub fn editor_token_text(id: TokenId, detail: &str) -> String {
    token_text(id, detail).replace(' ', &NO_BREAK_SPACE.to_string())
}

/// A token at the start of a text, with byte ranges relative to it.
struct ParsedToken {
    id: TokenId,
    len: usize,
    number: Range<usize>,
    detail: Option<Range<usize>>,
}

/// The token `text` starts with. Its spaces may be no-break spaces, as in the composer.
fn parse_token(text: &str) -> Option<ParsedToken> {
    let rest = text.strip_prefix('[')?;
    TokenKind::ALL.iter().find_map(|&kind| {
        let rest = strip_spaced(strip_spaced(rest, kind.label())?, " ")?;
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 || digits > MAX_NUMBER_DIGITS {
            return None;
        }
        let number_start = text.len() - rest.len();
        let number = rest[..digits].parse().ok()?;
        let mut after = &rest[digits..];
        let mut detail = None;
        if let Some(separator) = kind.detail_separator() {
            let rest = strip_spaced(after, separator)?;
            let end = rest.find([']', '[', '\n'])?;
            if rest[..end].trim().is_empty() {
                return None;
            }
            let detail_start = text.len() - rest.len();
            detail = Some(detail_start..detail_start + end);
            after = &rest[end..];
        }
        after.starts_with(']').then(|| ParsedToken {
            id: TokenId { kind, number },
            len: text.len() - after.len() + 1,
            number: number_start..number_start + digits,
            detail,
        })
    })
}

/// `text` without `prefix`, each space of which may also be a no-break space.
fn strip_spaced<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let mut rest = text.chars();
    for expected in prefix.chars() {
        let actual = rest.next()?;
        if actual != expected && !(expected == ' ' && actual == NO_BREAK_SPACE) {
            return None;
        }
    }
    Some(rest.as_str())
}

/// The detail of a token, like a file's name or path.
fn token_detail(token_text: &str) -> Option<&str> {
    parse_token(token_text)?
        .detail
        .map(|range| &token_text[range])
}

/// `text` with the numbers of its tokens changed by `renumber`; tokens it returns `None` for are
/// left as they are.
pub fn renumber_tokens(text: &str, renumber: impl Fn(TokenId) -> Option<usize>) -> String {
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;
    for token in find_tokens(text) {
        let Some(number) = renumber(token.id) else {
            continue;
        };
        let Some(parsed) = parse_token(&text[token.range.start..]) else {
            continue;
        };
        result.push_str(&text[cursor..token.range.start + parsed.number.start]);
        result.push_str(&number.to_string());
        cursor = token.range.start + parsed.number.end;
    }
    result.push_str(&text[cursor..]);
    result
}

/// When going from `old` to `new` deleted part of a token (but not all of it), the token and the
/// range of `new` left of it, so the token can be deleted whole.
pub fn token_remnant(old: &str, new: &str) -> Option<(TokenId, Range<usize>)> {
    if new.len() >= old.len() {
        return None;
    }
    let prefix = common_prefix_len(old, new);
    let suffix = common_suffix_len(&old[prefix..], &new[prefix..]);
    if prefix + suffix != new.len() {
        return None;
    }
    let deleted = prefix..old.len() - suffix;
    find_tokens(old).into_iter().find_map(|token| {
        let Range { start, end } = token.range;
        let overlaps = deleted.start < end && deleted.end > start;
        let is_whole = deleted.start <= start && deleted.end >= end;
        (overlaps && !is_whole).then(|| {
            let remnant = start.min(deleted.start)..deleted.start + end.saturating_sub(deleted.end);
            (token.id, remnant)
        })
    })
}

/// Where a selection goes so that neither end is inside one of `tokens`, or `None` when it's
/// fine. A cursor jumps past the token the way it was moving (`previous` is where it was), or else
/// to the nearer edge; a selection grows to cover the tokens it cuts. Offsets are bytes, as
/// `(tail, head)`.
pub fn snap_selection(
    tokens: &[Token],
    tail: usize,
    head: usize,
    previous: Option<usize>,
) -> Option<(usize, usize)> {
    let inside = |offset: usize| {
        tokens
            .iter()
            .find(|token| token.range.start < offset && offset < token.range.end)
    };
    if tail == head {
        let Range { start, end } = inside(head)?.range.clone();
        let to = match previous {
            Some(previous) if previous <= start => end,
            Some(previous) if previous >= end => start,
            _ if head - start <= end - head => start,
            _ => end,
        };
        return Some((to, to));
    }
    let (mut start, mut end) = (tail.min(head), tail.max(head));
    if let Some(token) = inside(start) {
        start = token.range.start;
    }
    if let Some(token) = inside(end) {
        end = token.range.end;
    }
    let snapped = if tail <= head {
        (start, end)
    } else {
        (end, start)
    };
    (snapped != (tail, head)).then_some(snapped)
}

/// When going from `old` to `new` only inserted text, and inside one of `tokens` (of `old`),
/// `new` with that text moved to just after the token, and where the cursor then goes.
pub fn move_insertion_out_of_token(
    old: &str,
    new: &str,
    tokens: &[Token],
) -> Option<(String, usize)> {
    if new.len() <= old.len() {
        return None;
    }
    let prefix = common_prefix_len(old, new);
    let suffix = common_suffix_len(&old[prefix..], &new[prefix..]);
    if prefix + suffix != old.len() {
        return None;
    }
    let inserted = &new[prefix..new.len() - suffix];
    let token = tokens
        .iter()
        .find(|token| token.range.start < prefix && prefix < token.range.end)?;
    let end = token.range.end;
    let fixed = format!("{}{inserted}{}", &old[..end], &old[end..]);
    Some((fixed, end + inserted.len()))
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum()
}

fn common_suffix_len(a: &str, b: &str) -> usize {
    a.chars()
        .rev()
        .zip(b.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum()
}

// --- linking ----------------------------------------------------------------------------------

/// An attachment of a draft and the token that places it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Link {
    /// Tells the attachment apart from the others while numbers change.
    pub key: u64,
    pub id: TokenId,
}

/// The keys of the `links` whose token isn't in `text`.
pub fn unlinked(text: &str, links: &[Link]) -> Vec<u64> {
    let present: HashSet<TokenId> = find_tokens(text)
        .into_iter()
        .map(|token| token.id)
        .collect();
    links
        .iter()
        .filter(|link| !present.contains(&link.id))
        .map(|link| link.key)
        .collect()
}

/// Earlier texts of a draft with the attachments they had, so that undoing an edit that removed
/// attachments, or renumbered them, brings them back as they were.
#[derive(Debug, Default)]
pub struct LinkHistory {
    /// The text the attachments last matched, with their links then.
    current: Option<(String, Vec<Link>)>,
    saved: VecDeque<(String, Vec<Link>)>,
}

impl LinkHistory {
    /// Notes that the attachments of `text` are `links`.
    pub fn note(&mut self, text: &str, links: Vec<Link>) {
        self.current = Some((text.to_owned(), links));
    }

    /// Keeps the last noted state, from before an edit that's about to remove attachments, and
    /// `text` with `links`, the state right after it.
    pub fn save(&mut self, text: &str, links: Vec<Link>) {
        if let Some(current) = self.current.take() {
            self.push(current);
        }
        self.push((text.to_owned(), links.clone()));
        self.current = Some((text.to_owned(), links));
    }

    fn push(&mut self, state: (String, Vec<Link>)) {
        if state.1.is_empty() {
            return;
        }
        self.saved.retain(|(text, _)| *text != state.0);
        if self.saved.len() == MAX_SAVED_LINKS {
            self.saved.pop_front();
        }
        self.saved.push_back(state);
    }

    /// The links `text` had when it was saved, if the attachments with `keys` were all among
    /// them: an attachment added since makes an equal text a different draft.
    pub fn find(&self, text: &str, keys: &[u64]) -> Option<&[Link]> {
        self.saved
            .iter()
            .rev()
            .find(|(saved, links)| {
                saved == text
                    && keys
                        .iter()
                        .all(|key| links.iter().any(|link| link.key == *key))
            })
            .map(|(_, links)| links.as_slice())
    }

    /// Whether some saved state has the attachment with `key`.
    pub fn remembers(&self, key: u64) -> bool {
        self.saved
            .iter()
            .any(|(_, links)| links.iter().any(|link| link.key == key))
    }

    pub fn clear(&mut self) {
        self.current = None;
        self.saved.clear();
    }
}

/// The new number of each of `links` whose kind is in `kinds`, so that each kind is numbered 1, 2,
/// 3… in the order it was.
pub fn compact_numbers(links: &[Link], kinds: &[TokenKind]) -> Vec<(u64, usize)> {
    kinds
        .iter()
        .flat_map(|&kind| {
            let mut of_kind: Vec<&Link> =
                links.iter().filter(|link| link.id.kind == kind).collect();
            of_kind.sort_by_key(|link| link.id.number);
            of_kind
                .into_iter()
                .enumerate()
                .map(|(index, link)| (link.key, index + 1))
                .collect::<Vec<_>>()
        })
        .collect()
}

// --- sending ----------------------------------------------------------------------------------

/// What an attachment of a draft holds.
#[derive(Debug, Clone, Copy)]
pub enum DraftContent<'a> {
    Image(&'a OutgoingImage),
    File {
        file: &'a ListedFile,
        /// The base64 block of a file sent as a document, like a PDF.
        document: Option<&'a OutgoingDocument>,
    },
    /// Text quoted from the conversation.
    Mention(&'a str),
    Pasted(&'a str),
}

#[derive(Debug, Clone, Copy)]
pub struct DraftAttachment<'a> {
    pub id: TokenId,
    pub content: DraftContent<'a>,
}

/// A draft turned into what's sent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComposedMessage {
    pub blocks: Vec<OutgoingBlock>,
    /// The text blocks joined, which is how the conversation shows the message.
    pub text: String,
    pub images: Vec<OutgoingImage>,
}

/// Turns the composer's `text` and `attachments` into a message, each attachment where its token
/// is (the first one, when a token was copied). Attachments whose token was deleted follow the
/// text, and tokens whose attachment was removed are dropped. Images, files and pastes are
/// numbered again in the order they're sent.
pub fn compose(text: &str, attachments: &[DraftAttachment]) -> ComposedMessage {
    let mut composer = Composer::default();
    let mut placed = HashSet::new();
    let mut cursor = 0;
    for token in find_tokens(text) {
        composer.push_text(&text[cursor..token.range.start]);
        cursor = token.range.end;
        if placed.insert(token.id)
            && let Some(attachment) = attachments
                .iter()
                .find(|attachment| attachment.id == token.id)
        {
            composer.place(attachment.content);
        }
    }
    composer.push_text(&text[cursor..]);
    for attachment in attachments {
        if !placed.contains(&attachment.id) {
            composer.break_paragraph();
            composer.place(attachment.content);
        }
    }
    composer.finish()
}

#[derive(Default)]
struct Composer {
    blocks: Vec<OutgoingBlock>,
    /// Text since the last image or document.
    pending: String,
    text: String,
    images: Vec<OutgoingImage>,
    files: usize,
    pastes: usize,
    /// Set after a quote or paste, whose blank line the next text's leading space would widen.
    after_block: bool,
}

impl Composer {
    fn push_text(&mut self, text: &str) {
        let text = if self.after_block {
            text.trim_start()
        } else {
            text
        };
        if !text.is_empty() {
            self.after_block = false;
            self.push_raw(text);
        }
    }

    fn push_raw(&mut self, text: &str) {
        self.pending.push_str(text);
        self.text.push_str(text);
    }

    /// Ends the paragraph so far with a blank line.
    fn break_paragraph(&mut self) {
        let kept = self.pending.trim_end().len();
        let cut = self.pending.len() - kept;
        self.pending.truncate(kept);
        self.text.truncate(self.text.len() - cut);
        if !self.text.trim().is_empty() && !self.text.ends_with("\n\n") {
            self.push_raw("\n\n");
        }
        self.after_block = false;
    }

    fn flush(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        if !pending.trim().is_empty() {
            self.blocks.push(OutgoingBlock::Text(pending));
        }
    }

    fn place(&mut self, content: DraftContent) {
        self.after_block = false;
        match content {
            DraftContent::Image(image) => {
                self.images.push(image.clone());
                self.push_raw(&format!("[Imagem {}]", self.images.len()));
                self.flush();
                self.blocks.push(OutgoingBlock::Image(image.clone()));
            }
            DraftContent::File { file, document } => {
                self.files += 1;
                self.push_raw(&format!(
                    "[Arquivo {}: {}]",
                    self.files,
                    attachments::listed_line(file)
                ));
                if let Some(document) = document {
                    self.flush();
                    self.blocks.push(OutgoingBlock::Document(document.clone()));
                }
            }
            DraftContent::Mention(quote) => {
                self.break_paragraph();
                self.push_raw(&quote_block(quote));
                self.push_raw("\n\n");
                self.after_block = true;
            }
            DraftContent::Pasted(text) => {
                self.pastes += 1;
                self.break_paragraph();
                self.push_raw(&pasted_block(self.pastes, text));
                self.push_raw("\n\n");
                self.after_block = true;
            }
        }
    }

    fn finish(mut self) -> ComposedMessage {
        self.flush();
        if let Some(OutgoingBlock::Text(text)) = self.blocks.first_mut() {
            *text = text.trim_start().to_owned();
        }
        if let Some(OutgoingBlock::Text(text)) = self.blocks.last_mut() {
            *text = text.trim_end().to_owned();
        }
        ComposedMessage {
            blocks: self.blocks,
            text: self.text.trim().to_owned(),
            images: self.images,
        }
    }
}

/// `text` as a Markdown quote.
pub fn quote_block(text: &str) -> String {
    text.trim_matches('\n')
        .lines()
        .map(|line| {
            let line = line.trim_end();
            if line.is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn pasted_block(number: usize, text: &str) -> String {
    let text = text.trim_matches(['\n', '\r']);
    format!(
        "{PASTED_OPEN} label=\"Texto colado {number}\" lines=\"{}\">\n{text}\n{PASTED_CLOSE}",
        line_count(text)
    )
}

// --- showing ----------------------------------------------------------------------------------

/// A piece of a sent user message, in the order it was written.
#[derive(Debug, Clone, PartialEq)]
pub enum MessagePart {
    Text(String),
    /// The message's image at this index.
    Image(usize),
    File(AttachedFile),
    Quote(String),
    Pasted {
        text: String,
        lines: usize,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageLayout {
    pub parts: Vec<MessagePart>,
    /// Indexes of the images no label in the text places.
    pub loose_images: Vec<usize>,
}

/// Splits a user message's text into its parts, placing its `image_count` images at their
/// `[Imagem N]` labels.
pub fn message_layout(text: &str, image_count: usize) -> MessageLayout {
    let mut layout = MessageLayout::default();
    let mut placed = vec![false; image_count];
    let mut rest = text;
    while let Some((range, part)) = find_pasted(rest) {
        push_inline_parts(&mut layout.parts, &rest[..range.start], &mut placed);
        layout.parts.push(part);
        rest = &rest[range.end..];
    }
    push_inline_parts(&mut layout.parts, rest, &mut placed);
    layout.loose_images = placed
        .iter()
        .enumerate()
        .filter(|(_, is_placed)| !**is_placed)
        .map(|(index, _)| index)
        .collect();
    layout
}

/// The text of laid-out `parts` with each attachment as its token, numbered in order (images by
/// their index), and the character ranges of the tokens.
pub fn text_with_tokens(parts: &[MessagePart]) -> (String, Vec<(Range<usize>, TokenKind)>) {
    let mut text = String::new();
    let mut tokens = Vec::new();
    let (mut files, mut mentions, mut pastes) = (0, 0, 0);
    for part in parts {
        let (piece, kind) = match part {
            MessagePart::Text(prose) => (prose.clone(), None),
            MessagePart::Image(index) => (
                token_text(id(TokenKind::Image, index + 1), ""),
                Some(TokenKind::Image),
            ),
            MessagePart::File(file) => {
                files += 1;
                (
                    token_text(id(TokenKind::File, files), &file.name),
                    Some(TokenKind::File),
                )
            }
            MessagePart::Quote(_) => {
                mentions += 1;
                (
                    token_text(id(TokenKind::Mention, mentions), ""),
                    Some(TokenKind::Mention),
                )
            }
            MessagePart::Pasted { lines, .. } => {
                pastes += 1;
                (
                    token_text(id(TokenKind::Pasted, pastes), &lines_detail(*lines)),
                    Some(TokenKind::Pasted),
                )
            }
        };
        if piece.is_empty() {
            continue;
        }
        let glued = piece.starts_with(['.', ',', ';', ':', '!', '?', ')'])
            || text.is_empty()
            || text.ends_with(char::is_whitespace);
        if !glued {
            text.push(' ');
        }
        let start = text.chars().count();
        text.push_str(&piece);
        if let Some(kind) = kind {
            tokens.push((start..start + piece.chars().count(), kind));
        }
    }
    (text, tokens)
}

fn id(kind: TokenKind, number: usize) -> TokenId {
    TokenId { kind, number }
}

/// The text with each pasted block shown as its token, for one-line previews like titles.
pub fn display_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    let mut number = 0;
    while let Some((range, part)) = find_pasted(rest) {
        number += 1;
        result.push_str(&rest[..range.start]);
        if let MessagePart::Pasted { lines, .. } = part {
            result.push_str(&token_text(
                TokenId {
                    kind: TokenKind::Pasted,
                    number,
                },
                &lines_detail(lines),
            ));
        }
        rest = &rest[range.end..];
    }
    result.push_str(rest);
    result
}

/// The first `<pasted_text>` block in `text`.
fn find_pasted(text: &str) -> Option<(Range<usize>, MessagePart)> {
    let mut from = 0;
    loop {
        let open = from + text[from..].find(PASTED_OPEN)?;
        let after_name = open + PASTED_OPEN.len();
        if !text[after_name..].starts_with([' ', '>']) {
            from = after_name;
            continue;
        }
        let tag_end = after_name + text[after_name..].find('>')?;
        let content_start = tag_end + 1;
        let close = content_start + text[content_start..].find(PASTED_CLOSE)?;
        let content = &text[content_start..close];
        let content = content.strip_prefix('\n').unwrap_or(content);
        let content = content.strip_suffix('\n').unwrap_or(content);
        let lines = attribute(&text[after_name..tag_end], "lines")
            .and_then(|lines| lines.parse().ok())
            .unwrap_or_else(|| line_count(content));
        return Some((
            open..close + PASTED_CLOSE.len(),
            MessagePart::Pasted {
                text: content.to_owned(),
                lines,
            },
        ));
    }
}

fn attribute<'a>(attributes: &'a str, name: &str) -> Option<&'a str> {
    let start = attributes.find(&format!("{name}=\""))? + name.len() + 2;
    let end = start + attributes[start..].find('"')?;
    Some(&attributes[start..end])
}

/// Adds the parts of a stretch of text without pasted blocks. Labels inside quotes were quoted
/// from earlier messages, so they're text, not this message's attachments.
fn push_inline_parts(parts: &mut Vec<MessagePart>, text: &str, placed: &mut [bool]) {
    let mut cursor = 0;
    for range in quote_paragraph_ranges(text) {
        push_anchored_parts(parts, &text[cursor..range.start], placed);
        push_prose(parts, &text[range.clone()]);
        cursor = range.end;
    }
    push_anchored_parts(parts, &text[cursor..], placed);
}

/// The byte ranges of the paragraphs whose lines are all Markdown quotes.
fn quote_paragraph_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    // Start, end and whether every line so far is a quote.
    let mut current: Option<(usize, usize, bool)> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        if line.trim().is_empty() {
            if let Some((start, end, true)) = current.take() {
                ranges.push(start..end);
            }
            continue;
        }
        let is_quote = line.trim_start().starts_with('>');
        current = Some(match current {
            Some((start, _, all_quotes)) => (start, offset, all_quotes && is_quote),
            None => (start, offset, is_quote),
        });
    }
    if let Some((start, end, true)) = current {
        ranges.push(start..end);
    }
    ranges
}

/// Adds the parts of text without quotes, placing images and files at their labels.
fn push_anchored_parts(parts: &mut Vec<MessagePart>, text: &str, placed: &mut [bool]) {
    let mut cursor = 0;
    for token in find_tokens(text) {
        let token_text = &text[token.range.clone()];
        let part = match token.id.kind {
            TokenKind::Image => token
                .id
                .number
                .checked_sub(1)
                .filter(|&index| placed.get(index) == Some(&false))
                .map(|index| {
                    placed[index] = true;
                    MessagePart::Image(index)
                }),
            TokenKind::File => token_detail(token_text)
                .filter(|detail| looks_like_path(detail))
                .and_then(attachments::parse_listed_line)
                .map(MessagePart::File),
            TokenKind::Mention | TokenKind::Pasted => None,
        };
        if let Some(part) = part {
            push_prose(parts, &text[cursor..token.range.start]);
            parts.push(part);
            cursor = token.range.end;
        }
    }
    push_prose(parts, &text[cursor..]);
}

/// Whether a file token's detail is the path a sent message has, rather than the name the
/// composer shows.
fn looks_like_path(detail: &str) -> bool {
    let path = detail.trim_start_matches('@').trim_start_matches('"');
    path.starts_with('/') || path.starts_with('~') || path.chars().nth(1) == Some(':')
}

/// Adds prose, with its paragraphs of Markdown quotes as quotes.
fn push_prose(parts: &mut Vec<MessagePart>, text: &str) {
    let mut prose: Vec<String> = Vec::new();
    let flush = |parts: &mut Vec<MessagePart>, prose: &mut Vec<String>| {
        if !prose.is_empty() {
            parts.push(MessagePart::Text(prose.join("\n\n")));
            prose.clear();
        }
    };
    for paragraph in paragraphs(text) {
        if paragraph
            .iter()
            .all(|line| line.trim_start().starts_with('>'))
        {
            let quote = paragraph
                .iter()
                .map(|line| {
                    let line = line.trim_start();
                    line.strip_prefix("> ")
                        .or_else(|| line.strip_prefix('>'))
                        .unwrap_or(line)
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !quote.trim().is_empty() {
                flush(parts, &mut prose);
                parts.push(MessagePart::Quote(quote.trim_matches('\n').to_owned()));
            }
        } else {
            prose.push(paragraph.join("\n").trim().to_owned());
        }
    }
    flush(parts, &mut prose);
}

/// The runs of non-blank lines of `text`.
fn paragraphs(text: &str) -> Vec<Vec<&str>> {
    let mut paragraphs = Vec::new();
    let mut current = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    paragraphs
}

#[cfg(test)]
#[path = "inline_tests.rs"]
mod tests;
