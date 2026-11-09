//! Conservative, bounded Unicode math. Unsupported input returns to the source view.
use saya_core::text::is_terminal_control;
use unicode_width::UnicodeWidthStr;

const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_DEPTH: usize = 24;
const MAX_MATRIX_DIMENSION: usize = 16;

/// Convert the supported mathematical subset to a single Unicode line.
pub(super) fn render(source: &str) -> Option<String> {
    convert(source, false)
}

/// Display mode also permits standalone matrices, aligned rows and boxed expressions.
pub(super) fn render_block(source: &str) -> Option<String> {
    convert(source, true)
}

fn convert(source: &str, block: bool) -> Option<String> {
    if source.len() > MAX_INPUT_BYTES
        || source
            .chars()
            .any(|character| is_terminal_control(character) && !matches!(character, '\n' | '\t'))
    {
        return None;
    }
    let mut parser = Parser {
        characters: source.chars().collect(),
        position: 0,
        remaining: source.len().saturating_mul(4).saturating_add(1),
        depth: 0,
        block,
        in_font: false,
    };
    let output = parser.sequence(Stop::End)?;
    let output = output.trim();
    (parser.position == parser.characters.len() && !output.is_empty()).then(|| output.to_owned())
}

#[derive(Clone, Copy, PartialEq)]
enum Stop {
    End,
    Brace,
    Bracket,
    Right,
    Cell,
}

struct Atom {
    text: String,
    grouped: bool,
}

struct Parser {
    characters: Vec<char>,
    position: usize,
    remaining: usize,
    depth: usize,
    block: bool,
    in_font: bool,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.characters.get(self.position).copied()
    }

    fn advance(&mut self) -> Option<char> {
        if self.remaining == 0 {
            return None;
        }
        let character = self.peek()?;
        self.position += 1;
        self.remaining -= 1;
        Some(character)
    }

    fn spaces(&mut self) -> Option<()> {
        while self.peek().is_some_and(char::is_whitespace) {
            self.advance()?;
        }
        Some(())
    }

    fn command_at(&self, name: &str) -> bool {
        let remaining = &self.characters[self.position..];
        remaining.first() == Some(&'\\')
            && remaining[1..]
                .iter()
                .copied()
                .take(name.len())
                .eq(name.chars())
            && !remaining
                .get(name.len() + 1)
                .is_some_and(char::is_ascii_alphabetic)
    }

    fn stopped(&self, stop: Stop) -> bool {
        match stop {
            Stop::End => false,
            Stop::Brace => self.peek() == Some('}'),
            Stop::Bracket => self.peek() == Some(']'),
            Stop::Right => self.command_at("right"),
            Stop::Cell => {
                self.peek() == Some('&')
                    || self.characters[self.position..].starts_with(&['\\', '\\'])
                    || self.command_at("end")
            }
        }
    }

    fn sequence(&mut self, stop: Stop) -> Option<String> {
        let mut output = String::new();
        while let Some(character) = self.peek() {
            if self.stopped(stop) {
                break;
            }
            if character.is_whitespace() {
                self.spaces()?;
                if !output.is_empty() && !output.ends_with(' ') {
                    append(&mut output, " ")?;
                }
                continue;
            }
            let atom = self.scripted_atom()?;
            // Mixing a 2-D object with neighboring text needs a layout engine.
            if (atom.contains('\n') && !output.trim().is_empty())
                || (output.contains('\n') && !atom.trim().is_empty())
            {
                return None;
            }
            append(&mut output, &atom)?;
        }
        Some(output)
    }

    fn scripted_atom(&mut self) -> Option<String> {
        let atom = self.atom()?;
        let mut superscript = None;
        let mut subscript = None;
        loop {
            let before_spaces = self.position;
            self.spaces()?;
            let Some(marker @ ('^' | '_')) = self.peek() else {
                self.position = before_spaces;
                break;
            };
            self.advance()?;
            let value = self.argument()?;
            let destination = if marker == '^' {
                &mut superscript
            } else {
                &mut subscript
            };
            if destination.is_some() {
                return None;
            }
            *destination = Some(script(&value, marker == '^')?);
        }
        if superscript.is_none() && subscript.is_none() {
            return Some(atom.text);
        }
        if atom.text.is_empty() || atom.text.contains('\n') {
            return None;
        }
        let mut output = if atom.grouped && atom.text.chars().count() > 1 {
            format!("({})", atom.text)
        } else {
            atom.text
        };
        append(&mut output, subscript.as_deref().unwrap_or_default())?;
        append(&mut output, superscript.as_deref().unwrap_or_default())?;
        Some(output)
    }

    fn argument(&mut self) -> Option<String> {
        self.spaces()?;
        let text = self.atom()?.text;
        (!text.is_empty()).then_some(text)
    }

    fn group(&mut self, closing: char, stop: Stop) -> Option<String> {
        let output = self.sequence(stop)?;
        (self.advance()? == closing).then_some(output)
    }

    fn atom(&mut self) -> Option<Atom> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let result = match self.advance() {
            Some('{') => self.group('}', Stop::Brace).map(|text| Atom {
                text,
                grouped: true,
            }),
            Some('\\') => self.command(),
            Some('}' | '^' | '_' | '&' | '%' | '$' | '#') | None => None,
            Some(character) => Some(Atom {
                text: character.to_string(),
                grouped: false,
            }),
        };
        self.depth -= 1;
        result
    }

    fn command_name(&mut self) -> Option<String> {
        let first = self.advance()?;
        let mut name = first.to_string();
        if first.is_ascii_alphabetic() {
            while self
                .peek()
                .is_some_and(|character| character.is_ascii_alphabetic())
            {
                name.push(self.advance()?);
            }
        }
        Some(name)
    }

    fn command(&mut self) -> Option<Atom> {
        let name = self.command_name()?;
        let (text, grouped) = match name.as_str() {
            "frac" | "dfrac" | "tfrac" => {
                let numerator = self.argument()?;
                let denominator = self.argument()?;
                if numerator.contains('\n') || denominator.contains('\n') {
                    return None;
                }
                (format!("({})", fraction(&numerator, &denominator)), false)
            }
            "sqrt" => {
                self.spaces()?;
                let index = if self.peek() == Some('[') {
                    self.advance()?;
                    self.group(']', Stop::Bracket)?
                } else {
                    "2".into()
                };
                let root = match index.as_str() {
                    "2" => "√".into(),
                    "3" => "∛".into(),
                    "4" => "∜".into(),
                    _ => format!("{}√", script(&index, true)?),
                };
                let radicand = self.argument()?;
                if radicand.contains('\n') {
                    return None;
                }
                (format!("{root}({radicand})"), false)
            }
            "text" | "operatorname" if !self.in_font => (self.text_argument()?, true),
            "mathrm" | "mathnormal" => (self.font_argument()?, true),
            "mathbf" => (bold(&self.font_argument()?)?, true),
            "mathbb" => (blackboard(&self.font_argument()?)?, true),
            "left" => {
                let left = self.delimiter()?;
                let body = self.sequence(Stop::Right)?;
                if body.contains('\n') {
                    return None;
                }
                if self.advance()? != '\\' || self.command_name()? != "right" {
                    return None;
                }
                let right = self.delimiter()?;
                (format!("{left}{}{right}", body.trim()), false)
            }
            "begin" => {
                let environment = self.environment_name()?;
                (self.matrix(&environment)?, true)
            }
            "hat" | "bar" | "overline" | "vec" | "dot" | "ddot" | "tilde" => {
                let base = self.argument()?;
                if base.chars().count() != 1 {
                    return None;
                }
                let accent = match name.as_str() {
                    "hat" => '\u{302}',
                    "bar" | "overline" => '\u{305}',
                    "vec" => '\u{20d7}',
                    "dot" => '\u{307}',
                    "ddot" => '\u{308}',
                    _ => '\u{303}',
                };
                (format!("{base}{accent}"), false)
            }
            "boxed" if self.block => {
                let content = self.argument()?;
                (boxed(&content)?, true)
            }
            "displaystyle" | "textstyle" => (String::new(), false),
            _ => (symbol(&name)?.into(), false),
        };
        (text.len() <= MAX_OUTPUT_BYTES).then_some(Atom { text, grouped })
    }

    fn font_argument(&mut self) -> Option<String> {
        if self.in_font {
            return None;
        }
        self.in_font = true;
        let result = self.argument();
        self.in_font = false;
        result
    }

    fn text_argument(&mut self) -> Option<String> {
        self.spaces()?;
        if self.advance()? != '{' {
            return None;
        }
        let mut output = String::new();
        while let Some(character) = self.advance() {
            match character {
                '}' => return Some(output),
                '{' => return None,
                '\\' => {
                    let escaped = self.advance()?;
                    if !matches!(
                        escaped,
                        '{' | '}' | '$' | '%' | '&' | '#' | '_' | '\\' | ' '
                    ) {
                        return None;
                    }
                    output.push(escaped);
                }
                _ => output.push(character),
            }
        }
        None
    }

    fn delimiter(&mut self) -> Option<String> {
        self.spaces()?;
        match self.advance()? {
            '.' => Some(String::new()),
            character @ ('(' | ')' | '[' | ']' | '{' | '}' | '|') => Some(character.to_string()),
            '\\' => match self.command_name()?.as_str() {
                "{" | "lbrace" => Some("{".into()),
                "}" | "rbrace" => Some("}".into()),
                "langle" => Some("⟨".into()),
                "rangle" => Some("⟩".into()),
                "|" | "Vert" => Some("‖".into()),
                "vert" => Some("|".into()),
                _ => None,
            },
            _ => None,
        }
    }

    fn environment_name(&mut self) -> Option<String> {
        self.spaces()?;
        if self.advance()? != '{' {
            return None;
        }
        let mut name = String::new();
        while self
            .peek()
            .is_some_and(|character| character.is_ascii_alphabetic())
        {
            name.push(self.advance()?);
        }
        (self.advance()? == '}').then_some(name)
    }

    fn matrix(&mut self, environment: &str) -> Option<String> {
        if !matches!(
            environment,
            "matrix"
                | "pmatrix"
                | "bmatrix"
                | "Bmatrix"
                | "vmatrix"
                | "Vmatrix"
                | "aligned"
                | "cases"
        ) {
            return None;
        }
        let mut rows = Vec::new();
        let mut row = Vec::new();
        loop {
            if rows.len() >= MAX_MATRIX_DIMENSION || row.len() >= MAX_MATRIX_DIMENSION {
                return None;
            }
            let cell = self.sequence(Stop::Cell)?.trim().to_owned();
            if cell.contains('\n') {
                return None;
            }
            if self.command_at("end") {
                if !cell.is_empty() || !row.is_empty() {
                    row.push(cell);
                    rows.push(row);
                }
                self.advance()?;
                if self.command_name()? != "end" || self.environment_name()? != environment {
                    return None;
                }
                break;
            }
            row.push(cell);
            match self.advance()? {
                '&' => {}
                '\\' if self.advance()? == '\\' => {
                    rows.push(row);
                    row = Vec::new();
                    self.spaces()?;
                    if self.peek() == Some('[') {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        let columns = rows.first()?.len();
        if rows.iter().any(|row| row.len() != columns) {
            return None;
        }
        layout_matrix(&rows, environment, self.block)
    }
}

fn append(output: &mut String, value: &str) -> Option<()> {
    if output.len().saturating_add(value.len()) > MAX_OUTPUT_BYTES {
        return None;
    }
    output.push_str(value);
    Some(())
}

fn operand(value: &str) -> String {
    if value.chars().count() == 1 {
        value.to_owned()
    } else {
        format!("({value})")
    }
}

fn fraction(numerator: &str, denominator: &str) -> String {
    match (numerator, denominator) {
        ("1", "2") => "½".into(),
        ("1", "3") => "⅓".into(),
        ("2", "3") => "⅔".into(),
        ("1", "4") => "¼".into(),
        ("3", "4") => "¾".into(),
        _ => format!("{}/{}", operand(numerator), operand(denominator)),
    }
}

fn script(value: &str, upper: bool) -> Option<String> {
    let source = if upper {
        "0123456789+-=()abcdefghijklmnoprstuvwxyzABDEGHIJKLMNOPRTUVW∘′"
    } else {
        "0123456789+-=()aehijklmnoprstuvxβγρφχ"
    };
    let replacement = if upper {
        "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ᵃᵇᶜᵈᵉᶠᵍʰⁱʲᵏˡᵐⁿᵒᵖʳˢᵗᵘᵛʷˣʸᶻᴬᴮᴰᴱᴳᴴᴵᴶᴷᴸᴹᴺᴼᴾᴿᵀᵁⱽᵂ°′"
    } else {
        "₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₐₑₕᵢⱼₖₗₘₙₒₚᵣₛₜᵤᵥₓᵦᵧᵨᵩᵪ"
    };
    value
        .chars()
        .map(|character| {
            source
                .chars()
                .position(|candidate| candidate == character)
                .and_then(|index| replacement.chars().nth(index))
        })
        .collect()
}

fn bold(value: &str) -> Option<String> {
    value
        .chars()
        .map(|character| match character {
            'A'..='Z' => char::from_u32(0x1d400 + character as u32 - 'A' as u32),
            'a'..='z' => char::from_u32(0x1d41a + character as u32 - 'a' as u32),
            '0'..='9' => char::from_u32(0x1d7ce + character as u32 - '0' as u32),
            character if character.is_ascii_punctuation() || character.is_whitespace() => {
                Some(character)
            }
            _ => None,
        })
        .collect()
}

fn blackboard(value: &str) -> Option<String> {
    value
        .chars()
        .map(|character| match character {
            'C' => Some('ℂ'),
            'H' => Some('ℍ'),
            'N' => Some('ℕ'),
            'P' => Some('ℙ'),
            'Q' => Some('ℚ'),
            'R' => Some('ℝ'),
            'Z' => Some('ℤ'),
            'A'..='Z' => char::from_u32(0x1d538 + character as u32 - 'A' as u32),
            'a'..='z' => char::from_u32(0x1d552 + character as u32 - 'a' as u32),
            '0'..='9' => char::from_u32(0x1d7d8 + character as u32 - '0' as u32),
            character if character.is_ascii_punctuation() || character.is_whitespace() => {
                Some(character)
            }
            _ => None,
        })
        .collect()
}

fn layout_matrix(rows: &[Vec<String>], environment: &str, block: bool) -> Option<String> {
    if !block && matches!(environment, "aligned" | "cases") {
        return None;
    }
    let (left, right) = match environment {
        "pmatrix" => ("(", ")"),
        "bmatrix" => ("[", "]"),
        "Bmatrix" => ("{", "}"),
        "vmatrix" => ("|", "|"),
        "Vmatrix" => ("‖", "‖"),
        "cases" => ("{", ""),
        _ => ("", ""),
    };
    if !block {
        let body = rows
            .iter()
            .map(|row| row.join(", "))
            .collect::<Vec<_>>()
            .join("; ");
        return Some(format!("{left}{body}{right}"));
    }
    let columns = rows.first()?.len();
    let widths = (0..columns)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].width())
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    let width = widths
        .iter()
        .sum::<usize>()
        .saturating_add(columns.saturating_mul(2))
        .saturating_add(4);
    if width.saturating_mul(rows.len()).saturating_mul(4) > MAX_OUTPUT_BYTES {
        return None;
    }
    let mut output = String::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            append(&mut output, "\n")?;
        }
        let top = index == 0;
        let bottom = index + 1 == rows.len();
        let (opening, closing) = if rows.len() == 1 {
            (left, right)
        } else {
            match environment {
                "pmatrix" => (
                    if top {
                        "⎛"
                    } else if bottom {
                        "⎝"
                    } else {
                        "⎜"
                    },
                    if top {
                        "⎞"
                    } else if bottom {
                        "⎠"
                    } else {
                        "⎟"
                    },
                ),
                "bmatrix" => (
                    if top {
                        "⎡"
                    } else if bottom {
                        "⎣"
                    } else {
                        "⎢"
                    },
                    if top {
                        "⎤"
                    } else if bottom {
                        "⎦"
                    } else {
                        "⎥"
                    },
                ),
                "Bmatrix" | "cases" => (
                    if top {
                        "⎧"
                    } else if bottom {
                        "⎩"
                    } else {
                        "⎨"
                    },
                    if environment == "cases" {
                        ""
                    } else if top {
                        "⎫"
                    } else if bottom {
                        "⎭"
                    } else {
                        "⎬"
                    },
                ),
                _ => (left, right),
            }
        };
        append(&mut output, opening)?;
        for (column, cell) in row.iter().enumerate() {
            if column > 0 {
                append(&mut output, "  ")?;
            }
            append(&mut output, cell)?;
            append(&mut output, &" ".repeat(widths[column] - cell.width()))?;
        }
        append(&mut output, closing)?;
    }
    Some(output)
}

fn boxed(content: &str) -> Option<String> {
    let lines = content.lines().collect::<Vec<_>>();
    let width = lines.iter().map(|line| line.width()).max()?;
    if width
        .saturating_add(4)
        .saturating_mul(lines.len().saturating_add(2))
        .saturating_mul(4)
        > MAX_OUTPUT_BYTES
    {
        return None;
    }
    let mut output = format!("┌{}┐\n", "─".repeat(width + 2));
    for line in lines {
        append(
            &mut output,
            &format!("│ {line}{} │\n", " ".repeat(width - line.width())),
        )?;
    }
    append(&mut output, &format!("└{}┘", "─".repeat(width + 2)))?;
    Some(output)
}

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
        "times" => "×",
        "cdot" => "⋅",
        "div" => "÷",
        "pm" => "±",
        "mp" => "∓",
        "ast" => "∗",
        "star" => "⋆",
        "circ" => "∘",
        "prime" => "′",
        "bullet" => "∙",
        "oplus" => "⊕",
        "otimes" => "⊗",
        "odot" => "⊙",
        "le" | "leq" => "≤",
        "ge" | "geq" => "≥",
        "ne" | "neq" => "≠",
        "equiv" => "≡",
        "approx" => "≈",
        "sim" => "∼",
        "simeq" => "≃",
        "cong" => "≅",
        "propto" => "∝",
        "ll" => "≪",
        "gg" => "≫",
        "in" => "∈",
        "notin" => "∉",
        "ni" => "∋",
        "subset" => "⊂",
        "supset" => "⊃",
        "subseteq" => "⊆",
        "supseteq" => "⊇",
        "cup" => "∪",
        "cap" => "∩",
        "setminus" => "∖",
        "emptyset" | "varnothing" => "∅",
        "forall" => "∀",
        "exists" => "∃",
        "nexists" => "∄",
        "land" | "wedge" => "∧",
        "lor" | "vee" => "∨",
        "neg" | "lnot" => "¬",
        "therefore" => "∴",
        "because" => "∵",
        "infty" => "∞",
        "sum" => "∑",
        "prod" => "∏",
        "coprod" => "∐",
        "int" => "∫",
        "iint" => "∬",
        "iiint" => "∭",
        "oint" => "∮",
        "partial" => "∂",
        "nabla" => "∇",
        "ell" => "ℓ",
        "hbar" => "ℏ",
        "Re" => "ℜ",
        "Im" => "ℑ",
        "to" | "rightarrow" => "→",
        "leftarrow" => "←",
        "leftrightarrow" => "↔",
        "Rightarrow" => "⇒",
        "Leftarrow" => "⇐",
        "Leftrightarrow" => "⇔",
        "mapsto" => "↦",
        "longrightarrow" => "⟶",
        "ldots" => "…",
        "cdots" => "⋯",
        "vdots" => "⋮",
        "ddots" => "⋱",
        "langle" => "⟨",
        "rangle" => "⟩",
        "lbrace" | "{" => "{",
        "rbrace" | "}" => "}",
        "vert" | "|" => "|",
        "Vert" => "‖",
        "," => "\u{2009}",
        ";" | ":" => " ",
        "!" => "",
        "quad" => "  ",
        "qquad" => "    ",
        " " => " ",
        "$" => "$",
        "%" => "%",
        "&" => "&",
        "#" => "#",
        "_" => "_",
        "sin" => "sin",
        "cos" => "cos",
        "tan" => "tan",
        "cot" => "cot",
        "sec" => "sec",
        "csc" => "csc",
        "log" => "log",
        "ln" => "ln",
        "exp" => "exp",
        "lim" => "lim",
        "min" => "min",
        "max" => "max",
        "det" => "det",
        "gcd" => "gcd",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_unicode_math_and_explicit_fraction_precedence() {
        for (source, expected) in [
            ("x^2 + y_1", "x² + y₁"),
            (r"A^T x^N = 90^\circ", "Aᵀ xᴺ = 90°"),
            (r"x^\prime", "x′"),
            (r"\alpha + \beta \leq \gamma", "α + β ≤ γ"),
            (r"\frac{a+b}{c}", "((a+b)/c)"),
            (r"\frac12", "(½)"),
            (r"\sqrt{x^2+1}", "√(x²+1)"),
            (r"\sqrt[3]{8}", "∛(8)"),
            (r"\sum_{i=0}^{n} x_i", "∑ᵢ₌₀ⁿ xᵢ"),
            (r"\int_0^1 x\,dx", "∫₀¹ x\u{2009}dx"),
            (r"{ab}^2", "(ab)²"),
            (r"\left( a+b \right)", "(a+b)"),
            (
                r"\text{rate per second} = \mathrm{m}/\mathrm{s}",
                "rate per second = m/s",
            ),
            (r"\mathbf{Ab2} \in \mathbb{R}", "𝐀𝐛𝟐 ∈ ℝ"),
            (r"\vec{x} + \hat{y}", "x\u{20d7} + y\u{302}"),
        ] {
            assert_eq!(render(source).as_deref(), Some(expected), "{source}");
        }
    }

    #[test]
    fn adjacent_fractions_and_roots_keep_explicit_expression_boundaries() {
        for (source, expected) in [
            (r"\frac{a}{b}c", "(a/b)c"),
            (r"c\frac{a}{b}", "c(a/b)"),
            (r"\frac{a}{b}\frac{c}{d}", "(a/b)(c/d)"),
            (r"\frac{a}{b}^2", "(a/b)²"),
            (r"\frac12x", "(½)x"),
            (r"\sqrt{a}b", "√(a)b"),
            (r"a\sqrt{b}", "a√(b)"),
            (r"\sqrt{a}\sqrt{b}", "√(a)√(b)"),
            (r"\sqrt{ab}^2", "√(ab)²"),
            (r"\frac{\sqrt{x}}{y}z", "((√(x))/y)z"),
        ] {
            assert_eq!(render(source).as_deref(), Some(expected), "{source}");
            assert_eq!(render_block(source).as_deref(), Some(expected), "{source}");
        }
    }

    #[test]
    fn math_symbol_codepoints_follow_the_tex_symbol_mapping() {
        assert_eq!(
            render(r"\phi + \varphi + \cdot + \bullet").as_deref(),
            Some("\u{03d5} + \u{03c6} + \u{22c5} + \u{2219}")
        );
    }

    #[test]
    fn nested_fonts_and_multiline_left_right_delimiters_return_to_source() {
        for source in [
            r"\mathbf{\mathrm{x}}",
            r"\mathrm{\mathbf{x}}",
            r"\mathbf{\mathbf{x}}",
            r"\mathbb{\mathrm{x}}",
            r"\mathrm{\mathnormal{x}}",
            r"\mathbf{\text{x}}",
            r"\mathnormal{\operatorname{x}}",
            r"\text{\mathbf{x}}",
        ] {
            assert!(render(source).is_none(), "{source}");
            assert!(render_block(source).is_none(), "{source}");
        }
        assert_eq!(render(r"\mathbf{x}+\mathrm{y}").as_deref(), Some("𝐱+y"));
        let source = r"\left(\begin{matrix}a\\b\end{matrix}\right)";
        assert_eq!(render(source).as_deref(), Some("(a; b)"));
        assert!(render_block(source).is_none());
        assert!(render_block(r"\left\{\begin{aligned}a&b\\c&d\end{aligned}\right.").is_none());
    }

    #[test]
    fn unknown_incomplete_or_unrepresentable_math_returns_to_source() {
        for source in [
            "",
            " ",
            r"\unknown{x}",
            r"\frac{a}",
            r"\frac{}{b}",
            r"\sqrt",
            "x^",
            "x_q",
            "x^{y^2}",
            "x^{1",
            "x}",
            "^2",
            "x^2^3",
            r"\left(x",
            r"\right)",
            r"\text{unterminated",
            r"\text{\unknown{x}}",
            r"\sqrt[q]{x}",
            r"\hat{xy}",
            r"\input{file}",
            r"\def\x{a}",
            r"\begin{unknown}x\end{unknown}",
            r"\begin{matrix}1&2\\3\end{matrix}",
            r"\begin{matrix}1\end{bmatrix}",
            r"\begin{matrix}1\\[2pt]2\end{matrix}",
            r"\boxed{x}",
            "a & b",
            "x % comment",
        ] {
            assert!(render(source).is_none(), "unexpectedly rendered {source:?}");
        }
    }

    #[test]
    fn matrices_and_aligned_blocks_have_consistent_columns() {
        let source = r"\begin{bmatrix}1&20\\300&4\end{bmatrix}";
        assert_eq!(render(source).as_deref(), Some("[1, 20; 300, 4]"));
        assert_eq!(
            render_block(source).as_deref(),
            Some("⎡1    20⎤\n⎣300  4 ⎦")
        );
        assert_eq!(
            render_block(r"\begin{aligned}a &= b\\c &= d\end{aligned}").as_deref(),
            Some("a  = b\nc  = d")
        );
        assert!(render(r"\begin{aligned}a &= b\\c &= d\end{aligned}").is_none());
        assert!(render(r"\begin{cases}1 & x>0\\0 & x<0\end{cases}").is_none());
        assert!(render_block(&format!("x + {source}")).is_none());
        assert!(render_block(&format!("{source} + x")).is_none());
        assert!(render_block(r"\frac{\begin{matrix}1\\2\end{matrix}}{3}").is_none());
        assert_eq!(
            render_block(r"\boxed{x^2}").as_deref(),
            Some("┌────┐\n│ x² │\n└────┘")
        );
    }

    #[test]
    fn unicode_text_and_escaped_literals_are_kept_without_terminal_controls() {
        assert_eq!(
            render(r"\text{中文 与 é} + α").as_deref(),
            Some("中文 与 é + α")
        );
        assert_eq!(render(r"\text{a \& b}").as_deref(), Some("a & b"));
        for character in ['\u{1b}', '\u{7}', '\u{9b}', '\u{202e}', '\u{2066}', '\r'] {
            assert!(render(&format!("x{character}y")).is_none());
        }
    }

    #[test]
    fn parser_and_layout_budgets_reject_overflow_without_panicking() {
        assert!(render(&"x".repeat(MAX_INPUT_BYTES + 1)).is_none());
        let deep = format!(
            "{}x{}",
            "{".repeat(MAX_DEPTH + 1),
            "}".repeat(MAX_DEPTH + 1)
        );
        assert!(render(&deep).is_none());
        assert!(render(&format!("{}x", r"\sqrt".repeat(MAX_DEPTH + 1))).is_none());
        let many_columns = format!(
            r"\begin{{matrix}}{}\end{{matrix}}",
            vec!["x"; MAX_MATRIX_DIMENSION + 1].join("&")
        );
        assert!(render(&many_columns).is_none());
        let many_rows = format!(
            r"\begin{{matrix}}{}\end{{matrix}}",
            vec!["x"; MAX_MATRIX_DIMENSION + 1].join(r"\\")
        );
        assert!(render_block(&many_rows).is_none());
        for source in [
            "\\".repeat(8000),
            "x_".repeat(4000),
            "{".repeat(8000),
            "中".repeat(5000),
        ] {
            let _ = render(&source);
        }
    }
}
