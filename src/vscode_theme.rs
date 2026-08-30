//! VS Code colour theme import (#350): turn a `.json` theme into a croft
//! `[[themes]]` manifest.
//!
//! croft ships fifteen themes as data; VS Code has tens of thousands, and a
//! user switching editors arrives with one they already like. The formats are
//! close enough to convert and different enough that the conversion is a real
//! mapping rather than a rename: VS Code names hundreds of workbench keys and
//! colours code by TextMate scope, while a croft theme is a small fixed
//! palette (chrome, the 16 ANSI slots, and eight syntax roles).
//!
//! # What this does NOT do
//!
//! No extension code is fetched or run. This reads a JSON file the user
//! already has. Downloading a `.vsix` from the marketplace is deliberately
//! out of scope here (still open on #350): it is a network + archive path
//! with its own trust questions, and it is not needed to convert a theme.
//!
//! # Fidelity
//!
//! A converted theme is an approximation of a design made against a much
//! larger surface, and the two places it shows are recorded rather than
//! hidden: keys VS Code has and croft does not are dropped, and croft slots
//! the source theme never filled are derived from the ones it did. Every
//! derivation is reported in [`Converted::notes`] so the user can see which
//! colours were chosen for them rather than by their theme's author.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// How deep an `include` chain may go. Themes legitimately extend a base
/// (`dark_plus` includes `dark_vs`), and a cycle would otherwise hang.
const MAX_INCLUDE_DEPTH: usize = 8;

/// The raw shape of a VS Code theme file. Every field is optional: themes in
/// the wild omit any of them, and an import that refused the file would be
/// less useful than one that derives what is missing and says so.
#[derive(Debug, Default, Deserialize)]
struct RawTheme {
    #[serde(default)]
    name: Option<String>,
    /// `"dark"`, `"light"`, or `"hc"` variants. Decides the fallbacks.
    #[serde(rename = "type", default)]
    kind: Option<String>,
    /// A relative path to a base theme this one extends.
    #[serde(default)]
    include: Option<String>,
    #[serde(default)]
    colors: BTreeMap<String, String>,
    #[serde(default)]
    token_colors: Vec<TokenColor>,
    /// `semanticTokenColors`: a map of semantic token type to colour (or to
    /// an object carrying one). Consulted only where `tokenColors` left a
    /// role empty, since TextMate scopes are what most themes actually
    /// design against.
    #[serde(default)]
    semantic: BTreeMap<String, Value>,
    /// `tokenColors` entries croft could not read, counted so the import can
    /// say so rather than quietly colouring less than the theme asked for.
    #[serde(skip)]
    dropped_rules: usize,
}

/// One `tokenColors` rule. `scope` is a string or a list of them, and both
/// spellings appear in popular themes.
#[derive(Debug, Clone, Deserialize)]
struct TokenColor {
    #[serde(default)]
    scope: ScopeField,
    #[serde(default)]
    settings: TokenSettings,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TokenSettings {
    #[serde(default)]
    foreground: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ScopeField {
    One(String),
    Many(Vec<String>),
}

impl Default for ScopeField {
    fn default() -> Self {
        // A rule with no scope is the theme's DEFAULT rule (the editor
        // foreground), which is a real and common case, not a malformed one.
        ScopeField::Many(Vec::new())
    }
}

impl ScopeField {
    /// The scopes this rule claims, with VS Code's comma-separated spelling
    /// (`"a, b"` inside one string) expanded.
    fn scopes(&self) -> Vec<String> {
        let raw: Vec<&str> = match self {
            ScopeField::One(s) => vec![s.as_str()],
            ScopeField::Many(v) => v.iter().map(String::as_str).collect(),
        };
        raw.iter()
            .flat_map(|s| s.split(','))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// The result of a conversion: the manifest to write, plus what had to be
/// invented along the way.
#[derive(Debug, Clone)]
pub struct Converted {
    /// Stable theme id, also the extension directory name.
    pub id: String,
    /// Human-facing label for the theme picker.
    pub label: String,
    /// The complete `extension.toml` to write.
    pub manifest: String,
    /// Everything the user should know about this conversion: colours croft
    /// had to derive, an id that had to change, rules that could not be read.
    /// Shown by the CLI, because each is a choice croft made on the user's
    /// behalf, and silently making them is how an import "works" while
    /// looking wrong.
    pub notes: Vec<String>,
}

/// An RGB triple. Alpha is resolved at parse time rather than carried.
type Rgb = (u8, u8, u8);

/// Parse `#rgb`, `#rrggbb`, or `#rrggbbaa`, compositing any alpha over `over`.
///
/// VS Code's selection and overlay colours are routinely translucent
/// (`#264f4a80`), and croft's palette is opaque. Dropping the alpha would
/// hand back a colour far more saturated than the one the theme's author
/// designed, so the channel is composited against the surface the colour
/// actually sits on, which is what a reader sees in VS Code.
fn parse_color(raw: &str, over: Rgb) -> Option<Rgb> {
    let s = raw.trim().strip_prefix('#')?;
    // Byte lengths and byte-range slicing below, so a non-ASCII value would
    // index mid-character and panic: "#\u{20ac}" is one character and THREE
    // bytes, so it reaches the three-digit arm and slices `&s[0..1]`. A
    // colour is hex digits by definition, so anything else is simply not one.
    if !s.is_ascii() {
        return None;
    }
    let hex = |b: &str| u8::from_str_radix(b, 16).ok();
    let (r, g, b, a) = match s.len() {
        3 => {
            let d = |i: usize| hex(&s[i..i + 1]).map(|v| v * 17);
            (d(0)?, d(1)?, d(2)?, 255)
        }
        4 => {
            let d = |i: usize| hex(&s[i..i + 1]).map(|v| v * 17);
            (d(0)?, d(1)?, d(2)?, d(3)?)
        }
        6 => (hex(&s[0..2])?, hex(&s[2..4])?, hex(&s[4..6])?, 255),
        8 => (
            hex(&s[0..2])?,
            hex(&s[2..4])?,
            hex(&s[4..6])?,
            hex(&s[6..8])?,
        ),
        _ => return None,
    };
    if a == 255 {
        return Some((r, g, b));
    }
    let blend = |c: u8, base: u8| {
        let c = c as u32 * a as u32 + base as u32 * (255 - a as u32);
        (c / 255) as u8
    };
    Some((blend(r, over.0), blend(g, over.1), blend(b, over.2)))
}

fn hex_of(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// Move `c` toward white (positive) or black (negative) by `amount` of the
/// remaining distance. Used only for slots the source theme never named.
fn shade(c: Rgb, amount: f32) -> Rgb {
    let f = |v: u8| {
        let v = v as f32;
        let target = if amount >= 0.0 { 255.0 } else { 0.0 };
        (v + (target - v) * amount.abs()).clamp(0.0, 255.0) as u8
    };
    (f(c.0), f(c.1), f(c.2))
}

/// Load a theme file and every file it `include`s, base first.
fn load_chain(path: &Path, depth: usize, out: &mut Vec<RawTheme>) -> Result<()> {
    if depth > MAX_INCLUDE_DEPTH {
        return Err(anyhow!(
            "include chain deeper than {MAX_INCLUDE_DEPTH} files (a cycle?)"
        ));
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let theme = parse_theme(&raw).with_context(|| format!("parsing {}", path.display()))?;
    if let Some(inc) = theme.include.clone() {
        // Relative to the including file, and relative ONLY: an absolute
        // path in a downloaded theme has no business being read, and a
        // theme that needs one is broken rather than unsupported.
        let inc_path = Path::new(&inc);
        if inc_path.is_absolute()
            || inc_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            // A theme extends a sibling base, never something outside its own
            // directory. Refusing traversal keeps a downloaded theme file
            // from naming a path it has no business reading.
            return Err(anyhow!(
                "include must be a relative path inside the theme's directory, got {inc}"
            ));
        }
        let base = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(inc_path);
        load_chain(&base, depth + 1, out)?;
    }
    out.push(theme);
    Ok(())
}

/// Parse one theme document, tolerating VS Code's JSONC extras.
fn parse_theme(raw: &str) -> Result<RawTheme> {
    // `crate::tasks::strip_jsonc` walks CHARS. The `workspace.rs` one walks
    // bytes and turns a non-ASCII theme name ("Caf\u{e9} Noir") into mojibake,
    // which is exactly the sort of theme most likely to carry one.
    let stripped = crate::tasks::strip_jsonc(raw);
    let value: serde_json::Value = serde_json::from_str(&stripped)?;
    // `tokenColors` is camelCase in the file; take it by hand rather than
    // renaming through serde so a theme using the TextMate `settings` array
    // spelling (older themes) still yields its colours.
    let mut theme: RawTheme = serde_json::from_value(value.clone())?;
    theme.semantic = value
        .get("semanticTokenColors")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<String, Value>>()
        })
        .unwrap_or_default();
    let tokens = value
        .get("tokenColors")
        .or_else(|| value.get("settings"))
        .cloned()
        .unwrap_or(serde_json::Value::Array(Vec::new()));
    // Per RULE, not all-or-nothing. Deserialising the whole array at once
    // means one rule croft cannot read (`{"scope": null}` appears in real
    // themes) drops EVERY token colour, and `unwrap_or_default` turns that
    // into an empty list with no diagnostic: the theme imports looking fine
    // and renders with croft's Base16 palette throughout.
    let rules: Vec<Value> = serde_json::from_value(tokens).unwrap_or_default();
    let total = rules.len();
    theme.token_colors = rules
        .into_iter()
        .filter_map(|r| serde_json::from_value::<TokenColor>(r).ok())
        .collect();
    theme.dropped_rules = total - theme.token_colors.len();
    Ok(theme)
}

/// Merge a base theme under an overriding one.
fn merge(base: RawTheme, over: RawTheme) -> RawTheme {
    let mut colors = base.colors;
    colors.extend(over.colors);
    let mut semantic = base.semantic;
    semantic.extend(over.semantic);
    let mut token_colors = base.token_colors;
    // Later rules win in TextMate, and the including file is "later".
    token_colors.extend(over.token_colors);
    RawTheme {
        name: over.name.or(base.name),
        kind: over.kind.or(base.kind),
        include: None,
        colors,
        token_colors,
        semantic,
        dropped_rules: base.dropped_rules + over.dropped_rules,
    }
}

/// A semantic token type's colour, if the theme names one.
///
/// VS Code writes either `"function": "#rrggbb"` or
/// `"function": { "foreground": "#rrggbb", "bold": true }`, and a type can
/// carry modifiers (`"variable.readonly"`), which are matched on the type
/// before the dot.
fn semantic_color(theme: &RawTheme, want: &str, over: Rgb) -> Option<Rgb> {
    theme.semantic.iter().find_map(|(key, value)| {
        let ty = key.split(['.', ':']).next().unwrap_or(key);
        if ty != want {
            return None;
        }
        let raw = match value {
            Value::String(s) => Some(s.as_str()),
            Value::Object(o) => o.get("foreground").and_then(Value::as_str),
            _ => None,
        }?;
        parse_color(raw, over)
    })
}

/// The croft syntax roles, each with the TextMate scopes that fill it, most
/// specific first.
///
/// croft colours code by tree-sitter capture and LSP semantic token; VS Code
/// colours it by TextMate scope. The roles do not correspond one-to-one, so
/// each croft role names the scopes a theme author would have coloured for
/// the same thing, and the first one the theme actually defines wins.
const SYNTAX_SCOPES: &[(&str, &[&str])] = &[
    (
        "syn_comment",
        &["comment", "punctuation.definition.comment"],
    ),
    (
        "syn_keyword",
        &["keyword.control", "keyword", "storage.type", "storage"],
    ),
    ("syn_string", &["string.quoted", "string"]),
    (
        "syn_constant",
        &[
            "constant.numeric",
            "constant.language",
            "constant",
            "variable.parameter",
        ],
    ),
    (
        "syn_function",
        &[
            "entity.name.function",
            "support.function",
            "meta.function-call",
        ],
    ),
    (
        "syn_type",
        &[
            "entity.name.type",
            "entity.name.class",
            "support.type",
            "support.class",
        ],
    ),
    (
        "syn_tag",
        &[
            "entity.name.tag",
            "entity.other.attribute-name",
            "support.type.property-name",
        ],
    ),
];

/// Semantic token types that fill each croft role when `tokenColors` did
/// not. Same order as [`SYNTAX_SCOPES`].
const SYNTAX_SEMANTIC: &[(&str, &[&str])] = &[
    ("syn_comment", &["comment"]),
    ("syn_keyword", &["keyword", "modifier"]),
    ("syn_string", &["string"]),
    ("syn_constant", &["number", "parameter", "enumMember"]),
    ("syn_function", &["function", "method"]),
    ("syn_type", &["type", "class", "struct", "interface"]),
    ("syn_tag", &["property", "decorator", "macro"]),
];

/// The colour a theme gives `scope`, by TextMate's specificity rule: the
/// longest matching scope prefix wins, and among equals the last rule does.
fn scope_color(theme: &RawTheme, scope: &str, over: Rgb) -> Option<Rgb> {
    let mut best: Option<(usize, Rgb)> = None;
    for rule in &theme.token_colors {
        let Some(fg) = rule.settings.foreground.as_deref() else {
            continue;
        };
        let Some(color) = parse_color(fg, over) else {
            continue;
        };
        for claimed in rule.scope.scopes() {
            // A descendant selector ("source.js entity.name.function") scopes
            // a rule to a context. croft has no context to match against, so
            // the rule is considered for its LAST element only, and ranked
            // below any rule that claims that scope outright. Keeping the
            // selector whole made such rules dead, and whole themes (Tokyo
            // Night writes most of its rules this way) lost roles to them.
            let (claimed, contextual) = match claimed.rsplit_once(' ') {
                Some((_, last)) => (last.trim().to_string(), true),
                None => (claimed, false),
            };
            if claimed.is_empty() {
                continue;
            }
            // TextMate matches on dot-separated segments, and only ever
            // DOWNWARD: a rule for `keyword` colours `keyword.control.if`,
            // while a rule for `entity.name.function.decorator` colours
            // decorators and says nothing about functions in general.
            //
            // Matching upward as well looks harmless and is not: asked for
            // `entity.name.function`, One Dark Pro answered with the colour of
            // its narrowest function-ish rule and rendered every call yellow
            // instead of blue. A role must be filled by a rule that actually
            // covers it, or fall through to the next scope in its list.
            let matches = scope == claimed || scope.starts_with(&format!("{claimed}."));
            if !matches {
                continue;
            }
            // A contextual rule ranks below every direct one, whatever its
            // dot depth, so a specific rule for another language cannot
            // outrank a plain rule for this scope.
            let specificity = if contextual {
                0
            } else {
                claimed.matches('.').count() + 1
            };
            if best.map(|(s, _)| specificity >= s).unwrap_or(true) {
                best = Some((specificity, color));
            }
        }
    }
    best.map(|(_, c)| c)
}

/// The theme's default foreground: its scope-less `tokenColors` rule, then
/// `editor.foreground`.
fn default_fg(theme: &RawTheme, over: Rgb) -> Option<Rgb> {
    // Later rules win in TextMate, so the LAST scope-less rule is the
    // theme's default foreground, not the first.
    let scopeless = theme
        .token_colors
        .iter()
        .rev()
        .find(|r| r.scope.scopes().is_empty())
        .and_then(|r| r.settings.foreground.as_deref())
        .and_then(|c| parse_color(c, over));
    scopeless.or_else(|| {
        theme
            .colors
            .get("editor.foreground")
            .and_then(|c| parse_color(c, over))
    })
}

/// The 16 ANSI slots, in croft's order, with the workbench key that fills each.
const ANSI_KEYS: [&str; 16] = [
    "terminal.ansiBlack",
    "terminal.ansiRed",
    "terminal.ansiGreen",
    "terminal.ansiYellow",
    "terminal.ansiBlue",
    "terminal.ansiMagenta",
    "terminal.ansiCyan",
    "terminal.ansiWhite",
    "terminal.ansiBrightBlack",
    "terminal.ansiBrightRed",
    "terminal.ansiBrightGreen",
    "terminal.ansiBrightYellow",
    "terminal.ansiBrightBlue",
    "terminal.ansiBrightMagenta",
    "terminal.ansiBrightCyan",
    "terminal.ansiBrightWhite",
];

/// Convert a VS Code theme file (following any `include` chain) into a croft
/// extension manifest.
pub fn convert_file(path: &Path, id_override: Option<&str>) -> Result<Converted> {
    let mut chain = Vec::new();
    load_chain(path, 0, &mut chain)?;
    let merged = chain
        .into_iter()
        .reduce(merge)
        .ok_or_else(|| anyhow!("empty theme"))?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("imported"));
    convert(merged, id_override, &stem)
}

/// A theme name reduced to an id: letters, digits and the marks that belong
/// to them are kept, everything else becomes a separator.
///
/// The property is Unicode categories L, N and **M**, not
/// `char::is_alphanumeric()`, which covers only L and N. Combining marks are
/// not decoration: Devanagari carries them in NFC, so "\u{939}\u{93f}\u{928}\u{94d}\u{926}\u{940}"
/// slugged to "\u{939}\u{93f}\u{928}-\u{926}\u{940}", a word broken in half by a separator, and no amount
/// of normalising first would have helped. Latin text in NFD hits the same
/// edge ("Caf\u{65}\u{301}" becoming "cafe-"), and the damage was
/// position-dependent, since a leading mark survived the `is_empty` guard
/// while an interior one did not.
fn slug(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;

    static SEPARATORS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let separators = SEPARATORS.get_or_init(|| {
        regex::Regex::new(r"[^\p{L}\p{N}\p{M}]+").expect("a literal class compiles")
    });
    // Two names that are canonically equivalent (`Café` precomposed, `Cafe`
    // + a combining acute) render identically and are the same theme to
    // anyone; only their bytes differ, by which editor last saved the file.
    // Without this they minted two ids and installed twice (#407). NFC is
    // applied on both sides of the lowercase: lowercasing can expose a
    // composition the uppercase spelling had no precomposed character for
    // (`H` + U+0331 has none, so the first pass leaves it; its lowercase
    // `h` + U+0331 composes to U+1E96 `ẖ`). A name already in NFC
    // normalises to itself, so no existing single-form id changes.
    let lowered: String = s.nfc().collect::<String>().to_lowercase().nfc().collect();
    let slug = separators
        .replace_all(&lowered, "-")
        .trim_matches('-')
        .to_string();
    // A name of nothing BUT combining marks has no letter or digit to stand
    // on: the id renders as nothing and would install a directory with an
    // invisible name. The old loop rejected such a name as a side effect of
    // its `!out.is_empty()` position guard, which the regex dropped; this
    // says it directly. `is_alphanumeric` is categories L and N, so a mark
    // alone does not satisfy it while any real script does.
    if !slug.chars().any(char::is_alphanumeric) {
        return String::new();
    }
    slug
}

/// The header every generated manifest carries, used to recognise our own
/// output when deciding whether an id is really taken.
const GENERATED_HEADER: &str = "# Imported from a VS Code colour theme by `croft theme-import`.";

/// Ids already in use, so an import cannot shadow a theme croft ships.
///
/// A theme THIS importer wrote is not "in use" for the purposes of a
/// collision: re-importing an updated upstream theme must overwrite its own
/// manifest, as the generated header promises. Counting it minted
/// `one-dark-pro-2`, then `-3`, and grew the picker by an entry per run.
fn existing_ids() -> Vec<String> {
    crate::theme::Theme::all()
        .iter()
        .map(|t| t.id().to_string())
        .filter(|id| !was_generated_by_import(id))
        .collect()
}

/// Whether `id`'s manifest is one this importer wrote.
///
/// COUPLING, stated because every other judgment call in this file is: this
/// reconstructs the directory name (`theme-{id}`) that [`install`] chose,
/// while the loader enumerates every subdirectory and takes each id from the
/// manifest BODY, never comparing directory to id. The two agree for
/// anything this importer wrote and nothing keeps them agreeing. A user who
/// renames the directory therefore gets a fresh suffix on the next import.
///
/// Left as-is deliberately: the trigger needs a hand-rename nothing invites,
/// the failure is a duplicate picker entry rather than lost data, and the
/// user is told through the collision note. Closing it properly means asking
/// the loader which file an id came from, rather than adding a second
/// convention beside it.
fn was_generated_by_import(id: &str) -> bool {
    let path = crate::lsp::manifest::user_extensions_dir()
        .join(format!("theme-{id}"))
        .join("extension.toml");
    std::fs::read_to_string(path)
        .map(|text| text.starts_with(GENERATED_HEADER))
        .unwrap_or(false)
}

/// `wanted`, or `wanted-2`, `wanted-3`, ... until it is free.
///
/// `None` when every candidate is taken. Returning `wanted` on exhaustion
/// would hand back the one value just proved to collide, resurrecting the
/// silent no-op this function exists to prevent: the import would look like
/// it succeeded and the picker would keep showing the other theme.
fn unique_id(wanted: String, taken: &[String]) -> Option<String> {
    if !taken.contains(&wanted) {
        return Some(wanted);
    }
    (2..1000)
        .map(|n| format!("{wanted}-{n}"))
        .find(|candidate| !taken.contains(candidate))
}

fn convert(theme: RawTheme, id_override: Option<&str>, stem: &str) -> Result<Converted> {
    let existing = existing_ids();
    convert_with_ids(theme, id_override, stem, &existing)
}

fn convert_with_ids(
    theme: RawTheme,
    id_override: Option<&str>,
    stem: &str,
    taken: &[String],
) -> Result<Converted> {
    let light = theme
        .kind
        .as_deref()
        .map(|k| k.eq_ignore_ascii_case("light"))
        .unwrap_or(false);
    let mut notes: Vec<String> = Vec::new();

    // The background comes first: every translucent colour below composites
    // over it, so resolving it out of order would blend against the wrong base.
    let default_bg: Rgb = if light {
        (0xff, 0xff, 0xff)
    } else {
        (0x1e, 0x1e, 0x1e)
    };
    let bg = theme
        .colors
        .get("editor.background")
        .and_then(|c| parse_color(c, default_bg))
        .unwrap_or_else(|| {
            notes.push(format!(
                "editor.background missing: used VS Code's default {} ground",
                if light { "light" } else { "dark" }
            ));
            default_bg
        });

    let pick = |keys: &[&str]| -> Option<Rgb> {
        keys.iter()
            .find_map(|k| theme.colors.get(*k).and_then(|c| parse_color(c, bg)))
    };

    // Order matters more than it looks. `focusBorder` is the obvious key and
    // the wrong first choice: most themes set it to a muted grey, so leading
    // with it gave One Dark Pro a grey accent while its actual signature
    // colour sat unused in `activityBarBadge.background`.
    let accent = pick(&[
        "activityBarBadge.background",
        "button.background",
        "textLink.foreground",
        "focusBorder",
    ])
    .unwrap_or_else(|| {
        notes.push(String::from(
            "no activityBarBadge / button / textLink / focusBorder colour: accent derived from the background",
        ));
        shade(bg, if light { -0.45 } else { 0.45 })
    });
    let selection = pick(&[
        "editor.selectionBackground",
        "list.activeSelectionBackground",
        "editor.inactiveSelectionBackground",
    ])
    .unwrap_or_else(|| {
        notes.push(String::from(
            "no editor.selectionBackground: selection derived from the accent",
        ));
        shade(accent, -0.35)
    });
    let search = pick(&["input.background", "editorWidget.background"]).unwrap_or_else(|| {
        notes.push(String::from(
            "no input.background: search field derived from the background",
        ));
        shade(bg, if light { -0.06 } else { 0.08 })
    });
    let button =
        pick(&["button.background", "statusBarItem.remoteBackground"]).unwrap_or_else(|| {
            notes.push(String::from(
                "no button.background: button takes the accent",
            ));
            accent
        });

    // Tab chrome is emitted ONLY where the theme named it. `Theme::from_decl`
    // derives every omitted tab colour from the palette above, and writing a
    // derived value into the manifest would freeze croft's guess in place
    // where croft's own derivation would have tracked the theme.
    let tab_strip = pick(&["editorGroupHeader.tabsBackground", "tab.border"]);
    let tab_inactive = pick(&["tab.inactiveBackground"]);
    let tab_active = pick(&["tab.activeBackground"]);
    let tab_hover = pick(&["tab.hoverBackground"]);
    let tab_close_pill = pick(&["tab.activeBorderTop", "tab.activeBorder"]);

    // The OSK caps are croft's own surface; no VS Code theme names them.
    let osk_key = pick(&["keybindingLabel.background", "editorWidget.background"])
        .unwrap_or_else(|| shade(bg, if light { -0.10 } else { 0.14 }));
    let osk_special = shade(osk_key, if light { 0.35 } else { -0.35 });
    let osk_armed = button;

    let mut ansi: Vec<String> = Vec::new();
    let mut missing_ansi = 0usize;
    for key in ANSI_KEYS {
        match theme.colors.get(key).and_then(|c| parse_color(c, bg)) {
            Some(c) => ansi.push(hex_of(c)),
            None => {
                missing_ansi += 1;
                ansi.clear();
                break;
            }
        }
    }
    if missing_ansi > 0 {
        notes.push(String::from(
            "the theme does not define a full terminal.ansi* palette: croft keeps VS Code's default 16 colours, so terminal output may not match the editor",
        ));
    }

    if theme.dropped_rules > 0 {
        notes.push(format!(
            "{} tokenColors rule(s) could not be read and were skipped: the code palette may be less complete than the theme intends",
            theme.dropped_rules
        ));
    }

    let fg = default_fg(&theme, bg);
    if fg.is_none() {
        notes.push(String::from(
            "no editor.foreground and no default tokenColors rule: plain text keeps croft's Base16 foreground",
        ));
    }

    let mut syntax: Vec<(String, String)> = Vec::new();
    for (role, scopes) in SYNTAX_SCOPES {
        let from_scopes = scopes.iter().find_map(|s| scope_color(&theme, s, bg));
        // `semanticTokenColors` is the fallback, not the first choice: most
        // themes still design against TextMate scopes, and a theme that sets
        // both means the scope colour for anything without semantic tokens.
        let from_semantic = || {
            SYNTAX_SEMANTIC
                .iter()
                .find(|(r, _)| r == role)
                .and_then(|(_, types)| types.iter().find_map(|t| semantic_color(&theme, t, bg)))
        };
        // Decide first, then record. Pushing the note from inside the
        // `or_else` closure worked only because it is evaluated at most
        // once, which is a property of the call rather than of the code.
        let from_semantic = if from_scopes.is_none() {
            from_semantic()
        } else {
            None
        };
        if from_semantic.is_some() {
            notes.push(format!(
                "{} came from semanticTokenColors: the theme sets no TextMate scope for it, so it may differ from VS Code where semantic highlighting is off",
                role.trim_start_matches("syn_")
            ));
        }
        match from_scopes.or(from_semantic) {
            Some(c) => syntax.push(((*role).to_string(), hex_of(c))),
            None => notes.push(format!(
                "no colour for {}: croft's Base16 default is kept",
                role.trim_start_matches("syn_")
            )),
        }
    }
    if let Some(fg) = fg {
        syntax.push((String::from("syn_fg"), hex_of(fg)));
    }

    let label = theme
        .name
        .clone()
        .unwrap_or_else(|| stem.replace(['-', '_'], " "));
    let wanted = match id_override {
        Some(id) => slug(id),
        None => slug(&label),
    };
    if wanted.is_empty() {
        return Err(anyhow!("could not derive a theme id from {label:?}"));
    }
    // An id that collides with a theme croft already ships is worse than an
    // error: the picker resolves the id to the BUILT-IN, so the import
    // appears to succeed and then does nothing. Importing upstream One Dark
    // Pro produced "one-dark-pro", which croft already has.
    let id = unique_id(wanted.clone(), taken).ok_or_else(|| {
        anyhow!("every id from {wanted:?} to {wanted}-999 is already taken; pass --id")
    })?;
    if id != slug(id_override.unwrap_or(&label)) {
        notes.push(format!(
            "a theme with id {:?} already exists, so this one was installed as {id:?}",
            slug(id_override.unwrap_or(&label))
        ));
    }

    let mut m = String::new();
    m.push_str(GENERATED_HEADER);
    m.push('\n');
    m.push_str(
        "# Regenerate rather than hand-editing: re-running the import overwrites this file.\n",
    );
    for note in &notes {
        m.push_str(&format!("# note: {note}\n"));
    }
    m.push_str(&format!("id = \"{id}\"\n"));
    m.push_str(&format!("name = \"{}\"\n", toml_escape(&label)));
    m.push_str(&format!(
        "description = \"{} imported from VS Code.\"\n",
        toml_escape(&label)
    ));
    m.push_str("api_version = 1\n\n");
    m.push_str("[[themes]]\n");
    m.push_str(&format!("id = \"{id}\"\n"));
    m.push_str(&format!("label = \"{}\"\n", toml_escape(&label)));
    m.push_str(&format!("background = \"{}\"\n", hex_of(bg)));
    m.push_str(&format!("accent = \"{}\"\n", hex_of(accent)));
    m.push_str(&format!("selection = \"{}\"\n", hex_of(selection)));
    m.push_str(&format!("search = \"{}\"\n", hex_of(search)));
    m.push_str(&format!("button = \"{}\"\n", hex_of(button)));
    m.push_str("gradient = false\n");
    m.push_str(&format!("osk_key = \"{}\"\n", hex_of(osk_key)));
    m.push_str(&format!("osk_special = \"{}\"\n", hex_of(osk_special)));
    m.push_str(&format!("osk_armed = \"{}\"\n", hex_of(osk_armed)));
    for (key, value) in [
        ("tab_strip", tab_strip),
        ("tab_inactive", tab_inactive),
        ("tab_active", tab_active),
        ("tab_hover", tab_hover),
        ("tab_close_pill", tab_close_pill),
    ] {
        if let Some(c) = value {
            m.push_str(&format!("{key} = \"{}\"\n", hex_of(c)));
        }
    }
    if !ansi.is_empty() {
        let quoted: Vec<String> = ansi.iter().map(|c| format!("\"{c}\"")).collect();
        m.push_str(&format!("ansi = [{}]\n", quoted.join(", ")));
    }
    for (key, value) in &syntax {
        m.push_str(&format!("{key} = \"{value}\"\n"));
    }

    Ok(Converted {
        id,
        label,
        manifest: m,
        notes,
    })
}

/// Escape a string for a double-quoted TOML value. Theme names carry
/// quotes and backslashes often enough to matter (`"Andromeda \"Bordered\""`).
fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Any other control character has no TOML escape worth guessing
            // at and no business in a theme name; \u form keeps the file
            // parseable rather than truncating it at a stray byte.
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Write a converted theme into the user extensions directory, returning the
/// manifest path.
pub fn install(converted: &Converted) -> Result<Installed> {
    install_into(&crate::lsp::manifest::user_extensions_dir(), converted)
}

/// [`install`] into an explicit extensions directory.
///
/// Split out so a test can write somewhere real without touching
/// `XDG_CONFIG_HOME`: the test binary runs its tests on threads of ONE
/// process, so an env var set by a test is set for every other test running
/// beside it, and the failure lands somewhere unrelated.
pub fn install_into(extensions_dir: &Path, converted: &Converted) -> Result<Installed> {
    let dir = extensions_dir.join(format!("theme-{}", converted.id));
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("extension.toml");
    // Re-importing is how an upstream change is picked up, so this
    // overwrites, but it must SAY so: a user who did not realise a manifest
    // was already there cannot otherwise tell an update from a clobbering.
    let replaced = path.is_file();
    std::fs::write(&path, &converted.manifest)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(Installed { path, replaced })
}

/// Where a converted theme landed, and whether it replaced one already there.
pub struct Installed {
    pub path: std::path::PathBuf,
    pub replaced: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A compact but realistic dark theme: workbench colours, a full ANSI
    /// palette, and token rules at several specificities.
    fn dark_fixture() -> &'static str {
        r##"{
            "name": "Fixture Dark",
            "type": "dark",
            "colors": {
                "editor.background": "#1e2030",
                "editor.foreground": "#cad3f5",
                "activityBarBadge.background": "#8aadf4",
                "editor.selectionBackground": "#3a3f58",
                "input.background": "#181926",
                "button.background": "#7dc4e4",
                "editorGroupHeader.tabsBackground": "#181926",
                "tab.inactiveBackground": "#1e2030",
                "tab.activeBackground": "#24273a",
                "terminal.ansiBlack": "#000100",
                "terminal.ansiRed": "#000101",
                "terminal.ansiGreen": "#000102",
                "terminal.ansiYellow": "#000103",
                "terminal.ansiBlue": "#000104",
                "terminal.ansiMagenta": "#000105",
                "terminal.ansiCyan": "#000106",
                "terminal.ansiWhite": "#000107",
                "terminal.ansiBrightBlack": "#000108",
                "terminal.ansiBrightRed": "#000109",
                "terminal.ansiBrightGreen": "#00010a",
                "terminal.ansiBrightYellow": "#00010b",
                "terminal.ansiBrightBlue": "#00010c",
                "terminal.ansiBrightMagenta": "#00010d",
                "terminal.ansiBrightCyan": "#00010e",
                "terminal.ansiBrightWhite": "#00010f"
            },
            "tokenColors": [
                { "settings": { "foreground": "#cad3f5" } },
                { "scope": "comment", "settings": { "foreground": "#6e738d" } },
                { "scope": ["keyword", "storage.type"], "settings": { "foreground": "#c6a0f6" } },
                { "scope": "string", "settings": { "foreground": "#a6da95" } },
                { "scope": "constant.numeric", "settings": { "foreground": "#f5a97f" } },
                { "scope": "entity.name.function", "settings": { "foreground": "#8aadf4" } },
                { "scope": "entity.name.type", "settings": { "foreground": "#eed49f" } },
                { "scope": "entity.name.tag", "settings": { "foreground": "#ed8796" } }
            ]
        }"##
    }

    fn convert_str(src: &str) -> Converted {
        let raw = parse_theme(src).expect("fixture parses");
        convert(raw, None, "fixture").expect("fixture converts")
    }

    /// The output must be a manifest croft can actually load. Testing the
    /// generated string against itself would prove only that the builder is
    /// self-consistent, so it goes through the real parser.
    #[test]
    fn the_generated_manifest_parses_as_a_croft_theme() {
        let converted = convert_str(dark_fixture());
        let manifest =
            crate::lsp::manifest::parse(&converted.manifest).expect("croft parses the manifest");
        assert_eq!(manifest.themes.len(), 1);
        let t = &manifest.themes[0];
        assert_eq!(t.id, "fixture-dark");
        assert_eq!(t.label, "Fixture Dark");
        assert_eq!(t.background, "#1e2030");
        assert_eq!(t.accent, "#8aadf4", "the badge colour, not focusBorder");
        assert_eq!(t.selection, "#3a3f58");
        assert_eq!(t.search, "#181926");
        assert_eq!(t.button, "#7dc4e4");
        // Every slot differs, so the ORDER is observable: with the palette's
        // real colours six of the eight bright slots duplicated their normal
        // counterpart, and a conversion that swapped the halves passed this
        // assertion unchanged.
        assert_eq!(t.ansi.len(), 16, "a full terminal palette carries over");
        let expected: Vec<String> = (0..16).map(|i| format!("#0001{i:02x}")).collect();
        assert_eq!(
            t.ansi, expected,
            "the sixteen slots must land in croft's order, black..bright white"
        );
        assert_eq!(t.syn_keyword, "#c6a0f6");
        assert_eq!(t.syn_string, "#a6da95");
        assert_eq!(t.syn_comment, "#6e738d");
        assert_eq!(t.syn_constant, "#f5a97f");
        assert_eq!(t.syn_function, "#8aadf4");
        assert_eq!(t.syn_type, "#eed49f");
        assert_eq!(t.syn_tag, "#ed8796");
        assert_eq!(t.syn_fg, "#cad3f5", "the scope-less rule is the default fg");
    }

    /// The regression that real themes caught: a rule for a NARROWER scope
    /// must not fill a general role. One Dark Pro rendered every call yellow
    /// because `entity.name.function.decorator`-style rules answered a query
    /// for `entity.name.function`.
    #[test]
    fn a_narrower_rule_never_fills_a_general_role() {
        let src = r##"{
            "type": "dark",
            "colors": { "editor.background": "#000000" },
            "tokenColors": [
                { "scope": "entity.name.function.decorator", "settings": { "foreground": "#ffff00" } },
                { "scope": "support.function", "settings": { "foreground": "#0000ff" } },
                { "scope": "keyword.operator.arithmetic", "settings": { "foreground": "#ff0000" } },
                { "scope": "keyword", "settings": { "foreground": "#00ff00" } }
            ]
        }"##;
        let converted = convert_str(src);
        let t = &crate::lsp::manifest::parse(&converted.manifest)
            .unwrap()
            .themes[0];
        assert_eq!(
            t.syn_function, "#0000ff",
            "the decorator rule must not colour functions in general"
        );
        assert_eq!(
            t.syn_keyword, "#00ff00",
            "the arithmetic-operator rule must not colour keywords in general"
        );
    }

    /// Among rules that DO cover the role, the most specific one wins, and
    /// the later of two equals does.
    #[test]
    fn the_most_specific_covering_rule_wins() {
        let src = r##"{
            "type": "dark",
            "colors": { "editor.background": "#000000" },
            "tokenColors": [
                { "scope": "entity", "settings": { "foreground": "#111111" } },
                { "scope": "entity.name", "settings": { "foreground": "#222222" } },
                { "scope": "string", "settings": { "foreground": "#333333" } },
                { "scope": "string", "settings": { "foreground": "#444444" } }
            ]
        }"##;
        let converted = convert_str(src);
        let t = &crate::lsp::manifest::parse(&converted.manifest)
            .unwrap()
            .themes[0];
        assert_eq!(t.syn_function, "#222222", "entity.name beats entity");
        assert_eq!(t.syn_string, "#444444", "the later of two equals wins");
    }

    /// VS Code colours are routinely translucent. Dropping the alpha would
    /// hand back a colour far more saturated than the author designed, so it
    /// composites over the surface it sits on.
    #[test]
    fn translucent_colours_composite_over_the_background() {
        let src = r##"{
            "type": "dark",
            "colors": {
                "editor.background": "#000000",
                "editor.selectionBackground": "#ffffff80"
            }
        }"##;
        let converted = convert_str(src);
        let t = &crate::lsp::manifest::parse(&converted.manifest)
            .unwrap()
            .themes[0];
        assert_eq!(
            t.selection, "#808080",
            "50% white over black is mid grey, not white"
        );
    }

    /// A light theme must not inherit dark fallbacks (the #217 class of bug:
    /// light chrome derived from a dark assumption is unreadable).
    #[test]
    fn a_light_theme_falls_back_to_light_ground() {
        let src = r##"{ "name": "Pale", "type": "light", "colors": {} }"##;
        let converted = convert_str(src);
        let t = &crate::lsp::manifest::parse(&converted.manifest)
            .unwrap()
            .themes[0];
        assert_eq!(t.background, "#ffffff");
        // Derived chrome must stay on the light side of the ground rather
        // than lightening further into invisibility.
        let bg = parse_color(&t.background, (0, 0, 0)).unwrap();
        let search = parse_color(&t.search, (0, 0, 0)).unwrap();
        assert!(
            search.0 < bg.0 && search.1 < bg.1 && search.2 < bg.2,
            "a light theme's search field must be DARKER than its ground, got {}",
            t.search
        );
        assert!(
            !converted.notes.is_empty(),
            "a theme this bare must report what was derived"
        );
    }

    /// Short hex and JSONC extras both appear in real theme files.
    #[test]
    fn short_hex_and_jsonc_extras_parse() {
        let src = r##"{
            // a comment VS Code tolerates
            "name": "Terse",
            "type": "dark",
            "colors": {
                "editor.background": "#123",
                "editor.foreground": "#fff",
            },
        }"##;
        let converted = convert_str(src);
        let t = &crate::lsp::manifest::parse(&converted.manifest)
            .unwrap()
            .themes[0];
        assert_eq!(t.background, "#112233", "#123 expands to #112233");
        assert_eq!(t.syn_fg, "#ffffff");
    }

    /// A partial terminal palette is dropped whole rather than half-filled:
    /// eight themed colours beside eight defaults looks like a rendering bug.
    #[test]
    fn a_partial_ansi_palette_is_dropped_and_reported() {
        let src = r##"{
            "type": "dark",
            "colors": {
                "editor.background": "#000000",
                "terminal.ansiRed": "#ff0000",
                "terminal.ansiGreen": "#00ff00"
            }
        }"##;
        let converted = convert_str(src);
        let t = &crate::lsp::manifest::parse(&converted.manifest)
            .unwrap()
            .themes[0];
        assert!(t.ansi.is_empty(), "a half palette must not ship");
        assert!(
            converted.notes.iter().any(|n| n.contains("terminal.ansi")),
            "and the user must be told, got {:?}",
            converted.notes
        );
    }

    /// An `include` chain is how VS Code's own defaults are written
    /// (`dark_plus` includes `dark_vs`), and the including file wins.
    #[test]
    fn an_include_chain_merges_base_first() {
        let dir = tempfile::TempDir::new().unwrap();
        let base = dir.path().join("base.json");
        std::fs::write(
            &base,
            r##"{
                "type": "dark",
                "colors": { "editor.background": "#101010", "editor.foreground": "#aaaaaa" },
                "tokenColors": [{ "scope": "comment", "settings": { "foreground": "#555555" } }]
            }"##,
        )
        .unwrap();
        let top = dir.path().join("top.json");
        std::fs::write(
            &top,
            r##"{
                "name": "Layered",
                "include": "./base.json",
                "type": "dark",
                "colors": { "editor.foreground": "#ffffff" }
            }"##,
        )
        .unwrap();

        let converted = convert_file(&top, None).expect("the chain converts");
        let t = &crate::lsp::manifest::parse(&converted.manifest)
            .unwrap()
            .themes[0];
        assert_eq!(t.background, "#101010", "the base fills what the top omits");
        assert_eq!(t.syn_fg, "#ffffff", "the including file wins");
        assert_eq!(t.syn_comment, "#555555", "base token rules carry over");
    }

    /// An absolute include is refused: a theme file naming a path outside its
    /// own directory has no business being followed.
    #[test]
    fn an_absolute_include_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let top = dir.path().join("top.json");
        std::fs::write(
            &top,
            r##"{ "include": "/etc/passwd", "type": "dark", "colors": {} }"##,
        )
        .unwrap();
        let err = convert_file(&top, None).expect_err("an absolute include must be refused");
        assert!(
            format!("{err:#}").contains("relative"),
            "the error must say why, got {err:#}"
        );
    }

    /// Review finding: the byte-walking `strip_jsonc` in `workspace.rs`
    /// turned every non-ASCII character into mojibake, so a theme called
    /// "Caf\u{e9} Noir" imported under a mangled name. The chars-based
    /// stripper in `tasks.rs` is the right one.
    #[test]
    fn a_non_ascii_theme_name_survives_the_jsonc_strip() {
        let src = r##"{
            // a comment, so the stripper definitely runs
            "name": "Caf\u00e9 Noir \u2014 \u3086\u3081",
            "type": "dark",
            "colors": { "editor.background": "#101010" }
        }"##;
        let converted = convert_str(src);
        assert_eq!(
            converted.label, "Caf\u{e9} Noir \u{2014} \u{3086}\u{3081}",
            "the name must come through intact, not as mojibake"
        );
        let manifest = crate::lsp::manifest::parse(&converted.manifest).unwrap();
        assert_eq!(manifest.themes[0].label, converted.label);
    }

    /// Review finding: a descendant selector was kept whole and matched
    /// nothing, so themes that write most of their rules that way (Tokyo
    /// Night) lost roles. It now matches on the last element, and ranks
    /// below any rule claiming that scope outright.
    #[test]
    fn a_descendant_selector_fills_a_role_but_yields_to_a_direct_rule() {
        let contextual_only = r##"{
            "type": "dark",
            "colors": { "editor.background": "#000000" },
            "tokenColors": [
                { "scope": "source.js entity.name.function", "settings": { "foreground": "#112233" } }
            ]
        }"##;
        let t = &crate::lsp::manifest::parse(&convert_str(contextual_only).manifest)
            .unwrap()
            .themes[0];
        assert_eq!(
            t.syn_function, "#112233",
            "a contextual rule is better than leaving the role unset"
        );

        let both = r##"{
            "type": "dark",
            "colors": { "editor.background": "#000000" },
            "tokenColors": [
                { "scope": "source.js entity.name.function.member", "settings": { "foreground": "#112233" } },
                { "scope": "entity.name.function", "settings": { "foreground": "#445566" } }
            ]
        }"##;
        let t = &crate::lsp::manifest::parse(&convert_str(both).manifest)
            .unwrap()
            .themes[0];
        assert_eq!(
            t.syn_function, "#445566",
            "a direct rule outranks a contextual one however deep"
        );
    }

    /// `semanticTokenColors` fills a role the TextMate scopes left empty,
    /// and only that: a theme setting both means the scope colour for
    /// everything without semantic tokens.
    #[test]
    fn semantic_token_colours_are_a_fallback_not_an_override() {
        let src = r##"{
            "type": "dark",
            "colors": { "editor.background": "#000000" },
            "semanticTokenColors": {
                "function": "#aabbcc",
                "keyword": { "foreground": "#ddeeff", "bold": true },
                "type.defaultLibrary": "#010203"
            },
            "tokenColors": [
                { "scope": "keyword", "settings": { "foreground": "#999999" } }
            ]
        }"##;
        let t = &crate::lsp::manifest::parse(&convert_str(src).manifest)
            .unwrap()
            .themes[0];
        assert_eq!(
            t.syn_function, "#aabbcc",
            "no scope rule, so semantic fills it"
        );
        assert_eq!(
            t.syn_keyword, "#999999",
            "the scope rule wins where the theme set both"
        );
        assert_eq!(
            t.syn_type, "#010203",
            "a modifier suffix still matches its token type"
        );
    }

    /// Review finding: an id colliding with a bundled theme made the import
    /// UNREACHABLE, since the picker resolves the id to the built-in. The
    /// import appeared to succeed and changed nothing.
    #[test]
    fn an_id_that_collides_with_an_existing_theme_is_suffixed_and_reported() {
        let src = r##"{ "name": "Dracula", "type": "dark", "colors": { "editor.background": "#282a36" } }"##;
        let raw = parse_theme(src).unwrap();
        let taken = vec![String::from("dracula"), String::from("dracula-2")];
        let converted = convert_with_ids(raw, None, "dracula", &taken).unwrap();
        assert_eq!(converted.id, "dracula-3");
        assert!(
            converted.notes.iter().any(|n| n.contains("already exists")),
            "the user must be told the id changed: {:?}",
            converted.notes
        );

        // Every bundled theme id is taken, so a real import of one of them
        // cannot shadow it.
        let ids = existing_ids();
        assert!(
            ids.iter().any(|i| i == "black"),
            "the bundled ids must be what collision is checked against"
        );
    }

    /// TextMate takes the LAST scope-less rule as the default foreground.
    #[test]
    fn the_last_scopeless_rule_is_the_default_foreground() {
        let src = r##"{
            "type": "dark",
            "colors": { "editor.background": "#000000" },
            "tokenColors": [
                { "settings": { "foreground": "#111111" } },
                { "settings": { "foreground": "#222222" } }
            ]
        }"##;
        let t = &crate::lsp::manifest::parse(&convert_str(src).manifest)
            .unwrap()
            .themes[0];
        assert_eq!(t.syn_fg, "#222222");
    }

    /// An include may name a sibling, never a path outside the theme's own
    /// directory.
    #[test]
    fn an_include_that_traverses_upwards_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let nested = dir.path().join("themes");
        std::fs::create_dir(&nested).unwrap();
        let top = nested.join("top.json");
        std::fs::write(
            &top,
            r##"{ "include": "../secrets.json", "type": "dark", "colors": {} }"##,
        )
        .unwrap();
        let err = convert_file(&top, None).expect_err("traversal must be refused");
        assert!(format!("{err:#}").contains("inside the theme"), "{err:#}");
    }

    /// Tab chrome is emitted only where the theme named it, so croft's own
    /// derivation still applies to the rest.
    #[test]
    fn tab_colours_are_left_out_when_the_theme_does_not_name_them() {
        let bare = r##"{ "type": "dark", "colors": { "editor.background": "#101010" } }"##;
        let converted = convert_str(bare);
        assert!(
            !converted.manifest.contains("tab_strip"),
            "a derived tab colour must not be frozen into the manifest:\n{}",
            converted.manifest
        );
        let named = r##"{
            "type": "dark",
            "colors": {
                "editor.background": "#101010",
                "tab.activeBackground": "#202020"
            }
        }"##;
        let converted = convert_str(named);
        assert!(converted.manifest.contains("tab_active = \"#202020\""));
        assert!(!converted.manifest.contains("tab_hover"));
    }

    /// A control character in a theme name must not break the manifest.
    #[test]
    fn control_characters_in_a_name_stay_parseable() {
        let src = "{ \"name\": \"Odd\\u0007Name\\tTabbed\", \"type\": \"dark\", \"colors\": {} }";
        let converted = convert_str(src);
        crate::lsp::manifest::parse(&converted.manifest)
            .expect("a control character must not produce an unparseable manifest");
    }

    /// Re-importing an updated upstream theme must OVERWRITE its own
    /// manifest, which is what the generated header promises. Counting a
    /// previously imported theme as a collision minted `-2`, then `-3`, and
    /// grew the picker by an entry per run.
    #[test]
    fn a_reimport_reuses_its_own_id_rather_than_minting_a_new_one() {
        let manifest = convert_str(dark_fixture()).manifest;
        assert!(
            manifest.starts_with(GENERATED_HEADER),
            "the header is what marks a manifest as ours: {manifest:.80}"
        );

        // A bundled id IS a collision; one of our own manifests is not.
        let raw = parse_theme(dark_fixture()).unwrap();
        let taken = vec![String::from("fixture-dark")];
        let collided = convert_with_ids(raw, None, "fixture", &taken).unwrap();
        assert_eq!(collided.id, "fixture-dark-2");

        let raw = parse_theme(dark_fixture()).unwrap();
        let fresh = convert_with_ids(raw, None, "fixture", &[]).unwrap();
        assert_eq!(
            fresh.id, "fixture-dark",
            "with the id free, the import keeps its natural name"
        );
    }

    /// `install` writes where the id says and reports whether it replaced a
    /// manifest, so a re-import is distinguishable from a first import.
    ///
    /// Goes through `install_into` rather than setting `XDG_CONFIG_HOME`:
    /// tests share one process, so an env var set here would apply to every
    /// test running alongside it. An earlier version of this test did that
    /// and took an unrelated navigator test down with it.
    #[test]
    fn install_writes_the_manifest_and_reports_a_replacement() {
        let dir = tempfile::TempDir::new().unwrap();
        let converted = convert_str(dark_fixture());

        let first = install_into(dir.path(), &converted).expect("first install");
        assert!(first.path.is_file());
        assert!(!first.replaced, "nothing was there the first time");
        assert!(
            std::fs::read_to_string(&first.path)
                .unwrap()
                .starts_with(GENERATED_HEADER)
        );

        let second = install_into(dir.path(), &converted).expect("re-install");
        assert_eq!(
            second.path, first.path,
            "the same id lands in the same file"
        );
        assert!(second.replaced, "the second run must say it overwrote");
    }

    /// A role filled from `semanticTokenColors` is REPORTED, because such a
    /// theme renders differently in an editor with semantic highlighting off,
    /// and the user should know which colours came from that path.
    #[test]
    fn a_role_filled_from_semantic_tokens_is_reported() {
        let src = r##"{
            "type": "dark",
            "colors": { "editor.background": "#000000" },
            "semanticTokenColors": { "function": "#aabbcc" }
        }"##;
        let converted = convert_str(src);
        assert!(
            converted
                .notes
                .iter()
                .any(|n| n.contains("semanticTokenColors") && n.contains("function")),
            "the note must name the role: {:?}",
            converted.notes
        );
    }

    /// A const inserted between another const and its doc comment silently
    /// takes that documentation, and the CI gate only inspects functions, so
    /// nothing else would catch it. This pins the two tables apart.
    #[test]
    fn the_scope_and_semantic_tables_cover_the_same_roles() {
        let scope_roles: Vec<&str> = SYNTAX_SCOPES.iter().map(|(r, _)| *r).collect();
        let semantic_roles: Vec<&str> = SYNTAX_SEMANTIC.iter().map(|(r, _)| *r).collect();
        assert_eq!(
            scope_roles, semantic_roles,
            "the two tables are read together by role and must stay aligned"
        );
    }

    /// Round-3 review finding: parsing `tokenColors` as one array meant a
    /// single rule croft could not read dropped EVERY token colour, and
    /// `unwrap_or_default` turned that into an empty list with no
    /// diagnostic. `{"scope": null}` appears in real themes, so the theme
    /// imported looking fine and rendered with croft's own palette.
    #[test]
    fn one_unreadable_rule_does_not_discard_the_whole_palette() {
        let src = r##"{
            "type": "dark",
            "colors": { "editor.background": "#000000" },
            "tokenColors": [
                { "scope": "comment", "settings": { "foreground": "#111111" } },
                { "scope": null, "settings": { "foreground": "#222222" } },
                { "scope": "keyword", "settings": { "foreground": "#333333" } }
            ]
        }"##;
        let converted = convert_str(src);
        let t = &crate::lsp::manifest::parse(&converted.manifest)
            .unwrap()
            .themes[0];
        assert_eq!(t.syn_comment, "#111111", "rules before the bad one survive");
        assert_eq!(t.syn_keyword, "#333333", "and rules after it");
        assert!(
            converted
                .notes
                .iter()
                .any(|n| n.contains("could not be read")),
            "and the skipped rule is reported: {:?}",
            converted.notes
        );
    }

    /// Review finding: `slug` kept only ASCII alphanumerics, so a theme
    /// named in any non-Latin script slugged to nothing and could not be
    /// imported at all, and a MIXED name silently lost its distinguishing
    /// half.
    ///
    /// The round-1 test asserting the non-ASCII LABEL survives passed
    /// throughout: it named a property next to the one that mattered.
    ///
    /// And the first version of THIS test used four mark-free scripts, so a
    /// change to `slug` altering Hindi, German and French output left the
    /// suite green with nothing edited. A fixture that cannot reach the
    /// dimension under test is the same failure one level down.
    #[test]
    fn a_theme_named_in_a_non_latin_script_can_be_imported() {
        for (name, want) in [
            (
                "\u{4e2d}\u{6587}\u{4e3b}\u{9898}",
                "\u{4e2d}\u{6587}\u{4e3b}\u{9898}",
            ),
            ("\u{30c6}\u{30fc}\u{30de}", "\u{30c6}\u{30fc}\u{30de}"),
            (
                "\u{422}\u{435}\u{43c}\u{430}",
                "\u{442}\u{435}\u{43c}\u{430}",
            ),
            // A mixed name keeps BOTH halves: dropping the script half made
            // every "... Dark" theme collide with every other.
            ("\u{4e2d}\u{6587} Dark", "\u{4e2d}\u{6587}-dark"),
            // COMBINING MARKS. The four scripts above are all mark-free, so
            // they could not reach the dimension that actually broke: a mark
            // is category M, not L or N, so `is_alphanumeric` treated it as a
            // separator and cut a word in half. Devanagari carries marks in
            // NFC, so normalising first would not have saved it.
            (
                "\u{939}\u{93f}\u{928}\u{94d}\u{926}\u{940}",
                "\u{939}\u{93f}\u{928}\u{94d}\u{926}\u{940}",
            ),
            // Latin in NFD hits the same edge: "Cafe" + combining acute.
            // Since #407 the id is the NFC form, the same one the precomposed
            // spelling mints, so the mark is folded rather than kept.
            ("Cafe\u{301}", "caf\u{e9}"),
            // And the damage was POSITION-dependent: a leading mark survived
            // the empty-output guard while an interior one did not.
            ("U\u{308}ber", "\u{fc}ber"),
        ] {
            let src = format!(
                r##"{{ "name": "{name}", "type": "dark", "colors": {{ "editor.background": "#101010" }} }}"##
            );
            let raw = parse_theme(&src).expect("parses");
            let converted =
                convert_with_ids(raw, None, "fixture", &[]).expect("a non-Latin name must import");
            assert_eq!(converted.id, want, "id derived from {name:?}");
            let manifest = crate::lsp::manifest::parse(&converted.manifest)
                .expect("and the manifest must parse");
            assert_eq!(manifest.themes[0].label, name);
        }
    }

    /// A colour value that is not ASCII must be refused, not sliced.
    ///
    /// `parse_color` indexes by BYTE range, so a multi-byte character makes
    /// the length arms lie: "#\u{20ac}" is one character and three bytes, hits
    /// the three-digit arm, and slices mid-character. The bug was a panic, so
    /// reaching the assertions is the test.
    #[test]
    fn a_non_ascii_colour_value_is_refused_rather_than_sliced() {
        for value in [
            "#\u{20ac}",           // three bytes, three-digit arm
            "#\u{e9}\u{e9}\u{e9}", // six bytes, six-digit arm
            "#\u{4e2d}\u{6587}",   // six bytes, two characters
            "#\u{1f600}",          // four bytes, four-digit arm
        ] {
            assert_eq!(
                parse_color(value, (0, 0, 0)),
                None,
                "{value:?} is not a colour and must not be indexed as one"
            );
        }

        // The ASCII forms still parse, so the guard is narrow.
        assert_eq!(parse_color("#fff", (0, 0, 0)), Some((255, 255, 255)));
        assert_eq!(parse_color("#102030", (0, 0, 0)), Some((16, 32, 48)));
    }

    /// A name with no letter or digit cannot become an id.
    ///
    /// The regex rewrite dropped the old loop's `!out.is_empty()` position
    /// guard, which had rejected a mark-only name as a side effect. Without
    /// it, `"\u{301}\u{308}"` slugged to a string of combining marks and
    /// installed a directory whose name renders as nothing.
    #[test]
    fn a_name_with_no_letter_or_digit_is_refused() {
        for name in [
            "\u{301}\u{308}",     // combining marks alone
            "\u{1f600}\u{1f680}", // emoji
            "!!!",
            "..",
            "   ",
        ] {
            assert_eq!(slug(name), "", "{name:?} has nothing to name a theme with");
            let src = format!(r##"{{ "name": "{name}", "type": "dark", "colors": {{}} }}"##);
            let raw = parse_theme(&src).expect("parses");
            assert!(
                convert_with_ids(raw, None, "", &[]).is_err(),
                "{name:?} must fail the import rather than install an unnameable theme"
            );
        }

        // And a real script still passes: the guard is "no letter or digit",
        // not "no marks".
        assert_eq!(
            slug("\u{939}\u{93f}\u{928}\u{94d}\u{926}\u{940}"),
            "\u{939}\u{93f}\u{928}\u{94d}\u{926}\u{940}"
        );
        assert_eq!(slug("Cafe\u{301}"), "café");
    }

    /// Two names that are canonically equivalent (#407) are the same theme
    /// to a user: they render identically and compare equal under NFC. The
    /// id must not depend on which encoding the theme file happened to use,
    /// or importing the same theme from two sources installs it twice.
    #[test]
    fn canonically_equivalent_names_share_an_id() {
        // Precomposed é (NFC) and e + combining acute (NFD).
        assert_eq!(slug("Café Dark"), "café-dark");
        assert_eq!(slug("Cafe\u{301} Dark"), "café-dark");
        // Lowercasing İ (U+0130) yields i + U+0307; a name spelled that way
        // from the start must land on the same id.
        assert_eq!(slug("İstanbul"), slug("i\u{307}stanbul"));
        // The trailing NFC is load-bearing: "H" + U+0331 has no precomposed
        // uppercase, so the first pass leaves it, and its lowercase
        // "h" + U+0331 composes to U+1E96. Without the second pass the two
        // spellings mint two ids, the #407 bug surviving its own fix.
        assert_eq!(slug("H\u{331}ana"), slug("\u{1e96}ana"));
        assert_eq!(slug("H\u{331}ana"), "\u{1e96}ana");
        // A name already in NFC is unchanged by the normalisation, so every
        // existing single-form id survives.
        assert_eq!(slug("One Dark Pro"), "one-dark-pro");
        assert_eq!(slug("Ñandú"), "ñandú");

        // And the two forms resolve to ONE installed theme, not a suffixed pair.
        let nfc =
            parse_theme(r##"{ "name": "Café Dark", "type": "dark", "colors": {} }"##).unwrap();
        let nfd =
            parse_theme(r##"{ "name": "Café Dark", "type": "dark", "colors": {} }"##).unwrap();
        let a = convert_with_ids(nfc, None, "", &[]).expect("converts");
        let b = convert_with_ids(nfd, None, "", &[]).expect("converts");
        assert_eq!(a.id, b.id);
        assert_eq!(a.id, "café-dark");
    }

    /// `unique_id` must never hand back the value it just proved is taken:
    /// that resurrects the silent no-op the suffixing exists to prevent.
    #[test]
    fn an_exhausted_id_space_is_an_error_not_a_collision() {
        let mut taken = vec![String::from("busy")];
        taken.extend((2..1000).map(|n| format!("busy-{n}")));
        assert_eq!(unique_id(String::from("busy"), &taken), None);
        assert_eq!(
            unique_id(String::from("free"), &taken),
            Some(String::from("free"))
        );

        let raw = parse_theme(r##"{ "name": "Busy", "type": "dark", "colors": {} }"##).unwrap();
        let err = convert_with_ids(raw, None, "busy", &taken)
            .expect_err("an exhausted space must fail loudly");
        assert!(format!("{err:#}").contains("--id"), "{err:#}");
    }

    /// Theme names carry characters TOML would otherwise take as syntax.
    #[test]
    fn a_quoted_theme_name_stays_valid_toml() {
        let src = r##"{
            "name": "Andromeda \"Bordered\"",
            "type": "dark",
            "colors": { "editor.background": "#000000" }
        }"##;
        let converted = convert_str(src);
        let manifest = crate::lsp::manifest::parse(&converted.manifest)
            .expect("a quoted name must not break the manifest");
        assert_eq!(manifest.themes[0].label, "Andromeda \"Bordered\"");
        assert_eq!(manifest.themes[0].id, "andromeda-bordered");
    }
}
