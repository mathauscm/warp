//! Highlighting for a standalone piece of code, like a fenced block in a chat message, without a
//! buffer or an editor.

use std::ops::Range;

use arborium::tree_sitter::{Parser, QueryCursor};
use rangemap::RangeMap;
use streaming_iterator::StreamingIterator;
use warpui_core::color::ColorU;

use crate::queries::highlight_query::{ColorMap, HighlightQuery, TextSlice};

/// Above this size code is shown without highlighting.
const MAX_SNIPPET_BYTES: usize = 256 * 1024;

/// The colored character ranges of `code` written in `language` (a name or alias such as `rust`,
/// `ts` or `bash`), in order and without overlaps. Empty when the language is unknown.
pub fn highlight_snippet(
    language: &str,
    code: &str,
    color_map: ColorMap,
) -> Vec<(Range<usize>, ColorU)> {
    if code.len() > MAX_SNIPPET_BYTES {
        return Vec::new();
    }
    let Some(language) = languages::language_by_name(&language.trim().to_ascii_lowercase()) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&language.grammar).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(code, None) else {
        return Vec::new();
    };
    let query = &language.highlight_query;
    let highlights = HighlightQuery::new(query, color_map);

    let mut char_of_byte = vec![0usize; code.len() + 1];
    let mut chars = 0;
    for (byte, char) in code.char_indices() {
        for offset in 0..char.len_utf8() {
            char_of_byte[byte + offset] = chars;
        }
        chars += 1;
    }
    char_of_byte[code.len()] = chars;

    let mut colors = RangeMap::new();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(query, tree.root_node(), TextSlice(code.as_bytes()));
    while let Some((query_match, _)) = captures.next() {
        for capture in query_match.captures {
            let Some(color) = highlights.color_for_capture(capture.index) else {
                continue;
            };
            let bytes = capture.node.byte_range();
            let start = char_of_byte[bytes.start.min(code.len())];
            let end = char_of_byte[bytes.end.min(code.len())];
            if start < end {
                colors.insert(start..end, color);
            }
        }
    }
    colors.into_iter().collect()
}
