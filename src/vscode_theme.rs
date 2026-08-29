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
    /// Colours croft needed that the source theme did not name, and where
    /// each came from instead. Shown to the user by the CLI: a derived
    /// colour is a choice croft made, and silently making it is how an
    /// import "works" while looking wrong.
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
        if inc_path.is_absolute() {
            return Err(anyhow!("include must be a relative path, got {inc}"));
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
    let stripped = crate::workspace::strip_jsonc(raw);
    let value: serde_json::Value = serde_json::from_str(&stripped)?;
    // `tokenColors` is camelCase in the file; take it by hand rather than
    // renaming through serde so a theme using the TextMate `settings` array
    // spelling (older themes) still yields its colours.
    let mut theme: RawTheme = serde_json::from_value(value.clone())?;
    let tokens = value
        .get("tokenColors")
        .or_else(|| value.get("settings"))
        .cloned()
        .unwrap_or(serde_json::Value::Array(Vec::new()));
    theme.token_colors = serde_json::from_value(tokens).unwrap_or_default();
    Ok(theme)
}

/// Merge a base theme under an overriding one.
fn merge(base: RawTheme, over: RawTheme) -> RawTheme {
    let mut colors = base.colors;
    colors.extend(over.colors);
    let mut token_colors = base.token_colors;
    // Later rules win in TextMate, and the including file is "later".
    token_colors.extend(over.token_colors);
    RawTheme {
        name: over.name.or(base.name),
        kind: over.kind.or(base.kind),
        include: None,
        colors,
        token_colors,
    }
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
            let specificity = claimed.matches('.').count() + 1;
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
    let scopeless = theme
        .token_colors
        .iter()
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

fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn convert(theme: RawTheme, id_override: Option<&str>, stem: &str) -> Result<Converted> {
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

    let tab_strip = pick(&["editorGroupHeader.tabsBackground", "tab.border"])
        .unwrap_or_else(|| shade(bg, if light { -0.04 } else { 0.06 }));
    let tab_inactive = pick(&["tab.inactiveBackground"])
        .unwrap_or_else(|| shade(tab_strip, if light { -0.03 } else { 0.05 }));
    let tab_active = pick(&["tab.activeBackground"]).unwrap_or(bg);
    let tab_hover = pick(&["tab.hoverBackground"])
        .unwrap_or_else(|| shade(tab_inactive, if light { -0.08 } else { 0.12 }));
    let tab_close_pill = pick(&["tab.activeBorderTop", "tab.activeBorder"]).unwrap_or(accent);

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

    let fg = default_fg(&theme, bg);
    if fg.is_none() {
        notes.push(String::from(
            "no editor.foreground and no default tokenColors rule: plain text keeps croft's Base16 foreground",
        ));
    }

    let mut syntax: Vec<(String, String)> = Vec::new();
    for (role, scopes) in SYNTAX_SCOPES {
        match scopes.iter().find_map(|s| scope_color(&theme, s, bg)) {
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
    let id = match id_override {
        Some(id) => slug(id),
        None => slug(&label),
    };
    if id.is_empty() {
        return Err(anyhow!("could not derive a theme id from {label:?}"));
    }

    let mut m = String::new();
    m.push_str("# Imported from a VS Code colour theme by `croft theme-import`.\n");
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
    m.push_str(&format!("tab_strip = \"{}\"\n", hex_of(tab_strip)));
    m.push_str(&format!("tab_inactive = \"{}\"\n", hex_of(tab_inactive)));
    m.push_str(&format!("tab_active = \"{}\"\n", hex_of(tab_active)));
    m.push_str(&format!("tab_hover = \"{}\"\n", hex_of(tab_hover)));
    m.push_str(&format!(
        "tab_close_pill = \"{}\"\n",
        hex_of(tab_close_pill)
    ));
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
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Write a converted theme into the user extensions directory, returning the
/// manifest path.
pub fn install(converted: &Converted) -> Result<std::path::PathBuf> {
    let dir = crate::lsp::manifest::user_extensions_dir().join(format!("theme-{}", converted.id));
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("extension.toml");
    std::fs::write(&path, &converted.manifest)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
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
                "terminal.ansiBlack": "#494d64",
                "terminal.ansiRed": "#ed8796",
                "terminal.ansiGreen": "#a6da95",
                "terminal.ansiYellow": "#eed49f",
                "terminal.ansiBlue": "#8aadf4",
                "terminal.ansiMagenta": "#f5bde6",
                "terminal.ansiCyan": "#8bd5ca",
                "terminal.ansiWhite": "#b8c0e0",
                "terminal.ansiBrightBlack": "#5b6078",
                "terminal.ansiBrightRed": "#ed8796",
                "terminal.ansiBrightGreen": "#a6da95",
                "terminal.ansiBrightYellow": "#eed49f",
                "terminal.ansiBrightBlue": "#8aadf4",
                "terminal.ansiBrightMagenta": "#f5bde6",
                "terminal.ansiBrightCyan": "#8bd5ca",
                "terminal.ansiBrightWhite": "#a5adcb"
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
        assert_eq!(t.ansi.len(), 16, "a full terminal palette carries over");
        assert_eq!(t.ansi[1], "#ed8796");
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
