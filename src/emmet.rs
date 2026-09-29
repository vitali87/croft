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
//! * CSS abbreviations (`m10-20`), snippets (`!`, `link:css`) and lorem
//!   ipsum. They are separate features that happen to share a keystroke.
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
}

/// The expansion of `abbr`, indented one level per depth with `indent`, or
/// `None` when `abbr` is not an abbreviation.
///
/// The returned offset is where the caret belongs: the first empty element's
/// content position, which is where typing continues. It is a byte offset
/// into the returned string.
pub fn expand(abbr: &str, indent: &str, profile: Profile) -> Option<(String, usize)> {
    let nodes = parse(abbr)?;
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
        if !name.contains('-') && !KNOWN_TAGS.contains(&name) {
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
                _ => break,
            }
        }
        if node.is_bare() {
            return None;
        }
        Some(node)
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
            "<li>\n  <a></a>\n</li>\n<li>\n  <a></a>\n</li>\n"
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
        assert_eq!(out, "<a title=\"></\"></a>\n");
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
}
