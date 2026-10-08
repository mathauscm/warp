//! Makes the code editor look like the user's VS Code: the color theme set in
//! VS Code's `workbench.colorTheme` and its editor font settings.
//!
//! Everything is read once, the first time a code editor is created. When VS
//! Code or the theme can't be found, the editor keeps Warp's own look.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pathfinder_color::ColorU;
use serde_json::Value;
use warpui::fonts::FamilyId;
use warpui::{AppContext, SingletonEntity};

/// VS Code's defaults on macOS when `editor.fontFamily`/`fontSize`/`lineHeight`
/// are not set.
const DEFAULT_FONT_FAMILY: &str = "Menlo";
const DEFAULT_FONT_SIZE: f32 = 12.;
const DEFAULT_LINE_HEIGHT_RATIO: f32 = 1.5;

/// Tree-sitter capture names emitted by the highlight queries, mapped to the
/// TextMate scope VS Code themes use for the same kind of token.
const CAPTURE_SCOPES: &[(&str, &str)] = &[
    ("attribute", "entity.other.attribute-name"),
    ("boolean", "constant.language"),
    ("comment", "comment"),
    ("comment.documentation", "comment.block.documentation"),
    ("constant", "variable.other.constant"),
    ("constant.builtin", "constant.language"),
    ("constructor", "support.class"),
    ("escape", "constant.character.escape"),
    ("function", "entity.name.function"),
    ("function.builtin", "support.function"),
    ("function.macro", "entity.name.function"),
    ("function.method", "entity.name.function"),
    ("keyword", "keyword"),
    ("keyword.conditional", "keyword.control"),
    ("keyword.function", "storage.type.function"),
    ("keyword.import", "keyword.control.import"),
    ("keyword.operator", "keyword.operator"),
    ("keyword.repeat", "keyword.control"),
    ("keyword.return", "keyword.control"),
    ("label", "entity.name.label"),
    ("module", "entity.name.module"),
    ("namespace", "entity.name.namespace"),
    ("number", "constant.numeric"),
    ("operator", "keyword.operator"),
    ("property", "variable.other.property"),
    ("punctuation", "punctuation"),
    ("punctuation.bracket", "punctuation.definition.block"),
    ("punctuation.delimiter", "punctuation.separator"),
    (
        "punctuation.special",
        "punctuation.definition.template-expression",
    ),
    ("string", "string"),
    ("string.escape", "constant.character.escape"),
    ("string.regex", "string.regexp"),
    ("string.regexp", "string.regexp"),
    ("string.special", "string"),
    ("string.special.key", "support.type.property-name.json"),
    ("tag", "entity.name.tag"),
    ("type", "entity.name.type"),
    ("type.builtin", "support.type.primitive"),
    ("variable", "variable"),
    ("variable.builtin", "variable.language"),
    ("variable.member", "variable.other.property"),
    ("variable.parameter", "variable.parameter"),
];

/// Editor colors taken from a VS Code theme.
#[derive(Debug, Clone)]
pub struct EditorPalette {
    pub background: ColorU,
    pub foreground: ColorU,
    pub line_number: ColorU,
    pub line_number_active: ColorU,
    pub line_highlight: Option<ColorU>,
    pub selection: Option<ColorU>,
    pub cursor: Option<ColorU>,
    pub gutter_added: Option<ColorU>,
    pub gutter_modified: Option<ColorU>,
    pub gutter_deleted: Option<ColorU>,
    /// Color per tree-sitter capture name (see [`CAPTURE_SCOPES`]).
    pub captures: HashMap<String, ColorU>,
    pub git_decorations: GitDecorationColors,
}

/// The theme's `gitDecoration.*ResourceForeground` colors, used by the file tree.
#[derive(Debug, Clone, Default)]
pub struct GitDecorationColors {
    pub modified: Option<ColorU>,
    pub added: Option<ColorU>,
    pub renamed: Option<ColorU>,
    pub untracked: Option<ColorU>,
    pub deleted: Option<ColorU>,
    pub conflicting: Option<ColorU>,
}

#[derive(Debug, Default)]
pub struct EditorAppearance {
    pub palette: Option<EditorPalette>,
    pub font_family: Option<FamilyId>,
    pub font_size: Option<f32>,
    pub line_height_ratio: Option<f32>,
}

static EDITOR_APPEARANCE: OnceLock<EditorAppearance> = OnceLock::new();

/// Loads the VS Code look on first use. Needs the app context to load the font.
pub fn ensure_loaded(ctx: &mut AppContext) {
    if EDITOR_APPEARANCE.get().is_some() {
        return;
    }
    let appearance = load(ctx);
    let _ = EDITOR_APPEARANCE.set(appearance);
}

/// The loaded VS Code look, if [`ensure_loaded`] ran.
pub fn get() -> Option<&'static EditorAppearance> {
    EDITOR_APPEARANCE.get()
}

pub fn palette() -> Option<&'static EditorPalette> {
    get().and_then(|appearance| appearance.palette.as_ref())
}

fn load(ctx: &mut AppContext) -> EditorAppearance {
    let Some(settings) = read_vscode_settings() else {
        return EditorAppearance::default();
    };

    let palette = settings
        .get("workbench.colorTheme")
        .and_then(Value::as_str)
        .and_then(find_theme_file)
        .and_then(|path| read_theme(&path))
        .map(|theme| palette_from_theme(&theme));

    let font_size = settings
        .get("editor.fontSize")
        .and_then(Value::as_f64)
        .map(|size| size as f32)
        .filter(|size| *size > 0.)
        .unwrap_or(DEFAULT_FONT_SIZE);
    let line_height_ratio = match settings.get("editor.lineHeight").and_then(Value::as_f64) {
        // VS Code treats small values as a multiplier and larger ones as pixels.
        Some(value) if value > 0. && value < 8. => value as f32,
        Some(value) if value >= 8. => value as f32 / font_size,
        _ => DEFAULT_LINE_HEIGHT_RATIO,
    };
    let font_family = settings
        .get("editor.fontFamily")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_FONT_FAMILY)
        .split(',')
        .map(|name| name.trim().trim_matches(|c| c == '\'' || c == '"'))
        .filter(|name| !name.is_empty() && *name != "monospace")
        .chain(std::iter::once(DEFAULT_FONT_FAMILY))
        .find_map(|name| load_font(name, ctx));

    EditorAppearance {
        palette,
        font_family,
        font_size: Some(font_size),
        line_height_ratio: Some(line_height_ratio),
    }
}

#[cfg(not(target_family = "wasm"))]
fn load_font(name: &str, ctx: &mut AppContext) -> Option<FamilyId> {
    warpui::fonts::Cache::handle(ctx).update(ctx, |font_cache: &mut warpui::fonts::Cache, _| {
        font_cache.get_or_load_system_font(name).ok()
    })
}

#[cfg(target_family = "wasm")]
fn load_font(_name: &str, _ctx: &mut AppContext) -> Option<FamilyId> {
    None
}

fn vscode_user_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join("Library/Application Support/Code/User"))
        .filter(|dir| dir.is_dir())
        .or_else(|| Some(home.join(".config/Code/User")).filter(|dir| dir.is_dir()))
}

fn read_vscode_settings() -> Option<serde_json::Map<String, Value>> {
    let path = vscode_user_dir()?.join("settings.json");
    match read_jsonc(&path)? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// Finds the theme file of the extension that contributes the theme `label`.
fn find_theme_file(label: &str) -> Option<PathBuf> {
    let extensions = dirs::home_dir()?.join(".vscode/extensions");
    std::fs::read_dir(extensions)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find_map(|extension| {
            let manifest = read_jsonc(&extension.join("package.json"))?;
            let themes = manifest.pointer("/contributes/themes")?.as_array()?;
            themes.iter().find_map(|theme| {
                let matches = theme.get("label").and_then(Value::as_str) == Some(label)
                    || theme.get("id").and_then(Value::as_str) == Some(label);
                let path = theme.get("path").and_then(Value::as_str)?;
                matches.then(|| extension.join(path))
            })
        })
}

/// Theme colors and token rules, following `include` to a parent theme.
#[derive(Default)]
struct Theme {
    colors: HashMap<String, String>,
    token_rules: Vec<(Vec<String>, String)>,
}

fn read_theme(path: &Path) -> Option<Theme> {
    read_theme_with_depth(path, 0)
}

fn read_theme_with_depth(path: &Path, depth: usize) -> Option<Theme> {
    let json = read_jsonc(path)?;
    let mut theme = json
        .get("include")
        .and_then(Value::as_str)
        .filter(|_| depth < 4)
        .and_then(|include| read_theme_with_depth(&path.parent()?.join(include), depth + 1))
        .unwrap_or_default();

    if let Some(colors) = json.get("colors").and_then(Value::as_object) {
        for (key, value) in colors {
            if let Some(value) = value.as_str() {
                theme.colors.insert(key.clone(), value.to_owned());
            }
        }
    }
    for rule in json
        .get("tokenColors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(foreground) = rule.pointer("/settings/foreground").and_then(Value::as_str) else {
            continue;
        };
        let selectors = match rule.get("scope") {
            Some(Value::String(scopes)) => scopes.split(',').map(str::to_owned).collect(),
            Some(Value::Array(scopes)) => scopes
                .iter()
                .filter_map(Value::as_str)
                .flat_map(|scopes| scopes.split(','))
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        };
        let selectors: Vec<String> = selectors
            .into_iter()
            .map(|selector| selector.trim().to_owned())
            // Descendant selectors ("a b") need the full scope stack, which
            // tree-sitter captures don't have.
            .filter(|selector| !selector.is_empty() && !selector.contains(' '))
            .collect();
        if !selectors.is_empty() {
            theme.token_rules.push((selectors, foreground.to_owned()));
        }
    }
    Some(theme)
}

fn palette_from_theme(theme: &Theme) -> EditorPalette {
    let color = |key: &str| theme.colors.get(key).and_then(|value| parse_color(value));
    let foreground = color("editor.foreground").unwrap_or(ColorU::new(212, 212, 212, 255));
    let captures = CAPTURE_SCOPES
        .iter()
        .filter_map(|(capture, scope)| {
            let color = scope_color(theme, scope)?;
            Some(((*capture).to_owned(), color))
        })
        .collect();

    EditorPalette {
        background: color("editor.background").unwrap_or(ColorU::new(30, 30, 30, 255)),
        foreground,
        line_number: color("editorLineNumber.foreground").unwrap_or(foreground),
        line_number_active: color("editorLineNumber.activeForeground").unwrap_or(foreground),
        line_highlight: color("editor.lineHighlightBackground"),
        selection: color("editor.selectionBackground"),
        cursor: color("editorCursor.foreground"),
        gutter_added: color("editorGutter.addedBackground"),
        gutter_modified: color("editorGutter.modifiedBackground"),
        gutter_deleted: color("editorGutter.deletedBackground"),
        captures,
        git_decorations: GitDecorationColors {
            modified: color("gitDecoration.modifiedResourceForeground"),
            added: color("gitDecoration.addedResourceForeground"),
            renamed: color("gitDecoration.renamedResourceForeground"),
            untracked: color("gitDecoration.untrackedResourceForeground"),
            deleted: color("gitDecoration.deletedResourceForeground"),
            conflicting: color("gitDecoration.conflictingResourceForeground"),
        },
    }
}

/// Picks the rule whose selector is the longest prefix of `scope`, like VS
/// Code does; later rules win ties.
fn scope_color(theme: &Theme, scope: &str) -> Option<ColorU> {
    let mut best: Option<(usize, &str)> = None;
    for (selectors, foreground) in &theme.token_rules {
        for selector in selectors {
            let matches = scope == selector
                || scope
                    .strip_prefix(selector.as_str())
                    .is_some_and(|rest| rest.starts_with('.'));
            if !matches {
                continue;
            }
            let depth = selector.split('.').count();
            if best.is_none_or(|(best_depth, _)| depth >= best_depth) {
                best = Some((depth, foreground));
            }
        }
    }
    best.and_then(|(_, foreground)| parse_color(foreground))
}

/// Parses `#rgb`, `#rgba`, `#rrggbb` and `#rrggbbaa`.
fn parse_color(value: &str) -> Option<ColorU> {
    let hex = value.trim().strip_prefix('#')?;
    let expanded: String = match hex.len() {
        3 | 4 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 | 8 => hex.to_owned(),
        _ => return None,
    };
    let channel = |index: usize| u8::from_str_radix(expanded.get(index..index + 2)?, 16).ok();
    let alpha = if expanded.len() == 8 {
        channel(6)?
    } else {
        255
    };
    Some(ColorU::new(channel(0)?, channel(2)?, channel(4)?, alpha))
}

fn read_jsonc(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&strip_jsonc(&text)).ok()
}

/// Removes comments and trailing commas so VS Code's JSON-with-comments files
/// parse as JSON.
fn strip_jsonc(text: &str) -> String {
    let mut without_comments = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            without_comments.push(c);
            if c == '\\' {
                if let Some(escaped) = chars.next() {
                    without_comments.push(escaped);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                without_comments.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        without_comments.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut previous = ' ';
                for c in chars.by_ref() {
                    if previous == '*' && c == '/' {
                        break;
                    }
                    previous = c;
                }
            }
            _ => without_comments.push(c),
        }
    }

    // Drop commas that are followed only by whitespace and a closing bracket.
    let mut result = String::with_capacity(without_comments.len());
    let mut in_string = false;
    let chars: Vec<char> = without_comments.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if in_string {
            result.push(c);
            if c == '\\' {
                if let Some(next) = chars.get(index + 1) {
                    result.push(*next);
                    index += 1;
                }
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            result.push(c);
        } else if c == ',' {
            let next = chars[index + 1..].iter().find(|c| !c.is_whitespace());
            if !matches!(next, Some('}') | Some(']')) {
                result.push(c);
            }
        } else {
            result.push(c);
        }
        index += 1;
    }
    result
}

#[cfg(test)]
#[path = "vscode_appearance_tests.rs"]
mod tests;
