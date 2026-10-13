//! Emmet abbreviation expansion (#352's `vscode.emmet` gap).
//!
//! VS Code ships Emmet in the box, and it is the single biggest reason
//! writing HTML there feels different from writing it in a plain editor:
//! `ul>li.item*3` becomes a three-item list in one keystroke. nvim and Zed
//! both reach for it through plugins for the same reason.
//!
//! This is the abbreviation subset that covers ordinary markup authoring —
//! nesting, siblings, climbing up (`^`, #1640), grouping, repetition,
//! numbering, ids, classes, attributes and text. Deliberately absent:
//!
//! * CSS abbreviations (`m10-20`) and the document snippet `!`. They are
//!   separate features that happen to share a keystroke.
//!
//! Emmet's HTML aliases (`input:email`, `btn:s`, `link:css`, ...) and lorem
//! ipsum (`lorem`, `lorem5`) are in (#1230): a form is mostly typed inputs,
//! and a colon name that is no alias would otherwise become a made-up tag.
//!
//! An abbreviation that does not parse expands to nothing, so the chord is
//! inert on prose rather than mangling it.

/// HTML elements that never take a closing tag. Emmet's `html` profile
/// writes them bare (`<br>`), not self-closed, and so does this.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// A repetition cap. `div*10000` is a typo far more often than it is a
/// request, and expanding it would hang the editor building a string no one
/// asked for.
const MAX_REPEAT: usize = 1000;

/// A cap on the elements one expansion renders. `MAX_REPEAT` bounds each
/// node, not their product: nested, `div*1000>span*1000` would still render
/// a million elements.
const MAX_ELEMENTS: usize = 100_000;

/// The markup dialect an expansion is written in. The three differ only in
/// how a void element closes and in JSX's attribute spellings; everything
/// else is shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// `<img>` — HTML5 void elements take no closing slash.
    Html,
    /// `<img/>` — XML has no void elements, so an unclosed one is malformed.
    Xml,
    /// `<img />` with `className` — JSX rejects an unclosed tag outright.
    Jsx,
}

/// One element in a parsed abbreviation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Node {
    /// Empty when the abbreviation left it implicit (`.foo`), which the
    /// renderer resolves from the parent.
    name: String,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<(String, String)>,
    text: Option<String>,
    repeat: usize,
    children: Vec<Node>,
    /// A `(...)` group: contributes its children and no tag of its own.
    group: bool,
    /// Lorem ipsum (#1230): `text` on a line of its own, with no tag.
    bare_text: bool,
}

/// One alias: the colon name, the tag it stands for, and that tag's attributes.
type Alias = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

/// Emmet's HTML aliases (#1230): a colon name, the tag it stands for, and
/// that tag's attributes. An empty value is written as `name=""`, as Emmet
/// writes it, for the author to fill in.
const ALIASES: &[Alias] = &[
    ("input:hidden", "input", &[("type", "hidden"), ("name", "")]),
    ("input:h", "input", &[("type", "hidden"), ("name", "")]),
    (
        "input:text",
        "input",
        &[("type", "text"), ("name", ""), ("id", "")],
    ),
    (
        "input:t",
        "input",
        &[("type", "text"), ("name", ""), ("id", "")],
    ),
    (
        "input:search",
        "input",
        &[("type", "search"), ("name", ""), ("id", "")],
    ),
    (
        "input:email",
        "input",
        &[("type", "email"), ("name", ""), ("id", "")],
    ),
    (
        "input:url",
        "input",
        &[("type", "url"), ("name", ""), ("id", "")],
    ),
    (
        "input:password",
        "input",
        &[("type", "password"), ("name", ""), ("id", "")],
    ),
    (
        "input:p",
        "input",
        &[("type", "password"), ("name", ""), ("id", "")],
    ),
    (
        "input:datetime",
        "input",
        &[("type", "datetime"), ("name", ""), ("id", "")],
    ),
    (
        "input:date",
        "input",
        &[("type", "date"), ("name", ""), ("id", "")],
    ),
    (
        "input:datetime-local",
        "input",
        &[("type", "datetime-local"), ("name", ""), ("id", "")],
    ),
    (
        "input:month",
        "input",
        &[("type", "month"), ("name", ""), ("id", "")],
    ),
    (
        "input:week",
        "input",
        &[("type", "week"), ("name", ""), ("id", "")],
    ),
    (
        "input:time",
        "input",
        &[("type", "time"), ("name", ""), ("id", "")],
    ),
    (
        "input:tel",
        "input",
        &[("type", "tel"), ("name", ""), ("id", "")],
    ),
    (
        "input:number",
        "input",
        &[("type", "number"), ("name", ""), ("id", "")],
    ),
    (
        "input:color",
        "input",
        &[("type", "color"), ("name", ""), ("id", "")],
    ),
    (
        "input:checkbox",
        "input",
        &[("type", "checkbox"), ("name", ""), ("id", "")],
    ),
    (
        "input:c",
        "input",
        &[("type", "checkbox"), ("name", ""), ("id", "")],
    ),
    (
        "input:radio",
        "input",
        &[("type", "radio"), ("name", ""), ("id", "")],
    ),
    (
        "input:r",
        "input",
        &[("type", "radio"), ("name", ""), ("id", "")],
    ),
    (
        "input:range",
        "input",
        &[("type", "range"), ("name", ""), ("id", "")],
    ),
    (
        "input:file",
        "input",
        &[("type", "file"), ("name", ""), ("id", "")],
    ),
    (
        "input:f",
        "input",
        &[("type", "file"), ("name", ""), ("id", "")],
    ),
    (
        "input:submit",
        "input",
        &[("type", "submit"), ("value", "")],
    ),
    ("input:s", "input", &[("type", "submit"), ("value", "")]),
    (
        "input:image",
        "input",
        &[("type", "image"), ("src", ""), ("alt", "")],
    ),
    (
        "input:i",
        "input",
        &[("type", "image"), ("src", ""), ("alt", "")],
    ),
    (
        "input:button",
        "input",
        &[("type", "button"), ("value", "")],
    ),
    ("input:b", "input", &[("type", "button"), ("value", "")]),
    ("input:reset", "input", &[("type", "reset"), ("value", "")]),
    ("btn:s", "button", &[("type", "submit")]),
    ("btn:r", "button", &[("type", "reset")]),
    ("btn:d", "button", &[("disabled", "")]),
    ("button:s", "button", &[("type", "submit")]),
    ("button:submit", "button", &[("type", "submit")]),
    ("button:r", "button", &[("type", "reset")]),
    ("button:reset", "button", &[("type", "reset")]),
    ("button:d", "button", &[("disabled", "")]),
    ("button:disabled", "button", &[("disabled", "")]),
    ("a:link", "a", &[("href", "http://")]),
    ("a:mail", "a", &[("href", "mailto:")]),
    ("a:tel", "a", &[("href", "tel:+")]),
    (
        "link:css",
        "link",
        &[("rel", "stylesheet"), ("href", "style.css")],
    ),
    (
        "link:favicon",
        "link",
        &[
            ("rel", "shortcut icon"),
            ("type", "image/x-icon"),
            ("href", "favicon.ico"),
        ],
    ),
    ("script:src", "script", &[("src", "")]),
    ("form:get", "form", &[("action", ""), ("method", "get")]),
    ("form:post", "form", &[("action", ""), ("method", "post")]),
    ("meta:utf", "meta", &[("charset", "UTF-8")]),
    (
        "meta:vp",
        "meta",
        &[
            ("name", "viewport"),
            ("content", "width=device-width, initial-scale=1.0"),
        ],
    ),
];

fn alias(name: &str) -> Option<&'static Alias> {
    ALIASES.iter().find(|(n, _, _)| *n == name)
}

/// The standard lorem ipsum opening, cycled for longer runs.
const LOREM: &[&str] = &[
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipisicing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "ut",
    "labore",
    "et",
    "dolore",
    "magna",
    "aliqua",
    "ut",
    "enim",
    "ad",
    "minim",
    "veniam",
    "quis",
    "nostrud",
    "exercitation",
    "ullamco",
    "laboris",
    "nisi",
];

/// How many words `lorem`, `loremN`, `lipsum` or `lipsumN` asks for: 30
/// without a count, as in Emmet. `None` for any other word, and for a zero
/// or absurd count.
fn lorem_words(name: &str) -> Option<usize> {
    let rest = name
        .strip_prefix("lorem")
        .or_else(|| name.strip_prefix("lipsum"))?;
    if rest.is_empty() {
        return Some(30);
    }
    if !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok().filter(|n| (1..=MAX_REPEAT).contains(n))
}

/// `n` words of lorem ipsum as one sentence: capitalised, full stop.
fn lorem_text(n: usize) -> String {
    let words: Vec<&str> = LOREM.iter().copied().cycle().take(n).collect();
    let mut text = words.join(" ");
    if let Some(first) = text.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    text.push('.');
    text
}

/// Resolve aliases and lorem in `nodes` for `profile` (#1230). In HTML and
/// JSX a colon name must be an alias, or the abbreviation is refused
/// rather than written as a made-up tag; XML keeps namespaced names.
fn resolve(nodes: &mut [Node], profile: Profile) -> Option<()> {
    for node in nodes.iter_mut() {
        resolve(&mut node.children, profile)?;
        if node.group {
            continue;
        }
        if let Some(n) = lorem_words(&node.name)
            && node.is_lone_word()
        {
            node.name.clear();
            node.text = Some(lorem_text(n));
            node.bare_text = true;
            continue;
        }
        if profile == Profile::Xml || !node.name.contains(':') {
            continue;
        }
        let (_, tag, defaults) = alias(&node.name)?;
        node.name = tag.to_string();
        let mut attrs: Vec<(String, String)> = defaults
            .iter()
            .filter(|(k, _)| !(*k == "id" && node.id.is_some()))
            .filter(|(k, _)| !(*k == "class" && !node.classes.is_empty()))
            .map(|(k, v)| match node.attrs.iter().position(|(a, _)| a == k) {
                Some(i) => node.attrs.remove(i),
                None => (k.to_string(), v.to_string()),
            })
            .collect();
        attrs.append(&mut node.attrs);
        node.attrs = attrs;
    }
    // A lone lorem child is its parent's text, as `p>lorem5` writes
    // `<p>Lorem ipsum dolor sit amet.</p>` on one line.
    for node in nodes.iter_mut() {
        if node.text.is_none()
            && node.children.len() == 1
            && node.children[0].bare_text
            && node.children[0].repeat == 1
        {
            node.text = node.children.pop().and_then(|c| c.text);
        }
    }
    Some(())
}

/// The expansion of `abbr`, indented one level per depth with `indent`, or
/// `None` when `abbr` is not an abbreviation.
///
/// The returned offset is where the caret belongs: the first empty element's
/// content position, which is where typing continues. It is a byte offset
/// into the returned string.
pub fn expand(abbr: &str, indent: &str, profile: Profile) -> Option<(String, usize)> {
    let mut nodes = parse(abbr)?;
    // An alias on its own is HTML; XML reads the word as a namespaced tag,
    // which a bare word is not allowed to be.
    if profile == Profile::Xml
        && nodes.len() == 1
        && nodes[0].is_lone_word()
        && alias(&nodes[0].name).is_some()
    {
        return None;
    }
    resolve(&mut nodes, profile)?;
    if element_count(&nodes) > MAX_ELEMENTS {
        return None;
    }
    let mut r = Renderer {
        indent,
        profile,
        out: String::new(),
        caret: None,
    };
    r.render_all(&nodes, None, 0, 1)?;
    if r.out.is_empty() {
        return None;
    }
    // With no empty element the caret goes to the end of the last line, not
    // past its newline: the editor drops that newline when it splices the
    // expansion in.
    let caret = r.caret.unwrap_or(r.out.len() - 1);
    Some((r.out, caret))
}

/// How many elements `nodes` renders, saturating rather than overflowing on
/// an absurd product of repeats.
fn element_count(nodes: &[Node]) -> usize {
    nodes.iter().fold(0usize, |acc, n| {
        let own = usize::from(!n.group).saturating_add(element_count(&n.children));
        acc.saturating_add(n.repeat.saturating_mul(own))
    })
}

/// The abbreviation immediately left of `col` (a char index) in `line`, as a
/// char range. Returns `None` when there is nothing expandable there.
///
/// The scan stops at whitespace and at `<` and `>`, so an abbreviation typed
/// after existing markup on the same line is found without dragging the tag
/// before it into the expansion.
pub fn abbreviation_before(line: &str, col: usize) -> Option<(usize, String)> {
    let chars: Vec<char> = line.chars().collect();
    let end = col.min(chars.len());
    let mut start = end;
    // Brackets and braces may hold spaces (`a[title="Go home"]`,
    // `li{Item 1}`), which would otherwise end the scan.
    let mut depth = 0usize;
    while start > 0 {
        let c = chars[start - 1];
        match c {
            ']' | '}' => depth += 1,
            '[' | '{' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            _ if !is_abbr_char(c) => break,
            _ => {}
        }
        start -= 1;
    }
    // A `>` that closes a literal tag is markup, not the child operator, so
    // an abbreviation typed straight after `<div>` starts after it.
    start = start.max(last_tag_end(&chars[..end]));
    if start >= end {
        return None;
    }
    let abbr: String = chars[start..end].iter().collect();
    Some((start, abbr))
}

/// The index just past the last `>` that closes a literal `<...>` tag.
fn last_tag_end(chars: &[char]) -> usize {
    let mut in_tag = false;
    let mut out = 0;
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out = i + 1;
            }
            _ => {}
        }
    }
    out
}

fn is_abbr_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '.' | '#'
                | '>'
                | '+'
                | '*'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '='
                | '-'
                | '_'
                | '"'
                | '\''
                | '$'
                | ':'
                | '/'
                | '@'
                | '!'
                | '?'
                | '&'
                | ','
                | '^'
        )
}

/// The HTML element names a one-word abbreviation is allowed to be. Emmet
/// will happily turn any word into a tag, which is right in an editor where
/// expansion is an explicit keystroke on a known abbreviation — but croft's
/// chord can land on ordinary prose in an HTML buffer, and turning the last
/// word of a sentence into `<fox></fox>` is never what was meant. A word
/// carrying any other syntax (`.class`, `*3`, `[attr]`, `>child`) is
/// unambiguous and needs no list; only the bare word is checked.
///
/// Hyphenated names pass regardless: a `-` is the custom-element convention,
/// so `my-widget` is as intentional as `div`.
const KNOWN_TAGS: &[&str] = &[
    "a",
    "abbr",
    "address",
    "area",
    "article",
    "aside",
    "audio",
    "b",
    "base",
    "bdi",
    "bdo",
    "blockquote",
    "body",
    "br",
    "button",
    "canvas",
    "caption",
    "cite",
    "code",
    "col",
    "colgroup",
    "data",
    "datalist",
    "dd",
    "del",
    "details",
    "dfn",
    "dialog",
    "div",
    "dl",
    "dt",
    "em",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "i",
    "iframe",
    "img",
    "input",
    "ins",
    "kbd",
    "label",
    "legend",
    "li",
    "link",
    "main",
    "map",
    "mark",
    "menu",
    "meta",
    "meter",
    "nav",
    "noscript",
    "object",
    "ol",
    "optgroup",
    "option",
    "output",
    "p",
    "param",
    "picture",
    "pre",
    "progress",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "script",
    "search",
    "section",
    "select",
    "slot",
    "small",
    "source",
    "span",
    "strong",
    "style",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "time",
    "title",
    "tr",
    "track",
    "u",
    "ul",
    "var",
    "video",
    "wbr",
];

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

struct Parser<'a> {
    src: &'a [char],
    pos: usize,
    /// Levels a `^` run still has to climb (#1640): set where the run is
    /// read, spent one per child list on the way back up.
    climb: usize,
    /// A `^` read where there was no child list to climb out of (`x^2`).
    /// Emmet reads it as `+`, but that shape is far likelier to be math in
    /// prose, so the abbreviation is left alone.
    stray_caret: bool,
}

fn parse(abbr: &str) -> Option<Vec<Node>> {
    let chars: Vec<char> = abbr.trim().chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut p = Parser {
        src: &chars,
        pos: 0,
        climb: 0,
        stray_caret: false,
    };
    let nodes = p.sequence(true)?;
    if p.pos != p.src.len() || p.stray_caret {
        return None;
    }
    // Reject the degenerate parse that prose reaches: a single nameless,
    // classless, attribute-less node is not an abbreviation, it is a stray
    // punctuation character.
    if nodes.len() == 1 && nodes[0].is_bare() {
        return None;
    }
    if nodes.len() == 1 && nodes[0].is_lone_word() {
        let name = nodes[0].name.as_str();
        if !name.contains('-')
            && !KNOWN_TAGS.contains(&name)
            && alias(name).is_none()
            && lorem_words(name).is_none()
        {
            return None;
        }
    }
    Some(nodes)
}

impl Node {
    /// A name and nothing else — the one shape that is ambiguous with an
    /// ordinary English word.
    fn is_lone_word(&self) -> bool {
        self.repeat == 1
            && !self.group
            && self.id.is_none()
            && self.classes.is_empty()
            && self.attrs.is_empty()
            && self.text.is_none()
            && self.children.is_empty()
    }

    /// `{text}` with no tag, id, class or attribute: emitted as bare text,
    /// never wrapped in the parent's implicit tag (#1197).
    fn is_text_only(&self) -> bool {
        !self.group
            && self.name.is_empty()
            && self.id.is_none()
            && self.classes.is_empty()
            && self.attrs.is_empty()
            && self.text.is_some()
            && self.children.is_empty()
    }

    fn is_bare(&self) -> bool {
        self.name.is_empty()
            && self.id.is_none()
            && self.classes.is_empty()
            && self.attrs.is_empty()
            && self.text.is_none()
            && self.children.is_empty()
    }
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// `item (('+' | '^'+) item)*` — siblings, right-associative so that a
    /// `>` inside a later item keeps its own subtree.
    ///
    /// A run of `^` climbs out of this child list and as many more as it has
    /// carets (#1640): `a>b^c` is `a>b` then `c`. A `root` list (the whole
    /// abbreviation, or the inside of a `(...)` group) is as high as a climb
    /// goes, so extra carets stop there, as in Emmet.
    fn sequence(&mut self, root: bool) -> Option<Vec<Node>> {
        let mut out = Vec::new();
        loop {
            out.push(self.item()?);
            if self.climb > 0 {
                // A child list below just climbed out into this one.
                self.climb -= 1;
                if self.climb == 0 || root {
                    self.climb = 0;
                    continue;
                }
                return Some(out);
            }
            if self.eat('+') {
                continue;
            }
            let mut carets = 0;
            while self.eat('^') {
                carets += 1;
            }
            if carets == 0 {
                return Some(out);
            }
            if root {
                self.stray_caret = true;
                continue;
            }
            self.climb = carets;
            return Some(out);
        }
    }

    /// `(atom | '(' sequence ')') multiplier? ('>' sequence)?`
    fn item(&mut self) -> Option<Node> {
        let mut node = if self.eat('(') {
            let inner = self.sequence(true)?;
            if !self.eat(')') {
                return None;
            }
            Node {
                group: true,
                children: inner,
                repeat: 1,
                ..Node::default()
            }
        } else {
            self.atom()?
        };
        if self.eat('*') {
            node.repeat = self.number()?;
            if node.repeat == 0 || node.repeat > MAX_REPEAT {
                return None;
            }
            // Emmet takes an element's `#id`, `.class`, `[attrs]` and
            // `{text}` on either side of the multiplier: `li*3{Item $}` is
            // `li{Item $}*3` (#1197). A group has no tag to put them on.
            if !node.group {
                self.suffixes(&mut node)?;
            }
        }
        if self.eat('>') {
            let kids = self.sequence(false)?;
            if node.group {
                // `(a+b)>c` would have to distribute c over both, which
                // Emmet does not do either.
                return None;
            }
            node.children = kids;
        }
        Some(node)
    }

    fn number(&mut self) -> Option<usize> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if start == self.pos {
            return None;
        }
        self.src[start..self.pos]
            .iter()
            .collect::<String>()
            .parse()
            .ok()
    }

    /// `name? ('#'id | '.'class | '['attrs']' | '{'text'}')*`
    fn atom(&mut self) -> Option<Node> {
        let mut node = Node {
            repeat: 1,
            ..Node::default()
        };
        node.name = self.ident();
        self.suffixes(&mut node)?;
        if node.is_bare() {
            return None;
        }
        Some(node)
    }

    /// `('#'id | '.'class | '['attrs']' | '{'text'}')*` onto `node`.
    fn suffixes(&mut self, node: &mut Node) -> Option<()> {
        loop {
            match self.peek() {
                Some('#') => {
                    self.pos += 1;
                    let v = self.ident();
                    if v.is_empty() {
                        return None;
                    }
                    node.id = Some(v);
                }
                Some('.') => {
                    self.pos += 1;
                    let v = self.ident();
                    if v.is_empty() {
                        return None;
                    }
                    node.classes.push(v);
                }
                Some('[') => {
                    self.pos += 1;
                    // `a[href=x][title=y]` keeps both groups.
                    let attrs = self.attrs()?;
                    node.attrs.extend(attrs);
                }
                Some('{') => {
                    self.pos += 1;
                    node.text = Some(self.text()?);
                }
                _ => return Some(()),
            }
        }
    }

    /// A tag / class / id name. `$` is kept so the renderer can substitute
    /// the repetition index.
    fn ident(&mut self) -> String {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '$' | ':' | '!'))
        {
            self.pos += 1;
        }
        self.src[start..self.pos].iter().collect()
    }

    /// `[a=1 b="two" c]` — space separated, values optionally quoted.
    fn attrs(&mut self) -> Option<Vec<(String, String)>> {
        let mut out = Vec::new();
        loop {
            while self.eat(' ') {}
            if self.eat(']') {
                return Some(out);
            }
            let name = self.ident();
            if name.is_empty() {
                return None;
            }
            let value = if self.eat('=') {
                match self.peek() {
                    Some(q @ ('"' | '\'')) => {
                        self.pos += 1;
                        let start = self.pos;
                        while self.peek().is_some_and(|c| c != q) {
                            self.pos += 1;
                        }
                        let v: String = self.src[start..self.pos].iter().collect();
                        if !self.eat(q) {
                            return None;
                        }
                        v
                    }
                    _ => {
                        let start = self.pos;
                        while self.peek().is_some_and(|c| c != ' ' && c != ']') {
                            self.pos += 1;
                        }
                        self.src[start..self.pos].iter().collect()
                    }
                }
            } else {
                String::new()
            };
            out.push((name, value));
        }
    }

    /// `{...}`, which may hold anything but the closing brace.
    fn text(&mut self) -> Option<String> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c != '}') {
            self.pos += 1;
        }
        let t: String = self.src[start..self.pos].iter().collect();
        if !self.eat('}') {
            return None;
        }
        Some(t)
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The tag an implicit name resolves to, given its parent. `ul>.item` means
/// `li.item`, which is the whole reason the shorthand is worth having.
fn implicit_tag(parent: Option<&str>) -> &'static str {
    match parent {
        Some("ul" | "ol") => "li",
        Some("table" | "tbody" | "thead" | "tfoot") => "tr",
        Some("tr") => "td",
        Some("select" | "optgroup") => "option",
        Some("dl") => "dt",
        Some("map") => "area",
        _ => "div",
    }
}

struct Renderer<'a> {
    indent: &'a str,
    profile: Profile,
    out: String,
    /// Byte offset of the first empty element's content, recorded as it is
    /// rendered. Searching the output afterwards would mistake `></` inside
    /// text or an attribute value for an element boundary.
    caret: Option<usize>,
}

impl Renderer<'_> {
    /// `None` when the abbreviation asks for something markup cannot hold.
    fn render_all(
        &mut self,
        nodes: &[Node],
        parent: Option<&str>,
        depth: usize,
        inherited: usize,
    ) -> Option<()> {
        for node in nodes {
            for i in 1..=node.repeat {
                // A node without its own repetition numbers from the nearest
                // repeated ancestor, so `li.item*3>a{Link $}` counts its links.
                let index = if node.repeat > 1 { i } else { inherited };
                self.render(node, parent, depth, index)?;
            }
        }
        Some(())
    }

    fn render(
        &mut self,
        node: &Node,
        parent: Option<&str>,
        depth: usize,
        index: usize,
    ) -> Option<()> {
        if node.group {
            return self.render_all(&node.children, parent, depth, index);
        }
        if node.bare_text {
            self.out.push_str(&self.indent.repeat(depth));
            self.out.push_str(node.text.as_deref().unwrap_or(""));
            self.out.push('\n');
            return Some(());
        }

        // A text node is its text on its own line, as Emmet lays out
        // `p>{Click }+a{here}+{ to continue}` (#1197).
        if node.is_text_only() {
            let text = number(node.text.as_deref().unwrap_or(""), index);
            self.out.push_str(&self.indent.repeat(depth));
            self.out.push_str(&text);
            self.out.push('\n');
            return Some(());
        }
        let name = if node.name.is_empty() {
            implicit_tag(parent).to_string()
        } else {
            number(&node.name, index)
        };
        let void = VOID_ELEMENTS.contains(&name.as_str());
        // A void element has nowhere to put content, so `img>span` would
        // silently lose the `span`.
        if void && (!node.children.is_empty() || node.text.is_some()) {
            return None;
        }
        let jsx = self.profile == Profile::Jsx;

        let pad = self.indent.repeat(depth);
        self.out.push_str(&pad);
        self.out.push('<');
        self.out.push_str(&name);
        // Emmet's HTML snippets give a link an empty `href` to fill in.
        if name == "a" && !node.attrs.iter().any(|(k, _)| k == "href") {
            self.out.push_str(" href=\"\"");
        }
        if let Some(id) = &node.id {
            self.out.push_str(&format!(" id=\"{}\"", number(id, index)));
        }
        if !node.classes.is_empty() {
            let classes: Vec<String> = node.classes.iter().map(|c| number(c, index)).collect();
            let key = if jsx { "className" } else { "class" };
            self.out
                .push_str(&format!(" {key}=\"{}\"", classes.join(" ")));
        }
        for (k, v) in &node.attrs {
            let k = match (jsx, k.as_str()) {
                (true, "class") => "className",
                (true, "for") => "htmlFor",
                _ => k,
            };
            let v = number(v, index).replace('"', "&quot;");
            self.out.push_str(&format!(" {k}=\"{v}\""));
        }

        if void {
            self.out.push_str(match self.profile {
                Profile::Html => ">\n",
                Profile::Xml => "/>\n",
                Profile::Jsx => " />\n",
            });
            return Some(());
        }
        self.out.push('>');

        if !node.children.is_empty() {
            self.out.push('\n');
            self.render_all(&node.children, Some(&name), depth + 1, index)?;
            self.out.push_str(&pad);
        } else {
            let text = node.text.as_deref().map(|t| number(t, index));
            if text.as_deref().unwrap_or("").is_empty() && self.caret.is_none() {
                self.caret = Some(self.out.len());
            }
            if let Some(t) = text {
                self.out.push_str(&t);
            }
        }
        self.out.push_str(&format!("</{name}>\n"));
        Some(())
    }
}

/// Substitute `$` runs with `index`, zero-padded to the run's length —
/// `item$` gives `item1`, `item$$$` gives `item001`. A `$` with no
/// repetition still numbers, because `div.item$` is `item1`.
fn number(s: &str, index: usize) -> String {
    if !s.contains('$') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            let start = i;
            while i < chars.len() && chars[i] == '$' {
                i += 1;
            }
            let width = i - start;
            out.push_str(&format!("{index:0width$}"));
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quote_in_an_attribute_value_is_escaped() {
        let out = super::expand("a[title='say \"hi\"']", "  ", Profile::Html)
            .unwrap()
            .0;
        assert!(out.contains("title=\"say &quot;hi&quot;\""), "{out}");
    }

    /// The HTML profile, which is what every test below means unless it
    /// names another.
    fn expand(abbr: &str, indent: &str) -> Option<(String, usize)> {
        super::expand(abbr, indent, Profile::Html)
    }

    fn ex(abbr: &str) -> String {
        expand(abbr, "  ").expect("should expand").0
    }

    #[test]
    fn a_bare_tag_expands_to_a_tag_pair() {
        assert_eq!(ex("div"), "<div></div>\n");
    }

    #[test]
    fn a_class_without_a_tag_implies_a_div() {
        assert_eq!(ex(".wrapper"), "<div class=\"wrapper\"></div>\n");
    }

    #[test]
    fn ids_and_several_classes_combine() {
        assert_eq!(
            ex("section#hero.a.b"),
            "<section id=\"hero\" class=\"a b\"></section>\n"
        );
    }

    #[test]
    fn the_child_operator_nests_and_indents() {
        assert_eq!(
            ex("div>span"),
            "<div>\n  <span></span>\n</div>\n",
            "one indent level per depth"
        );
    }

    #[test]
    fn the_sibling_operator_keeps_nodes_at_the_same_level() {
        assert_eq!(ex("h1+p"), "<h1></h1>\n<p></p>\n");
    }

    /// `a>b+c` gives `a` two children, not a chain: the sibling operator
    /// binds inside the child list it was written in.
    #[test]
    fn a_sibling_after_a_child_stays_a_child_of_the_same_parent() {
        assert_eq!(ex("div>h1+p"), "<div>\n  <h1></h1>\n  <p></p>\n</div>\n");
    }

    #[test]
    fn multiplication_repeats_a_node() {
        assert_eq!(
            ex("ul>li*3"),
            "<ul>\n  <li></li>\n  <li></li>\n  <li></li>\n</ul>\n"
        );
    }

    #[test]
    fn an_implicit_child_of_a_list_is_an_li() {
        assert_eq!(
            ex("ul>.item*2"),
            "<ul>\n  <li class=\"item\"></li>\n  <li class=\"item\"></li>\n</ul>\n"
        );
    }

    #[test]
    fn an_implicit_child_of_a_row_is_a_cell() {
        assert_eq!(ex("tr>.cell"), "<tr>\n  <td class=\"cell\"></td>\n</tr>\n");
    }

    /// #1197: `{text}`, `.class` and the rest may follow the multiplier, as
    /// in Emmet.
    #[test]
    fn text_after_the_multiplier_belongs_to_the_element() {
        assert_eq!(
            ex("li*3{Item $}"),
            "<li>Item 1</li>\n<li>Item 2</li>\n<li>Item 3</li>\n"
        );
        assert_eq!(
            ex("ul>li.item$*2{Item $}"),
            "<ul>\n  <li class=\"item1\">Item 1</li>\n  <li class=\"item2\">Item 2</li>\n</ul>\n"
        );
        assert_eq!(
            ex("li*2.x"),
            "<li class=\"x\"></li>\n<li class=\"x\"></li>\n"
        );
    }

    /// #1197: a `{text}` with no tag is bare text on its own line, the
    /// example from Emmet's docs, never a `<div>`.
    #[test]
    fn a_text_node_is_bare_text() {
        assert_eq!(
            ex("p>{Click }+a{here}+{ to continue}"),
            "<p>\n  Click \n  <a href=\"\">here</a>\n   to continue\n</p>\n"
        );
        assert_eq!(ex("ul>{Item $}*2"), "<ul>\n  Item 1\n  Item 2\n</ul>\n");
    }

    /// Emmet's HTML profile gives a link an empty `href`; one the
    /// abbreviation names is kept as it is.
    #[test]
    fn a_link_gets_an_empty_href_unless_it_names_one() {
        assert_eq!(ex("a"), "<a href=\"\"></a>\n");
        assert_eq!(ex("a[href=#top]"), "<a href=\"#top\"></a>\n");
    }

    /// Negative (#1197): text on a tag, class or id is still that element's
    /// content, text before the multiplier still works, and a group or junk
    /// after the multiplier still refuses to expand.
    #[test]
    fn text_on_an_element_and_bad_multipliers_are_unchanged() {
        assert_eq!(ex(".x{hi}"), "<div class=\"x\">hi</div>\n");
        assert_eq!(ex("ul>.x{hi}"), "<ul>\n  <li class=\"x\">hi</li>\n</ul>\n");
        assert_eq!(ex("li{Item $}*2"), "<li>Item 1</li>\n<li>Item 2</li>\n");
        assert!(expand("(li)*2{x}", "  ").is_none());
        assert!(expand("li*3x", "  ").is_none());
    }

    #[test]
    fn the_dollar_placeholder_numbers_each_repetition() {
        assert_eq!(
            ex("li.item$*3"),
            "<li class=\"item1\"></li>\n<li class=\"item2\"></li>\n<li class=\"item3\"></li>\n"
        );
    }

    #[test]
    fn a_run_of_dollars_zero_pads_to_its_own_width() {
        assert_eq!(
            ex("li#row$$$*2"),
            "<li id=\"row001\"></li>\n<li id=\"row002\"></li>\n"
        );
    }

    #[test]
    fn text_in_braces_renders_inline() {
        assert_eq!(ex("p{Hello}"), "<p>Hello</p>\n");
    }

    #[test]
    fn text_is_numbered_too() {
        assert_eq!(ex("li{Item $}*2"), "<li>Item 1</li>\n<li>Item 2</li>\n");
    }

    #[test]
    fn attributes_parse_bare_quoted_and_valueless() {
        assert_eq!(
            ex("a[href=# title=\"Go home\" download]"),
            "<a href=\"#\" title=\"Go home\" download=\"\"></a>\n"
        );
    }

    #[test]
    fn groups_repeat_as_a_unit() {
        assert_eq!(
            ex("(li>a)*2"),
            "<li>\n  <a href=\"\"></a>\n</li>\n<li>\n  <a href=\"\"></a>\n</li>\n"
        );
    }

    #[test]
    fn a_group_can_sit_beside_other_nodes() {
        assert_eq!(
            ex("(h1+p)+footer"),
            "<h1></h1>\n<p></p>\n<footer></footer>\n"
        );
    }

    /// Emmet's html profile writes void elements bare, with no closing tag
    /// and no self-closing slash.
    #[test]
    fn void_elements_get_no_closing_tag() {
        assert_eq!(ex("br"), "<br>\n");
        assert_eq!(ex("img[src=a.png]"), "<img src=\"a.png\">\n");
    }

    #[test]
    fn a_realistic_abbreviation_builds_a_whole_block() {
        assert_eq!(
            ex("nav.menu>ul>li.item$*3>a[href=#]{Link $}"),
            concat!(
                "<nav class=\"menu\">\n",
                "  <ul>\n",
                "    <li class=\"item1\">\n",
                "      <a href=\"#\">Link 1</a>\n",
                "    </li>\n",
                "    <li class=\"item2\">\n",
                "      <a href=\"#\">Link 2</a>\n",
                "    </li>\n",
                "    <li class=\"item3\">\n",
                "      <a href=\"#\">Link 3</a>\n",
                "    </li>\n",
                "  </ul>\n",
                "</nav>\n"
            )
        );
    }

    #[test]
    fn the_indent_string_is_the_callers() {
        assert_eq!(
            expand("div>p", "\t").unwrap().0,
            "<div>\n\t<p></p>\n</div>\n"
        );
    }

    #[test]
    fn the_caret_lands_inside_the_first_empty_element() {
        let (out, caret) = expand("div>span", "  ").unwrap();
        assert_eq!(&out[caret..caret + 7], "</span>");
    }

    /// The end of the expansion's last line, not past its trailing newline:
    /// the editor drops that newline when it splices the lines in, so an
    /// offset beyond it lands on no line at all.
    #[test]
    fn the_caret_falls_to_the_end_when_nothing_is_empty() {
        let (out, caret) = expand("p{hi}", "  ").unwrap();
        assert_eq!(caret, out.len() - 1);
        assert_eq!(&out[caret..], "\n");
    }

    /// The caret is placed from the element structure, so markup-like text
    /// in a brace or an attribute value cannot pass for an empty element.
    #[test]
    fn markup_inside_text_does_not_capture_the_caret() {
        let (out, caret) = expand("p{a></b}+span", "  ").unwrap();
        assert_eq!(&out[caret..], "</span>\n");
    }

    #[test]
    fn markup_inside_an_attribute_value_does_not_capture_the_caret() {
        let (out, caret) = expand("a[title=\"></\"]", "  ").unwrap();
        assert_eq!(out, "<a href=\"\" title=\"></\"></a>\n");
        assert_eq!(&out[caret..], "</a>\n");
    }

    #[test]
    fn every_attribute_group_is_kept() {
        assert_eq!(ex("a[href=x][title=y]"), "<a href=\"x\" title=\"y\"></a>\n");
    }

    /// A void element has nowhere to put children, so accepting `img>span`
    /// would silently throw the `span` away.
    #[test]
    fn a_void_element_with_content_does_not_expand() {
        assert!(expand("img>span", "  ").is_none());
        assert!(expand("br{text}", "  ").is_none());
        // An implicit name resolving to a void element is caught too.
        assert!(expand("map>.x>span", "  ").is_none());
        assert!(expand("img+span", "  ").is_some());
    }

    #[test]
    fn xml_self_closes_void_elements() {
        assert_eq!(
            super::expand("br+img[src=a]", "  ", Profile::Xml)
                .unwrap()
                .0,
            "<br/>\n<img src=\"a\"/>\n"
        );
    }

    /// JSX refuses an unclosed tag, and spells `class` and `for` its own way.
    #[test]
    fn jsx_self_closes_and_renames_attributes() {
        assert_eq!(
            super::expand("img.hero+label[for=q]", "  ", Profile::Jsx)
                .unwrap()
                .0,
            "<img className=\"hero\" />\n<label htmlFor=\"q\"></label>\n"
        );
    }

    // ---- Things that must NOT expand -------------------------------------

    #[test]
    fn prose_does_not_expand() {
        assert!(expand("just some words", "  ").is_none());
    }

    /// A single English word in an HTML buffer is far more likely to be
    /// prose than a custom element, so the bare form is gated on the tag
    /// list.
    #[test]
    fn a_bare_word_that_is_not_a_tag_does_not_expand() {
        assert!(expand("fox", "  ").is_none());
        assert!(expand("section", "  ").is_some());
    }

    /// Custom elements must contain a hyphen, which is enough to tell them
    /// apart from prose without listing every one.
    #[test]
    fn a_hyphenated_custom_element_expands() {
        assert_eq!(ex("my-widget"), "<my-widget></my-widget>\n");
    }

    /// The gate is only for the ambiguous bare form. Any other syntax makes
    /// the intent explicit, so an unknown name is taken at its word.
    #[test]
    fn an_unknown_name_with_other_syntax_still_expands() {
        assert_eq!(ex("fox.red"), "<fox class=\"red\"></fox>\n");
        assert_eq!(ex("fox>cub"), "<fox>\n  <cub></cub>\n</fox>\n");
    }

    // ---- Climbing up (#1640) ---------------------------------------------

    /// The issue's abbreviation: `^` climbs out of `nav`, so the `h1` is
    /// `nav`'s sibling inside `header`, and nothing is left as text.
    #[test]
    fn a_caret_climbs_out_of_the_child_list() {
        assert_eq!(
            ex("header>nav>a*2^h1"),
            "<header>\n  <nav>\n    <a href=\"\"></a>\n    <a href=\"\"></a>\n  </nav>\n  <h1></h1>\n</header>\n"
        );
    }

    #[test]
    fn each_caret_climbs_one_level() {
        assert_eq!(
            ex("div>ul>li^p"),
            "<div>\n  <ul>\n    <li></li>\n  </ul>\n  <p></p>\n</div>\n"
        );
        assert_eq!(
            ex("div>ul>li^^p"),
            "<div>\n  <ul>\n    <li></li>\n  </ul>\n</div>\n<p></p>\n"
        );
        assert_eq!(
            ex("div.card>h2^div.card>h2"),
            "<div class=\"card\">\n  <h2></h2>\n</div>\n<div class=\"card\">\n  <h2></h2>\n</div>\n"
        );
    }

    /// Extra carets stop at the top, and at the top of a group, as in Emmet.
    #[test]
    fn extra_carets_stop_at_the_top_and_at_a_group() {
        assert_eq!(
            ex("div>p^^span"),
            "<div>\n  <p></p>\n</div>\n<span></span>\n"
        );
        assert_eq!(
            ex("section>(ul>li^^^p)+footer"),
            "<section>\n  <ul>\n    <li></li>\n  </ul>\n  <p></p>\n  <footer></footer>\n</section>\n"
        );
    }

    /// A climb continues the outer list, so `+` and `*` after it keep working.
    #[test]
    fn siblings_and_repeats_follow_a_climb() {
        assert_eq!(
            ex("ul>li^p*2+hr"),
            "<ul>\n  <li></li>\n</ul>\n<p></p>\n<p></p>\n<hr>\n"
        );
    }

    /// The scan back from the caret takes the whole abbreviation, `^` and
    /// all, so nothing before it is left behind as text.
    #[test]
    fn the_abbreviation_before_the_caret_includes_its_carets() {
        let line = "  header>nav>a*2^h1";
        let (start, abbr) = abbreviation_before(line, line.chars().count()).unwrap();
        assert_eq!(start, 2);
        assert_eq!(abbr, "header>nav>a*2^h1");
    }

    /// Negative: a caret with nothing to climb into, or nothing to climb out
    /// of, does not expand, so the chord stays inert on `x^2` in prose.
    #[test]
    fn a_caret_that_climbs_nothing_does_not_expand() {
        assert!(expand("ul>li^", "  ").is_none(), "nothing after the climb");
        assert!(expand("x^2", "  ").is_none(), "math, not markup");
        assert!(expand("2^10", "  ").is_none());
        assert!(
            expand("(p^span)", "  ").is_none(),
            "a group's top climbs nowhere"
        );
    }

    /// Negative: abbreviations without a caret are unchanged.
    #[test]
    fn abbreviations_without_a_caret_are_unchanged() {
        assert_eq!(
            ex("ul>li*3"),
            "<ul>\n  <li></li>\n  <li></li>\n  <li></li>\n</ul>\n"
        );
        assert_eq!(ex("div>h1+p"), "<div>\n  <h1></h1>\n  <p></p>\n</div>\n");
        assert_eq!(ex("p{2^10}"), "<p>2^10</p>\n", "a caret in text is text");
    }

    #[test]
    fn an_unbalanced_abbreviation_does_not_expand() {
        assert!(expand("div>(span", "  ").is_none());
        assert!(expand("a[href=#", "  ").is_none());
        assert!(expand("p{text", "  ").is_none());
    }

    #[test]
    fn a_dangling_operator_does_not_expand() {
        assert!(expand("div>", "  ").is_none());
        assert!(expand("div+", "  ").is_none());
        assert!(expand("*3", "  ").is_none());
    }

    #[test]
    fn an_empty_abbreviation_does_not_expand() {
        assert!(expand("", "  ").is_none());
        assert!(expand("   ", "  ").is_none());
    }

    /// A typo'd repeat count would otherwise build a multi-megabyte string
    /// and freeze the editor.
    #[test]
    fn an_absurd_repeat_count_is_refused() {
        assert!(expand("div*1000", "  ").is_some());
        assert!(expand("div*1001", "  ").is_none());
        assert!(expand("div*0", "  ").is_none());
    }

    /// The per-node cap does not bound a product of repeats: nested, it
    /// would render a million spans. The total output is capped as well.
    #[test]
    fn a_nested_product_of_repeats_is_refused() {
        assert!(expand("div*1000>span*1000", "  ").is_none());
        assert!(expand("(div>(span*1000))*1000", "  ").is_none());
        assert!(expand("div*100>span*100", "  ").is_some());
    }

    // ---- Locating the abbreviation in a line ------------------------------

    #[test]
    fn the_abbreviation_is_taken_from_the_caret_leftwards() {
        let (start, abbr) = abbreviation_before("  ul>li*3", 9).unwrap();
        assert_eq!(start, 2);
        assert_eq!(abbr, "ul>li*3");
    }

    #[test]
    fn leading_indentation_is_not_part_of_the_abbreviation() {
        let (start, _) = abbreviation_before("\t\tdiv", 5).unwrap();
        assert_eq!(start, 2);
    }

    /// An abbreviation typed after existing markup must not swallow it.
    #[test]
    fn an_earlier_tag_on_the_line_is_not_swallowed() {
        let (start, abbr) = abbreviation_before("<div>span", 9).unwrap();
        assert_eq!(start, 5);
        assert_eq!(abbr, "span");
    }

    #[test]
    fn nothing_to_the_left_means_no_abbreviation() {
        assert!(abbreviation_before("   ", 3).is_none());
        assert!(abbreviation_before("div", 0).is_none());
    }

    // ---- Aliases and lorem (#1230) -----------------------------------------

    /// #1230: the issue's abbreviation expands like Emmet: typed inputs with
    /// no closing tag, a submit button, and lorem text in the paragraph.
    #[test]
    fn colon_aliases_and_lorem_expand_inside_a_compound() {
        let out = expand("form>input:email+input:password+btn:s+p>lorem5", "  ")
            .unwrap()
            .0;
        assert_eq!(
            out,
            "<form>\n  <input type=\"email\" name=\"\" id=\"\">\n  \
             <input type=\"password\" name=\"\" id=\"\">\n  \
             <button type=\"submit\"></button>\n  \
             <p>Lorem ipsum dolor sit amet.</p>\n</form>\n"
        );
    }

    /// #1230: an alias on its own is an abbreviation now, like the tag it
    /// names.
    #[test]
    fn a_lone_alias_expands() {
        assert_eq!(
            expand("input:text", "  ").unwrap().0,
            "<input type=\"text\" name=\"\" id=\"\">\n"
        );
        assert_eq!(
            expand("a:link", "  ").unwrap().0,
            "<a href=\"http://\"></a>\n"
        );
        assert_eq!(
            expand("link:css", "  ").unwrap().0,
            "<link rel=\"stylesheet\" href=\"style.css\">\n"
        );
        assert_eq!(
            expand("form:post", "  ").unwrap().0,
            "<form action=\"\" method=\"post\"></form>\n"
        );
    }

    /// #1230: what the abbreviation spells wins over the alias's defaults,
    /// and an `#id` replaces the alias's empty `id` rather than doubling it.
    #[test]
    fn an_alias_takes_the_abbreviations_own_id_and_attributes() {
        assert_eq!(
            expand("input:text#q[name=query]", "  ").unwrap().0,
            "<input id=\"q\" type=\"text\" name=\"query\">\n"
        );
    }

    /// #1230: JSX closes the aliased void element its own way.
    #[test]
    fn an_alias_in_jsx_is_self_closed() {
        assert_eq!(
            super::expand("input:checkbox", "  ", Profile::Jsx)
                .unwrap()
                .0,
            "<input type=\"checkbox\" name=\"\" id=\"\" />\n"
        );
    }

    /// #1230: lorem on its own, or beside other elements, is bare text;
    /// `loremN` takes N words and ends a sentence.
    #[test]
    fn lorem_is_text() {
        assert_eq!(expand("lorem3", "  ").unwrap().0, "Lorem ipsum dolor.\n");
        assert_eq!(
            expand("div>h1+lorem4", "  ").unwrap().0,
            "<div>\n  <h1></h1>\n  Lorem ipsum dolor sit.\n</div>\n"
        );
        let words = expand("lorem", "  ").unwrap().0;
        assert_eq!(words.split_whitespace().count(), 30, "{words}");
    }

    /// #1230 negative: a colon name that is no alias refuses the whole
    /// abbreviation in HTML, instead of writing `<input:bogus>`.
    #[test]
    fn an_unknown_colon_name_refuses_in_html() {
        assert!(expand("form>input:bogus", "  ").is_none());
        assert!(expand("div>svg:rect", "  ").is_none());
        assert!(super::expand("form>btn:x", "  ", Profile::Jsx).is_none());
    }

    /// #1230 negative: XML keeps namespaced names as they are, and does not
    /// read HTML aliases into them.
    #[test]
    fn xml_keeps_namespaced_names() {
        assert_eq!(
            super::expand("xsl:template>xsl:value-of", "  ", Profile::Xml)
                .unwrap()
                .0,
            "<xsl:template>\n  <xsl:value-of></xsl:value-of>\n</xsl:template>\n"
        );
        assert!(super::expand("input:email", "  ", Profile::Xml).is_none());
    }

    /// #1230 negative: a word that only starts like lorem is still prose.
    #[test]
    fn a_word_like_lorem_is_not_lorem() {
        assert!(expand("loremipsum", "  ").is_none());
        assert!(expand("lorem0", "  ").is_none());
    }
}
