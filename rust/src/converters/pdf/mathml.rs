//! Math-run detection and Unicode → LaTeX emission.
//!
//! arXiv equations reach render as Unicode text (∇f(x) ≤ ‖v‖) with
//! private-use sentinels marking super/subscript runs (attached at
//! merge time in shared.rs). This module finds the mathematical runs
//! inside a line, converts them to LaTeX, and wraps them in `$…$`;
//! the sentinels are stripped from everything else.

use super::shared::{SUB_CLOSE, SUB_OPEN, SUP_CLOSE, SUP_OPEN};

/// Is this character strong evidence of mathematics?
fn is_strong_math(c: char) -> bool {
    matches!(c,
        '\u{2200}'..='\u{22FF}'   // mathematical operators
        | '\u{2A00}'..='\u{2AFF}' // supplemental operators
        | '\u{27E6}'..='\u{27EB}' // mathematical brackets
        | '\u{2190}'..='\u{21FF}' // arrows
        | '\u{0370}'..='\u{03FF}' // Greek
        | '\u{2100}'..='\u{214F}' // letterlike (ℓ, ℝ, ℑ…)
        | '\u{1D400}'..='\u{1D7FF}' // mathematical alphanumerics
        | '\u{2308}'..='\u{230B}' // ceilings and floors
        | '\u{2032}' | '\u{2033}' // primes
        | '\u{2016}'              // double bar
        | '\u{00B1}' | '\u{00D7}' | '\u{00F7}' | '\u{00AC}'
        | '\u{0338}'              // negation slash
    )
    // Script sentinels are NOT strong on their own: footnote markers
    // in prose carry them too. They only convert inside a run that
    // real math characters established.
}

fn has_strong_math(s: &str) -> bool {
    s.chars().any(is_strong_math)
}

/// May this word sit INSIDE a math run without being evidence itself?
/// Short identifier/operator material, script-carrying words, and
/// standard function names.
fn is_weak_math_word(s: &str) -> bool {
    // Script structure always joins the run it sits in.
    if s.chars()
        .any(|c| matches!(c, SUP_OPEN | SUP_CLOSE | SUB_OPEN | SUB_CLOSE))
    {
        return true;
    }
    let n = s.chars().count();
    if n == 0 || n > 8 {
        return false;
    }
    // Standard function names, possibly glued to parens.
    let bare = s
        .trim_end_matches(['(', ')', ',', '.', ';', ':'])
        .trim_start_matches('(');
    if matches!(
        bare,
        "sign"
            | "sin"
            | "cos"
            | "tan"
            | "log"
            | "ln"
            | "exp"
            | "min"
            | "max"
            | "lim"
            | "sup"
            | "inf"
            | "det"
            | "tr"
            | "dim"
            | "deg"
            | "mod"
            | "argmin"
            | "argmax"
    ) {
        return true;
    }
    let alpha_run = s.chars().filter(|c| c.is_ascii_alphabetic()).count();
    if alpha_run > 2 {
        return false;
    }
    // Short English words are prose, not identifiers.
    if matches!(
        s.trim_end_matches([',', '.', ';', ':']),
        "as" | "at"
            | "be"
            | "by"
            | "do"
            | "he"
            | "if"
            | "in"
            | "is"
            | "it"
            | "no"
            | "of"
            | "on"
            | "or"
            | "so"
            | "to"
            | "up"
            | "we"
            | "an"
            | "my"
            | "me"
            | "us"
            | "go"
    ) {
        return false;
    }
    s.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '=' | '+'
                    | '-'
                    | '*'
                    | '/'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '|'
                    | '<'
                    | '>'
                    | ','
                    | '.'
                    | ':'
                    | ';'
                    | '!'
                    | '\''
            )
    })
}

/// Convert one math run (already known mathematical) to LaTeX.
fn to_latex(run: &str) -> String {
    let mut out = String::with_capacity(run.len() * 2);
    let mut pending_not = false;
    for c in run.chars() {
        if c == '\u{0338}' {
            pending_not = true;
            continue;
        }
        let mapped: &str = match c {
            SUP_OPEN => "^{",
            SUP_CLOSE | SUB_CLOSE => "}",
            SUB_OPEN => "_{",
            '≤' => r"\le ",
            '≥' => r"\ge ",
            '≠' => r"\neq ",
            '∈' => r"\in ",
            '∉' => r"\notin ",
            '∋' => r"\ni ",
            '⊂' => r"\subset ",
            '⊆' => r"\subseteq ",
            '⊃' => r"\supset ",
            '⊇' => r"\supseteq ",
            '∪' => r"\cup ",
            '∩' => r"\cap ",
            '∧' => r"\wedge ",
            '∨' => r"\vee ",
            '∀' => r"\forall ",
            '∃' => r"\exists ",
            '∅' => r"\emptyset ",
            '∇' => r"\nabla ",
            '∂' => r"\partial ",
            '∑' => r"\sum ",
            '∏' => r"\prod ",
            '∐' => r"\coprod ",
            '∫' => r"\int ",
            '∮' => r"\oint ",
            '√' => r"\sqrt ",
            '∞' => r"\infty ",
            '±' => r"\pm ",
            '∓' => r"\mp ",
            '×' => r"\times ",
            '÷' => r"\div ",
            '⋅' => r"\cdot ",
            '∗' => "*",
            '∘' => r"\circ ",
            '‖' => r"\|",
            '⟨' => r"\langle ",
            '⟩' => r"\rangle ",
            '⌊' => r"\lfloor ",
            '⌋' => r"\rfloor ",
            '⌈' => r"\lceil ",
            '⌉' => r"\rceil ",
            '→' => r"\to ",
            '←' => r"\leftarrow ",
            '↦' => r"\mapsto ",
            '⇒' => r"\Rightarrow ",
            '⇐' => r"\Leftarrow ",
            '↔' => r"\leftrightarrow ",
            '⇔' => r"\Leftrightarrow ",
            '∼' => r"\sim ",
            '≃' => r"\simeq ",
            '≈' => r"\approx ",
            '≅' => r"\cong ",
            '≡' => r"\equiv ",
            '≍' => r"\asymp ",
            '∝' => r"\propto ",
            '≪' => r"\ll ",
            '≫' => r"\gg ",
            '≺' => r"\prec ",
            '≻' => r"\succ ",
            '⪯' => r"\preceq ",
            '⪰' => r"\succeq ",
            '⊥' => r"\perp ",
            '⊤' => r"\top ",
            '⊢' => r"\vdash ",
            '⊣' => r"\dashv ",
            '⊕' => r"\oplus ",
            '⊗' => r"\otimes ",
            '⊖' => r"\ominus ",
            '⊙' => r"\odot ",
            '⊔' => r"\sqcup ",
            '⊓' => r"\sqcap ",
            '∖' => r"\setminus ",
            '∣' => r"\mid ",
            '∥' => r"\parallel ",
            '∴' => r"\therefore ",
            '∵' => r"\because ",
            '⋆' => r"\star ",
            '⋄' => r"\diamond ",
            '·' => r"\cdot ",
            '−' | '–' => "-",
            'ℓ' => r"\ell ",
            'ℏ' => r"\hbar ",
            'ℜ' => r"\Re ",
            'ℑ' => r"\Im ",
            'ℵ' => r"\aleph ",
            '℘' => r"\wp ",
            '′' => "'",
            '″' => "''",
            // Greek lowercase.
            'α' => r"\alpha ",
            'β' => r"\beta ",
            'γ' => r"\gamma ",
            'δ' => r"\delta ",
            'ε' => r"\varepsilon ",
            'ϵ' => r"\epsilon ",
            'ζ' => r"\zeta ",
            'η' => r"\eta ",
            'θ' => r"\theta ",
            'ϑ' => r"\vartheta ",
            'ι' => r"\iota ",
            'κ' => r"\kappa ",
            'λ' => r"\lambda ",
            'μ' => r"\mu ",
            'ν' => r"\nu ",
            'ξ' => r"\xi ",
            'π' => r"\pi ",
            'ϖ' => r"\varpi ",
            'ρ' => r"\rho ",
            'ϱ' => r"\varrho ",
            'σ' => r"\sigma ",
            'ς' => r"\varsigma ",
            'τ' => r"\tau ",
            'υ' => r"\upsilon ",
            'φ' => r"\varphi ",
            'ϕ' => r"\phi ",
            'χ' => r"\chi ",
            'ψ' => r"\psi ",
            'ω' => r"\omega ",
            // Greek uppercase.
            'Γ' => r"\Gamma ",
            'Δ' => r"\Delta ",
            'Θ' => r"\Theta ",
            'Λ' => r"\Lambda ",
            'Ξ' => r"\Xi ",
            'Π' => r"\Pi ",
            'Σ' => r"\Sigma ",
            'Υ' => r"\Upsilon ",
            'Φ' => r"\Phi ",
            'Ψ' => r"\Psi ",
            'Ω' => r"\Omega ",
            // Blackboard and script letters.
            'ℝ' => r"\mathbb{R}",
            'ℕ' => r"\mathbb{N}",
            'ℤ' => r"\mathbb{Z}",
            'ℚ' => r"\mathbb{Q}",
            'ℂ' => r"\mathbb{C}",
            'ℍ' => r"\mathbb{H}",
            'ℙ' => r"\mathbb{P}",
            '%' => r"\% ",
            '&' => r"\& ",
            '#' => r"\# ",
            '~' => r"\sim ",
            other => {
                if let Some(bb) = blackboard(other) {
                    out.push_str(r"\mathbb{");
                    out.push(bb);
                    out.push('}');
                } else if let Some(sc) = script_letter(other) {
                    out.push_str(r"\mathcal{");
                    out.push(sc);
                    out.push('}');
                } else {
                    if pending_not {
                        out.push_str(r"\not ");
                        pending_not = false;
                    }
                    out.push(other);
                }
                continue;
            }
        };
        if pending_not {
            out.push_str(r"\not ");
            pending_not = false;
        }
        out.push_str(mapped);
    }
    // Command mappings carry a trailing space; joining words adds
    // another. KaTeX ignores runs of spaces, but keep output tidy.
    let mut tidy = String::with_capacity(out.len());
    let mut prev_space = false;
    for c in out.trim_end().chars() {
        if c == ' ' {
            if !prev_space {
                tidy.push(c);
            }
            prev_space = true;
        } else {
            tidy.push(c);
            prev_space = false;
        }
    }
    tidy
}

fn blackboard(c: char) -> Option<char> {
    let v = c as u32;
    (0x1D538..=0x1D551)
        .contains(&v)
        .then(|| char::from_u32('A' as u32 + (v - 0x1D538)))?
}

fn script_letter(c: char) -> Option<char> {
    match c {
        '\u{212C}' => Some('B'),
        '\u{2130}' => Some('E'),
        '\u{2131}' => Some('F'),
        '\u{210B}' => Some('H'),
        '\u{2110}' => Some('I'),
        '\u{2112}' => Some('L'),
        '\u{2133}' => Some('M'),
        '\u{211B}' => Some('R'),
        _ => {
            let v = c as u32;
            (0x1D49C..=0x1D4B5)
                .contains(&v)
                .then(|| char::from_u32('A' as u32 + (v - 0x1D49C)))?
        }
    }
}

/// Strip script sentinels from text destined for non-math contexts
/// (headings, table cells).
pub(crate) fn strip_script_sentinels(s: &str) -> String {
    if !s
        .chars()
        .any(|c| matches!(c, SUP_OPEN | SUP_CLOSE | SUB_OPEN | SUB_CLOSE))
    {
        return s.to_string();
    }
    strip_sentinels(s)
}

fn strip_sentinels(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(*c, SUP_OPEN | SUP_CLOSE | SUB_OPEN | SUB_CLOSE))
        .collect()
}

/// Split a line into plain and math segments; math segments become
/// `$latex$`, plain segments pass through `escape_plain`. Sentinels
/// never survive.
pub(crate) fn render_line_with_math(text: &str, escape_plain: impl Fn(&str) -> String) -> String {
    if !has_strong_math(text) {
        return escape_plain(&strip_sentinels(text));
    }
    let words: Vec<&str> = text.split(' ').collect();
    #[derive(PartialEq, Clone, Copy)]
    enum Kind {
        Strong,
        Weak,
        Plain,
    }
    let kinds: Vec<Kind> = words
        .iter()
        .map(|w| {
            if has_strong_math(w) {
                Kind::Strong
            } else if is_weak_math_word(w) {
                Kind::Weak
            } else {
                Kind::Plain
            }
        })
        .collect();

    let mut out_parts: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < words.len() {
        if kinds[i] == Kind::Plain {
            out_parts.push(escape_plain(&strip_sentinels(words[i])));
            i += 1;
            continue;
        }
        // A maximal non-plain segment, leading and trailing weak words
        // included ("0 ≤ n ≤ 2N": the 0 and the 2N belong inside).
        let mut j = i;
        let mut any_strong = false;
        while j < words.len() && kinds[j] != Kind::Plain {
            any_strong |= kinds[j] == Kind::Strong;
            j += 1;
        }
        let run = words[i..j].join(" ");
        // A single stray math character in prose (α-helix, 500 × 300)
        // is not an equation: demand two pieces of strong evidence, or
        // one plus script structure.
        let strong_chars = run.chars().filter(|c| is_strong_math(*c)).count();
        let has_scripts = run
            .chars()
            .any(|c| matches!(c, SUP_OPEN | SUP_CLOSE | SUB_OPEN | SUB_CLOSE));
        if any_strong && (strong_chars >= 2 || (strong_chars >= 1 && has_scripts)) {
            out_parts.push(format!("${}$", to_latex(&run)));
        } else {
            for w in &words[i..j] {
                out_parts.push(escape_plain(&strip_sentinels(w)));
            }
        }
        i = j;
    }
    out_parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn plain_prose_untouched() {
        assert_eq!(
            render_line_with_math("Just ordinary prose here.", ident),
            "Just ordinary prose here."
        );
    }

    #[test]
    fn norm_inequality_becomes_latex() {
        let line = "we have: ‖v‖\u{E002}2\u{E003} ≤ ‖v‖\u{E002}q\u{E003},";
        let out = render_line_with_math(line, ident);
        assert!(out.contains(r"$\|v\|_{2} \le \|v\|_{q},$"), "{out}");
        assert!(out.starts_with("we have: "), "{out}");
    }

    #[test]
    fn sentinels_are_stripped_from_plain_text() {
        let line = "footnote\u{E000}1\u{E001} continues";
        assert_eq!(render_line_with_math(line, ident), "footnote1 continues");
    }

    #[test]
    fn greek_and_operators_map() {
        let out = render_line_with_math("λ ∈ Ω", ident);
        assert_eq!(out, r"$\lambda \in \Omega$");
    }

    #[test]
    fn negation_slash_becomes_not() {
        let out = render_line_with_math("x ̸= y ∈ Z", ident);
        assert!(out.contains(r"\not"), "{out}");
        assert!(out.contains(r"\in"), "{out}");
    }

    #[test]
    fn single_stray_math_char_stays_prose() {
        assert_eq!(
            render_line_with_math("the α-helix structure", ident),
            "the α-helix structure"
        );
        assert_eq!(
            render_line_with_math("a 500 × 300 mm panel", ident),
            "a 500 × 300 mm panel"
        );
    }
}
