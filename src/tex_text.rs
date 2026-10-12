//! TeX math as plain text (#1564): the Markdown preview shows `$…$` and
//! `$$…$$` with the symbols a reader expects, `π`, `∑`, `≤`, `x²`, rather
//! than the TeX that spells them. A terminal can't typeset, so the aim is
//! readable text: Greek letters and operators become their Unicode
//! characters, scripts become super- and subscript characters where every
//! character has one (`^(…)` where not), and `\frac{a}{b}` becomes `a/b`.
//! A command it doesn't know stays as TeX, with its braces, so nothing in
//! the source is lost.

/// `tex` as text. A `\\` line break becomes `\n`; other whitespace runs
/// become one space, as TeX itself treats them, and empty rows go.
pub fn to_unicode(tex: &str) -> String {
    let chars: Vec<char> = tex.chars().collect();
    let mut p = Converter { chars, pos: 0 };
    let out = p.until(None);
    out.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

struct Converter {
    chars: Vec<char>,
    pos: usize,
}

impl Converter {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    /// Convert up to the `close` character (consumed) or the end.
    fn until(&mut self, close: Option<char>) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if Some(c) == close {
                self.pos += 1;
                return out;
            }
            match c {
                '^' | '_' => {
                    self.pos += 1;
                    let arg = self.argument();
                    out.push_str(&script(&arg, c == '^'));
                }
                _ => out.push_str(&self.atom()),
            }
        }
        out
    }

    /// One unit: a `{group}`, a command with its arguments, or a character.
    fn atom(&mut self) -> String {
        let Some(c) = self.peek() else {
            return String::new();
        };
        self.pos += 1;
        match c {
            '{' => self.until(Some('}')),
            '\\' => self.command(),
            '&' => String::from(" "),
            // Only `\\` breaks a line; a newline in the source is a space.
            '~' | '\n' | '\r' | '\t' => String::from(" "),
            '\'' => String::from("′"),
            c => c.to_string(),
        }
    }

    /// A script's or a command's argument: a group, or the next unit alone
    /// (`x^2`, `\frac12`). Spaces before it don't count.
    fn argument(&mut self) -> String {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
        self.atom()
    }

    /// A `{group}` argument kept as TeX, for a command left unconverted.
    fn raw_groups(&mut self) -> String {
        let mut out = String::new();
        while self.peek() == Some('{') {
            self.pos += 1;
            out.push('{');
            out.push_str(&self.until(Some('}')));
            out.push('}');
        }
        out
    }

    /// After a `\`: the command's name and what it becomes.
    fn command(&mut self) -> String {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        if self.pos == start {
            // A one-character control symbol: `\,`, `\{`, `\\`, ...
            let Some(c) = self.peek() else {
                return String::from("\\");
            };
            self.pos += 1;
            return match c {
                '\\' => String::from("\n"),
                ',' | ';' | ':' | ' ' => String::from(" "),
                '!' => String::new(),
                c => c.to_string(),
            };
        }
        let name: String = self.chars[start..self.pos].iter().collect();
        if let Some(sym) = symbol(&name) {
            return sym.to_string();
        }
        match name.as_str() {
            "frac" | "dfrac" | "tfrac" => {
                let num = self.argument();
                let den = self.argument();
                format!("{}/{}", grouped(&num), grouped(&den))
            }
            "sqrt" => {
                let arg = self.argument();
                format!("√{}", grouped(&arg))
            }
            "text" | "textrm" | "textit" | "textbf" | "mathrm" | "mathit" | "mathbf" | "mathsf"
            | "mathtt" | "mathcal" | "operatorname" | "boldsymbol" => self.argument(),
            "mathbb" => self.argument().chars().map(double_struck).collect(),
            "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl"
            | "Bigr" | "displaystyle" | "textstyle" | "limits" | "nolimits" => String::new(),
            "begin" | "end" => {
                // The environment's name; `aligned` or `cases` rows break
                // on `\\` and their `&` columns become spaces.
                self.argument();
                String::new()
            }
            "quad" => String::from("  "),
            "qquad" => String::from("    "),
            "sin" | "cos" | "tan" | "cot" | "sec" | "csc" | "log" | "ln" | "exp" | "lim"
            | "max" | "min" | "det" | "sup" | "inf" | "arg" | "deg" | "gcd" | "dim" | "ker"
            | "Pr" | "sinh" | "cosh" | "tanh" | "arcsin" | "arccos" | "arctan" => name,
            _ => format!("\\{name}{}", self.raw_groups()),
        }
    }
}

/// `text` as a fraction's numerator or denominator: bare when it is one
/// term, in parentheses when an operator or space would make `a/b`
/// ambiguous.
fn grouped(text: &str) -> String {
    let mut depth = 0i32;
    let compound = text.chars().any(|c| {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
        depth == 0 && matches!(c, ' ' | '+' | '-' | '−' | '=' | '/' | ',' | '<' | '>' | '±')
    });
    if compound || text.is_empty() {
        format!("({text})")
    } else {
        text.to_string()
    }
}

/// A script as super- or subscript characters when each has one, else
/// `^x`, `^(…)`.
fn script(arg: &str, sup: bool) -> String {
    let mapped: Option<String> = arg
        .chars()
        .map(|c| if sup { superscript(c) } else { subscript(c) })
        .collect();
    match mapped {
        Some(s) if !s.is_empty() => s,
        _ => {
            let mark = if sup { '^' } else { '_' };
            if arg.chars().count() == 1 {
                format!("{mark}{arg}")
            } else {
                format!("{mark}({arg})")
            }
        }
    }
}

fn superscript(c: char) -> Option<char> {
    const FROM: &str = "0123456789+-−=()abcdefghijklmnoprstuvwxyzABDEGHIJKLMNOPRTUVWβγδθιφχ′";
    const TO: &str = "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁻⁼⁽⁾ᵃᵇᶜᵈᵉᶠᵍʰⁱʲᵏˡᵐⁿᵒᵖʳˢᵗᵘᵛʷˣʸᶻᴬᴮᴰᴱᴳᴴᴵᴶᴷᴸᴹᴺᴼᴾᴿᵀᵁⱽᵂᵝᵞᵟᶿᶥᵠᵡ′";
    FROM.chars()
        .position(|f| f == c)
        .and_then(|i| TO.chars().nth(i))
}

fn subscript(c: char) -> Option<char> {
    const FROM: &str = "0123456789+-−=()aehijklmnoprstuvxβγρφχ";
    const TO: &str = "₀₁₂₃₄₅₆₇₈₉₊₋₋₌₍₎ₐₑₕᵢⱼₖₗₘₙₒₚᵣₛₜᵤᵥₓᵦᵧᵨᵩᵪ";
    FROM.chars()
        .position(|f| f == c)
        .and_then(|i| TO.chars().nth(i))
}

fn double_struck(c: char) -> char {
    match c {
        'R' => 'ℝ',
        'N' => 'ℕ',
        'Z' => 'ℤ',
        'Q' => 'ℚ',
        'C' => 'ℂ',
        'P' => 'ℙ',
        'E' => '𝔼',
        c => c,
    }
}

/// The character a symbol command stands for.
fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" => "ϵ",
        "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "vartheta" => "ϑ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "varpi" => "ϖ",
        "rho" => "ρ",
        "varrho" => "ϱ",
        "sigma" => "σ",
        "varsigma" => "ς",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" => "ϕ",
        "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Upsilon" => "Υ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        "sum" => "∑",
        "prod" => "∏",
        "coprod" => "∐",
        "int" => "∫",
        "iint" => "∬",
        "iiint" => "∭",
        "oint" => "∮",
        "partial" => "∂",
        "nabla" => "∇",
        "infty" => "∞",
        "le" | "leq" => "≤",
        "ge" | "geq" => "≥",
        "ne" | "neq" => "≠",
        "ll" => "≪",
        "gg" => "≫",
        "approx" => "≈",
        "equiv" => "≡",
        "cong" => "≅",
        "sim" => "∼",
        "simeq" => "≃",
        "propto" => "∝",
        "pm" => "±",
        "mp" => "∓",
        "times" => "×",
        "div" => "÷",
        "cdot" => "·",
        "cdots" => "⋯",
        "ldots" | "dots" => "…",
        "vdots" => "⋮",
        "ddots" => "⋱",
        "to" | "rightarrow" => "→",
        "leftarrow" | "gets" => "←",
        "leftrightarrow" => "↔",
        "Rightarrow" | "implies" => "⇒",
        "Leftarrow" => "⇐",
        "Leftrightarrow" | "iff" => "⇔",
        "mapsto" => "↦",
        "uparrow" => "↑",
        "downarrow" => "↓",
        "in" => "∈",
        "notin" => "∉",
        "ni" => "∋",
        "subset" => "⊂",
        "subseteq" => "⊆",
        "supset" => "⊃",
        "supseteq" => "⊇",
        "cup" => "∪",
        "cap" => "∩",
        "setminus" => "∖",
        "emptyset" | "varnothing" => "∅",
        "forall" => "∀",
        "exists" => "∃",
        "nexists" => "∄",
        "neg" | "lnot" => "¬",
        "land" | "wedge" => "∧",
        "lor" | "vee" => "∨",
        "oplus" => "⊕",
        "otimes" => "⊗",
        "circ" => "∘",
        "bullet" => "∙",
        "star" => "⋆",
        "ast" => "∗",
        "prime" => "′",
        "ell" => "ℓ",
        "hbar" => "ℏ",
        "Re" => "ℜ",
        "Im" => "ℑ",
        "aleph" => "ℵ",
        "angle" => "∠",
        "perp" => "⊥",
        "parallel" => "∥",
        "mid" => "∣",
        "langle" => "⟨",
        "rangle" => "⟩",
        "lfloor" => "⌊",
        "rfloor" => "⌋",
        "lceil" => "⌈",
        "rceil" => "⌉",
        "lbrace" => "{",
        "rbrace" => "}",
        "vert" => "|",
        "Vert" => "‖",
        "degree" => "°",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_and_scripts() {
        assert_eq!(to_unicode(r"e^{i\pi} + 1 = 0"), "e^(iπ) + 1 = 0");
        assert_eq!(to_unicode("x^2 + y_1"), "x² + y₁");
        assert_eq!(to_unicode(r"a \le b \to c \cdot \infty"), "a ≤ b → c · ∞");
        assert_eq!(to_unicode(r"\alpha\beta\Omega"), "αβΩ");
    }

    #[test]
    fn a_sum_and_a_fraction() {
        assert_eq!(
            to_unicode(r"\sum_{k=1}^{n} k = \frac{n(n+1)}{2}"),
            "∑ₖ₌₁ⁿ k = n(n+1)/2"
        );
        assert_eq!(to_unicode(r"\frac{a+b}{c}"), "(a+b)/c");
        assert_eq!(to_unicode(r"\frac12"), "1/2");
        assert_eq!(to_unicode(r"\sqrt{x^2+1}"), "√(x²+1)");
    }

    #[test]
    fn text_fonts_and_sizes() {
        assert_eq!(to_unicode(r"x \in \mathbb{R}"), "x ∈ ℝ");
        assert_eq!(to_unicode(r"\text{if } x > 0"), "if x > 0");
        assert_eq!(to_unicode(r"\left( \frac{x}{2} \right)"), "( x/2 )");
        assert_eq!(to_unicode(r"\sin x + \log_2 n"), "sin x + log₂ n");
    }

    #[test]
    fn rows_break_on_double_backslash() {
        let tex = "\\begin{aligned}\n a &= b + c \\\\\n &= d\n\\end{aligned}";
        assert_eq!(to_unicode(tex), "a = b + c\n= d");
    }

    #[test]
    fn what_it_does_not_know_stays_as_tex() {
        assert_eq!(to_unicode(r"\binom{n}{k}"), r"\binom{n}{k}");
        assert_eq!(to_unicode(r"\overline{z}"), r"\overline{z}");
        assert_eq!(to_unicode("q^q"), "q^q", "no superscript q");
        assert_eq!(to_unicode("a_{xyz}"), "a_(xyz)", "no subscript y or z");
        assert_eq!(to_unicode(r"\{1, 2\}"), "{1, 2}");
    }

    #[test]
    fn unbalanced_input_does_not_panic() {
        for tex in ["{", "}", "x^", r"\", r"\frac{1}", "a_{b", "\\begin"] {
            to_unicode(tex);
        }
    }
}
