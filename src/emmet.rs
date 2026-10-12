//! Emmet abbreviation expansion (#352's `vscode.emmet` gap).
//!
//! VS Code ships Emmet in the box, and it is the single biggest reason
//! writing HTML there feels different from writing it in a plain editor:
//! `ul>li.item*3` becomes a three-item list in one keystroke. nvim and Zed
//! both reach for it through plugins for the same reason.
//!
//! This is the abbreviation subset that covers ordinary markup authoring —
//! nesting, siblings, grouping, repetition, numbering, ids, classes,
//! attributes and text. Deliberately absent:
//!
//! * The climb-up operator `^`. It only ever saves a pair of parentheses,
//!   and it is the one piece of the grammar whose meaning readers argue
//!   about.
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
    /// Wrap with Abbreviation (#1557): lines this element holds, one level
    /// in, in place of children.
    wrapped: Option<Vec<String>>,
    /// Wrap with Abbreviation's `li*` (#1557): repetition `i` holds line
    /// `i` as its text.
    per_line: Option<Vec<String>>,
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
}

fn parse(abbr: &str) -> Option<Vec<Node>> {
    let chars: Vec<char> = abbr.trim().chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut p = Parser {
        src: &chars,
        pos: 0,
    };
    let nodes = p.sequence()?;
    if p.pos != p.src.len() {
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

    /// `item ('+' sequence)?` — siblings, right-associative so that a `>`
    /// inside a later item keeps its own subtree.
    fn sequence(&mut self) -> Option<Vec<Node>> {
        let mut out = vec![self.item()?];
        while self.eat('+') {
            out.push(self.item()?);
        }
        Some(out)
    }

    /// `(atom | '(' sequence ')') multiplier? ('>' sequence)?`
    fn item(&mut self) -> Option<Node> {
        let mut node = if self.eat('(') {
            let inner = self.sequence()?;
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
            let kids = self.sequence()?;
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
// Editing existing markup (#1557): Wrap with Abbreviation, Remove Tag,
// Update Tag, Go to Matching Pair and Balance, VS Code's Emmet commands for
// markup already written.

/// What Wrap with Abbreviation puts in the abbreviation's innermost element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapContent<'a> {
    /// Whole lines, already without their shared indentation: they go one
    /// level inside the element, each on its own line.
    Lines(&'a [String]),
    /// Part of one line: it becomes the element's text, on the same line.
    Inline(&'a str),
}

/// `abbr` expanded around `content` (#1557), VS Code's Emmet: Wrap with
/// Abbreviation. The content goes in the abbreviation's innermost last
/// element, so `nav.menu` around a `<ul>` gives a `<nav class="menu">`
/// holding it, one level in. A bare `*` repeats that element once per
/// line, each holding its line as text, so `ul>li*` around three lines
/// gives three `li`. Lines are indented with `indent` per level and the
/// result ends in a newline. `None` when `abbr` is not an abbreviation or
/// its innermost element can hold nothing (`img`).
pub fn wrap(
    abbr: &str,
    content: WrapContent<'_>,
    indent: &str,
    profile: Profile,
) -> Option<String> {
    let lines: Vec<String> = match content {
        WrapContent::Lines(lines) => lines.to_vec(),
        WrapContent::Inline(text) => vec![text.to_string()],
    };
    // `li*` names no count: it is one repetition per line.
    let chars: Vec<char> = abbr.trim().chars().collect();
    let mut spelled = String::new();
    let mut per_line = false;
    for (i, &c) in chars.iter().enumerate() {
        spelled.push(c);
        if c == '*' && !chars.get(i + 1).is_some_and(char::is_ascii_digit) {
            spelled.push_str(&lines.len().max(1).to_string());
            per_line = true;
        }
    }
    let chars: Vec<char> = spelled.chars().collect();
    let mut p = Parser {
        src: &chars,
        pos: 0,
    };
    // Not `parse`: a lone word is refused there only because, typed in
    // prose, it is more likely a word than a tag. Here it was asked for.
    let mut nodes = p.sequence()?;
    if p.pos != p.src.len() || (nodes.len() == 1 && nodes[0].is_bare()) {
        return None;
    }
    resolve(&mut nodes, profile)?;
    if element_count(&nodes) > MAX_ELEMENTS {
        return None;
    }
    let target = innermost_last(&mut nodes)?;
    // A void element (`img`, `br`) holds nothing, so it wraps nothing.
    if target.is_text_only()
        || target.bare_text
        || VOID_ELEMENTS.contains(&target.name.to_ascii_lowercase().as_str())
    {
        return None;
    }
    match content {
        WrapContent::Inline(text) => target.text = Some(text.to_string()),
        WrapContent::Lines(_) if per_line && target.repeat == lines.len().max(1) => {
            target.per_line = Some(lines.iter().map(|l| l.trim().to_string()).collect());
        }
        WrapContent::Lines(_) => target.wrapped = Some(lines),
    }
    let mut r = Renderer {
        indent,
        profile,
        out: String::new(),
        caret: None,
    };
    r.render_all(&nodes, None, 0, 1)?;
    (!r.out.is_empty()).then_some(r.out)
}

/// The element the wrapped content goes in: the last one, at the deepest
/// level, as Emmet places it.
fn innermost_last(nodes: &mut [Node]) -> Option<&mut Node> {
    let last = nodes.last_mut()?;
    if !last.children.is_empty() {
        return innermost_last(&mut last.children);
    }
    (!last.group).then_some(last)
}

/// One element of existing markup, by byte range: its opening tag and,
/// unless it closes itself (`<br>`, `<img/>`), its closing tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub open: std::ops::Range<usize>,
    pub close: Option<std::ops::Range<usize>>,
}

impl Element {
    /// From the start of its opening tag to the end of its closing one.
    pub fn outer(&self) -> std::ops::Range<usize> {
        self.open.start..self.close.as_ref().map_or(self.open.end, |c| c.end)
    }

    /// Between its tags; empty for an element that closes itself.
    pub fn inner(&self) -> std::ops::Range<usize> {
        self.open.end..self.close.as_ref().map_or(self.open.end, |c| c.start)
    }
}

/// Every element of `text`, its tags paired, in document order of their
/// opening tags. Comments, `<!doctype>`, `<?xml?>`, quoted attribute values
/// and JSX `{...}` attributes are skipped, the bodies of `<script>` and
/// `<style>` are not read as markup, and HTML's void elements need no
/// closing tag. A closing tag with no opening one is ignored, and an
/// opening tag never closed is left out.
pub fn elements(text: &str) -> Vec<Element> {
    let b = text.as_bytes();
    let mut out: Vec<Element> = Vec::new();
    let mut open: Vec<(String, std::ops::Range<usize>)> = Vec::new();
    let mut i = 0;
    while let Some(off) = text[i..].find('<') {
        let start = i + off;
        let rest = &text[start..];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(text.len(), |e| start + e + 3);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            i = rest.find('>').map_or(text.len(), |e| start + e + 1);
            continue;
        }
        let closing = rest.starts_with("</");
        let name_at = start + if closing { 2 } else { 1 };
        let name_len = text[name_at..]
            .find(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')))
            .unwrap_or(text.len() - name_at);
        if name_len == 0 || !b[name_at].is_ascii_alphabetic() {
            i = start + 1;
            continue;
        }
        let name = text[name_at..name_at + name_len].to_string();
        let Some(end) = tag_end(text, name_at + name_len) else {
            break;
        };
        if closing {
            if let Some(at) = open
                .iter()
                .rposition(|(n, _)| n.eq_ignore_ascii_case(&name))
            {
                // Elements left open inside this one never closed; they
                // are dropped, not matched with a later stray tag.
                open.truncate(at + 1);
                let (name, range) = open.pop().expect("found above");
                out.push(Element {
                    name,
                    open: range,
                    close: Some(start..end),
                });
            }
            i = end;
            continue;
        }
        let self_closing = text[..end - 1].ends_with('/')
            || VOID_ELEMENTS.contains(&name.to_ascii_lowercase().as_str());
        if self_closing {
            out.push(Element {
                name,
                open: start..end,
                close: None,
            });
            i = end;
            continue;
        }
        let raw = ["script", "style"]
            .iter()
            .any(|r| name.eq_ignore_ascii_case(r));
        open.push((name.clone(), start..end));
        i = end;
        if raw {
            let needle = format!("</{}", name.to_ascii_lowercase());
            i = text[end..]
                .to_ascii_lowercase()
                .find(&needle)
                .map_or(text.len(), |e| end + e);
        }
    }
    out.sort_by_key(|e| e.open.start);
    out
}

/// The byte after the `>` that ends the tag whose name ends at `from`,
/// stepping over quoted values and `{...}` expressions.
fn tag_end(text: &str, from: usize) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut braces = 0usize;
    for (off, c) in text[from..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') if braces == 0 => quote = Some(c),
            (None, '{') => braces += 1,
            (None, '}') => braces = braces.saturating_sub(1),
            (None, '>') if braces == 0 => return Some(from + off + 1),
            (None, '<') if braces == 0 => return None,
            _ => {}
        }
    }
    None
}

/// The innermost element of `text` whose span holds byte `at`.
pub fn enclosing(text: &str, at: usize) -> Option<Element> {
    elements(text)
        .into_iter()
        .filter(|e| e.outer().contains(&at))
        .min_by_key(|e| e.outer().len())
}

/// Go to Matching Pair (#1557): from inside an opening tag, the start of
/// its closing tag, and back. `None` off a tag, or on one that closes
/// itself.
pub fn matching_tag(text: &str, at: usize) -> Option<usize> {
    elements(text).into_iter().find_map(|e| {
        let close = e.close?;
        if e.open.contains(&at) {
            Some(close.start)
        } else if close.contains(&at) {
            Some(e.open.start)
        } else {
            None
        }
    })
}

/// `range` without the whitespace at either end.
fn trimmed(text: &str, range: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let s = &text[range.clone()];
    let start = range.start + (s.len() - s.trim_start().len());
    let end = range.end - (s.len() - s.trim_end().len());
    start..end.max(start)
}

/// Balance outward (#1557): the smallest of the enclosing elements'
/// contents and whole spans that holds `sel` and is larger than it, so
/// repeating it grows the selection one step: the element's content, then
/// the element, then its parent's content.
pub fn balance_out(text: &str, sel: std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
    elements(text)
        .into_iter()
        .flat_map(|e| {
            let inner = trimmed(text, e.inner());
            [inner, e.outer()]
        })
        .filter(|r| r.start <= sel.start && sel.end <= r.end && *r != sel)
        .min_by_key(|r| r.len())
}

/// Balance inward (#1557): back down a step: from a whole element to its
/// content, and from content to the first element inside it.
pub fn balance_in(text: &str, sel: std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
    let all = elements(text);
    if let Some(e) = all.iter().find(|e| e.outer() == sel && e.close.is_some()) {
        let inner = trimmed(text, e.inner());
        if !inner.is_empty() {
            return Some(inner);
        }
    }
    all.iter()
        .map(Element::outer)
        .filter(|r| sel.start <= r.start && r.end <= sel.end && *r != sel)
        .min_by_key(|r| r.start)
}

/// The byte range of `text`'s line holding `at`, with its line break.
fn line_span(text: &str, at: usize) -> std::ops::Range<usize> {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let end = text[at..].find('\n').map_or(text.len(), |i| at + i + 1);
    start..end
}

/// Whether `range` is all its line holds but whitespace.
fn alone_on_line(text: &str, range: &std::ops::Range<usize>) -> bool {
    let line = line_span(text, range.start);
    line.end >= range.end
        && text[line.start..range.start].trim().is_empty()
        && text[range.end..line.end].trim().is_empty()
}

/// `line` one `indent` shallower: that unit, or as much leading whitespace
/// as it has up to its width.
fn dedent_line(line: &str, indent: &str) -> String {
    if let Some(rest) = line.strip_prefix(indent) {
        return rest.to_string();
    }
    let width = indent.chars().count().max(1);
    let lead = line
        .chars()
        .take(width)
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    line.chars().skip(lead).collect()
}

/// Remove Tag (#1557): `text` without the tags of the element around byte
/// `at`, and where its opening tag was. A tag alone on its line takes its
/// line with it, and the content between two such tags moves out one
/// `indent` level, as VS Code leaves it.
pub fn remove_tag(text: &str, at: usize, indent: &str) -> Option<(String, usize)> {
    let e = enclosing(text, at)?;
    let Some(close) = e.close.clone() else {
        let span = if alone_on_line(text, &e.open) {
            line_span(text, e.open.start)
        } else {
            e.open.clone()
        };
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..span.start]);
        out.push_str(&text[span.end..]);
        return Some((out, span.start));
    };
    let block = alone_on_line(text, &e.open) && alone_on_line(text, &close);
    let mut out = String::with_capacity(text.len());
    if block {
        let first = line_span(text, e.open.start);
        let last = line_span(text, close.start);
        out.push_str(&text[..first.start]);
        for line in text[first.end..last.start].split_inclusive('\n') {
            out.push_str(&dedent_line(line, indent));
        }
        out.push_str(&text[last.end..]);
        // The caret on the first line that moved out, at its text.
        let moved = dedent_line(
            text[first.end..last.start].split('\n').next().unwrap_or(""),
            indent,
        );
        Some((out, first.start + (moved.len() - moved.trim_start().len())))
    } else {
        out.push_str(&text[..e.open.start]);
        out.push_str(&text[e.open.end..close.start]);
        out.push_str(&text[close.end..]);
        Some((out, e.open.start))
    }
}

/// Update Tag (#1557): `text` with the element around byte `at` renamed
/// to `name` in both its tags. `None` when `name` is not a tag name.
pub fn rename_tag(text: &str, at: usize, name: &str) -> Option<String> {
    let name = name.trim();
    let valid = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'));
    if !valid {
        return None;
    }
    let e = enclosing(text, at)?;
    let old = e.name.len();
    let mut out = text.to_string();
    // The closing tag first: it sits later, so the opening tag's offsets
    // still hold after it changes.
    if let Some(close) = &e.close {
        out.replace_range(close.start + 2..close.start + 2 + old, name);
    }
    out.replace_range(e.open.start + 1..e.open.start + 1 + old, name);
    Some(out)
}

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

        if let Some(lines) = &node.wrapped {
            self.out.push('\n');
            let inner = self.indent.repeat(depth + 1);
            for line in lines {
                if !line.is_empty() {
                    self.out.push_str(&inner);
                    self.out.push_str(line);
                }
                self.out.push('\n');
            }
            self.out.push_str(&pad);
        } else if !node.children.is_empty() {
            self.out.push('\n');
            self.render_all(&node.children, Some(&name), depth + 1, index)?;
            self.out.push_str(&pad);
        } else {
            let own = node
                .per_line
                .as_ref()
                .and_then(|lines| lines.get(index.saturating_sub(1)))
                .cloned();
            let text = own.or_else(|| node.text.as_deref().map(|t| number(t, index)));
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

    const PAGE: &str = "<ul>\n  <li>Home</li>\n  <li>About</li>\n</ul>\n<p>Some text</p>\n";

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    /// #1557: the issue's example: the `<ul>` wrapped in `nav.menu`, one
    /// level in.
    #[test]
    fn wrap_puts_the_lines_one_level_inside_the_new_element() {
        let ul = lines("<ul>\n  <li>Home</li>\n  <li>About</li>\n</ul>");
        assert_eq!(
            wrap("nav.menu", WrapContent::Lines(&ul), "  ", Profile::Html).unwrap(),
            "<nav class=\"menu\">\n  <ul>\n    <li>Home</li>\n    <li>About</li>\n  </ul>\n</nav>\n"
        );
    }

    /// #1557: the content goes in the innermost last element, and a bare
    /// `*` repeats that element once per line.
    #[test]
    fn wrap_nests_in_the_innermost_element_and_repeats_per_line() {
        let three = lines("Home\nAbout\nContact");
        assert_eq!(
            wrap("ul>li*", WrapContent::Lines(&three), "  ", Profile::Html).unwrap(),
            "<ul>\n  <li>Home</li>\n  <li>About</li>\n  <li>Contact</li>\n</ul>\n"
        );
        let one = lines("<p>hi</p>");
        assert_eq!(
            wrap("div>section", WrapContent::Lines(&one), "\t", Profile::Html).unwrap(),
            "<div>\n\t<section>\n\t\t<p>hi</p>\n\t</section>\n</div>\n"
        );
        assert_eq!(
            wrap("b", WrapContent::Inline("Home"), "  ", Profile::Html).unwrap(),
            "<b>Home</b>\n",
            "part of a line stays on its line; a lone word is a tag here"
        );
    }

    /// #1557 negative: nothing to wrap with, or an element that can hold
    /// nothing, wraps nothing.
    #[test]
    fn wrap_refuses_what_is_not_an_abbreviation_or_holds_nothing() {
        let one = lines("x");
        assert_eq!(
            wrap("", WrapContent::Lines(&one), "  ", Profile::Html),
            None
        );
        assert_eq!(
            wrap("div>", WrapContent::Lines(&one), "  ", Profile::Html),
            None
        );
        assert_eq!(
            wrap("img", WrapContent::Lines(&one), "  ", Profile::Html),
            None
        );
    }

    /// #1557: tags pair up past comments, quoted `>`, void elements and a
    /// script body.
    #[test]
    fn elements_pair_tags_and_skip_what_is_not_markup() {
        let text =
            "<div a=\"x>y\"><!-- <p> --><br><img src=x/><script>if (a < b) {}</script></div>";
        let all = elements(text);
        let names: Vec<&str> = all.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["div", "br", "img", "script"]);
        assert_eq!(all[0].outer(), 0..text.len());
        assert_eq!(all[1].close, None);
        assert!(all[3].close.is_some(), "the script closes, its `<` aside");
    }

    /// #1557: Go to Matching Pair hops between an element's two tags.
    #[test]
    fn matching_tag_hops_between_the_two_tags() {
        let close = PAGE.find("</ul>").unwrap();
        assert_eq!(matching_tag(PAGE, 1), Some(close));
        assert_eq!(matching_tag(PAGE, close + 2), Some(0));
        let li = PAGE.find("<li>").unwrap();
        assert_eq!(matching_tag(PAGE, li + 1), PAGE.find("</li>"));
    }

    /// #1557 negative: off a tag, or on one that closes itself, there is no
    /// pair to go to.
    #[test]
    fn matching_tag_needs_a_tag_with_a_pair() {
        let home = PAGE.find("Home").unwrap();
        assert_eq!(matching_tag(PAGE, home), None);
        assert_eq!(matching_tag("<br>", 1), None);
    }

    /// #1557: Remove Tag takes both tags and their lines, and moves the
    /// content out one level.
    #[test]
    fn remove_tag_drops_both_tags_and_dedents_the_content() {
        let in_ul = PAGE.find("Home").unwrap() - 6;
        let (out, caret) = remove_tag(PAGE, in_ul, "  ").unwrap();
        assert_eq!(out, "<li>Home</li>\n<li>About</li>\n<p>Some text</p>\n");
        assert_eq!(caret, 0);
        let p = PAGE.find("Some").unwrap();
        let (out, caret) = remove_tag(PAGE, p, "  ").unwrap();
        assert!(out.ends_with("</ul>\nSome text\n"), "{out}");
        assert_eq!(&out[caret..caret + 4], "Some");
    }

    /// #1557: Update Tag renames both tags and nothing else.
    #[test]
    fn rename_tag_changes_both_tags() {
        let out = rename_tag(PAGE, 1, "ol").unwrap();
        assert_eq!(out, PAGE.replace("<ul>", "<ol>").replace("</ul>", "</ol>"));
        let at = PAGE.find("About").unwrap();
        let out = rename_tag(PAGE, at, "dt").unwrap();
        assert!(
            out.contains("<dt>About</dt>") && out.contains("<li>Home</li>"),
            "{out}"
        );
    }

    /// #1557 negative: no tag around the caret, or a name that is no tag
    /// name, changes nothing.
    #[test]
    fn rename_tag_refuses_a_bad_name_or_no_element() {
        assert_eq!(rename_tag(PAGE, 1, "1x"), None);
        assert_eq!(rename_tag(PAGE, 1, "a b"), None);
        assert_eq!(rename_tag("just text", 2, "div"), None);
        assert_eq!(remove_tag("just text", 2, "  "), None);
    }

    /// #1557: Balance outward grows the selection a step at a time, and
    /// inward walks it back.
    #[test]
    fn balance_grows_and_shrinks_one_step_at_a_time() {
        let home = PAGE.find("Home").unwrap();
        let li = PAGE.find("<li>").unwrap();
        let li_end = PAGE.find("</li>").unwrap() + 5;
        let step1 = balance_out(PAGE, home + 1..home + 1).unwrap();
        assert_eq!(&PAGE[step1.clone()], "Home");
        let step2 = balance_out(PAGE, step1.clone()).unwrap();
        assert_eq!(step2, li..li_end);
        let step3 = balance_out(PAGE, step2.clone()).unwrap();
        assert_eq!(&PAGE[step3.clone()], "<li>Home</li>\n  <li>About</li>");
        let ul = balance_out(PAGE, step3.clone()).unwrap();
        assert_eq!(&PAGE[ul.clone()], &PAGE[..PAGE.find("</ul>").unwrap() + 5]);
        assert_eq!(balance_in(PAGE, ul), Some(step3.clone()));
        assert_eq!(balance_in(PAGE, step3), Some(step2.clone()));
        assert_eq!(balance_in(PAGE, step2), Some(step1));
    }
}
