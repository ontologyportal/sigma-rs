//! `sumo celt TEXT...` -- controlled English to SUO-KIF.
//!
//! Runs `sigmakee_rs_sdk::celt::translate` over the session's KB and
//! installed WordNet lexicon and renders each sentence's outcome: its KIF
//! formulas, or the refusal with a caret under the offending span and a
//! rewrite hint.

use std::io::Read;
use std::path::PathBuf;

use crate::style::*;
use sigmakee_rs_sdk::celt::{self, Kind, Logic, Opts, Refused, Translation};
use sigmakee_rs_sdk::{manager::KBManager, Session, TopLayer};

/// The reading conventions `sumo celt` passes through to [`Opts`].
#[derive(Debug, Clone, Default)]
pub struct CeltFlags {
    pub assume_names: bool,
    pub generic: bool,
    pub speaker: Option<String>,
    pub addressee: Option<String>,
    pub generic_you: Option<String>,
    pub now: Option<String>,
    pub kif_only: bool,
    pub lines: bool,
    pub parrot: bool,
    pub explain: bool,
}

pub fn run_celt<L>(
    session: Session<L>,
    manager: KBManager,
    text: Vec<String>,
    input: Option<PathBuf>,
    names: Option<PathBuf>,
    flags: CeltFlags,
) -> bool
where
    L: TopLayer,
{
    let Some(wn) = session.lexicon() else {
        log::error!(
            "sumo celt needs the WordNet mappings: point <lexicon> in config.xml at a \
             WordNetMappings directory, or place one under <kbDir>/WordNetMappings"
        );
        return false;
    };

    let input = match read_input(&text, input.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            log::error!("{e}");
            return false;
        }
    };
    if input.trim().is_empty() {
        log::error!("no text to translate (pass TEXT, --input FILE, or pipe to stdin)");
        return false;
    }

    let names = match names {
        Some(path) => match sigmakee_rs_sdk::lexicon::load_first_name_genders(&path) {
            Ok(n) => n,
            Err(e) => {
                log::error!("--names: {e}");
                return false;
            }
        },
        None => manager.load_first_name_genders().unwrap_or_default(),
    };
    let opts = Opts {
        assume_human_names: flags.assume_names,
        names: names.iter().map(|(n, _)| n.clone()).collect(),
        generic_subjects: flags.generic,
        name_genders: names
            .into_iter()
            .filter_map(|(n, g)| g.map(|g| (n, g)))
            .collect(),
        speaker: flags.speaker,
        addressee: flags.addressee,
        generic_you: flags.generic_you,
        now: flags.now,
        ..Opts::default()
    };
    let translation = if flags.lines {
        let mut all = Translation::default();
        for line in input.lines().filter(|l| !l.trim().is_empty()) {
            let t = celt::translate(session.kb(), wn, line, &opts);
            all.sentences.extend(t.sentences);
        }
        all
    } else {
        celt::translate(session.kb(), wn, &input, &opts)
    };
    let parrot: Option<&dyn Fn(&str) -> String> = if flags.parrot {
        Some(&|kif| parrot_back(&session, kif))
    } else {
        None
    };

    if flags.kif_only {
        print_kif_only(&translation, parrot);
    } else {
        print_report(&translation, flags.explain, parrot);
    }
    translation.complete()
}

/// The text to translate: `--input`, else the positional words, else stdin.
fn read_input(text: &[String], file: Option<&std::path::Path>) -> Result<String, String> {
    if let Some(path) = file {
        return std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()));
    }
    if !text.is_empty() {
        return Ok(text.join(" "));
    }
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .map_err(|e| format!("stdin: {e}"))?;
    Ok(s)
}

/// English for a formula through the KB's `format`/`termFormat`
/// templates, variables as generic noun phrases.
#[cfg(feature = "ask")]
fn parrot_back<L: TopLayer>(session: &Session<L>, kif: &str) -> String {
    session.parrot(kif)
}

#[cfg(not(feature = "ask"))]
fn parrot_back<L: TopLayer>(_session: &Session<L>, _kif: &str) -> String {
    "(natural-language rendering needs the `ask` feature)".into()
}

fn print_kif_only(translation: &Translation, parrot: Option<&dyn Fn(&str) -> String>) {
    for sentence in &translation.sentences {
        match &sentence.outcome {
            Ok(logic) => {
                for f in logic.declared.conjuncts() {
                    println!("{f}");
                }
                for f in own_formulas(logic) {
                    println!("{f}");
                    if let Some(p) = parrot {
                        println!(";; ~ {}", p(&f.to_string()));
                    }
                }
            }
            Err(refused) => println!(";; {}: {}", sentence.text, refused),
        }
    }
}

/// The sentence's own formulas without its side assertions: the tellable
/// conjuncts of an assertion, or the single conjecture of a query.
fn own_formulas(logic: &Logic) -> Vec<sigmakee_rs_sdk::AstNode> {
    match logic.kind {
        Kind::Assert => logic.drs.conjuncts(),
        Kind::Query => vec![logic.formula()],
    }
}

fn print_report(translation: &Translation, explain: bool, parrot: Option<&dyn Fn(&str) -> String>) {
    for sentence in &translation.sentences {
        println!("{style_bold}{}{style_reset}", sentence.text);
        match &sentence.outcome {
            Ok(logic) => print_logic(logic, explain, parrot),
            Err(refused) => print_refusal(&sentence.text, refused),
        }
    }

    let total = translation.sentences.len();
    let ok = translation
        .sentences
        .iter()
        .filter(|s| s.outcome.is_ok())
        .count();
    let color = if ok == total {
        color_bright_green
    } else {
        color_bright_yellow
    };
    println!("{color}{ok}/{total} sentences translated{color_reset}");
    for (code, count) in translation.histogram() {
        println!("{color_bright_black}  {code}: {count}{color_reset}");
    }
}

fn print_logic(logic: &Logic, explain: bool, parrot: Option<&dyn Fn(&str) -> String>) {
    let tag = match logic.kind {
        Kind::Assert => "",
        Kind::Query => "?- ",
    };
    for f in logic.declared.conjuncts() {
        println!("    {color_bright_cyan}{f}{color_reset}");
    }
    for f in own_formulas(logic) {
        println!("    {color_bright_cyan}{tag}{f}{color_reset}");
        if let Some(p) = parrot {
            println!(
                "    {color_bright_green}~ {}{color_reset}",
                p(&f.to_string())
            );
        }
    }
    if !explain {
        return;
    }
    println!("    {color_bright_black}rule: {}{color_reset}", logic.rule);
    for c in &logic.choices {
        let alternatives = if c.alternatives.is_empty() {
            String::new()
        } else {
            format!(" [{}]", c.alternatives.join(" "))
        };
        println!(
            "    {color_bright_black}{} -> {color_yellow}{}{color_bright_black} ({}){}{color_reset}",
            c.word, c.term, c.sense, alternatives
        );
    }
}

fn print_refusal(text: &str, refused: &Refused) {
    println!(
        "    {color_bright_red}refused [{}]: {}{color_reset}",
        refused.reason.code(),
        refused.reason
    );
    println!(
        "    {color_bright_black}hint: {}{color_reset}",
        refused.reason.hint()
    );
    let (start, end) = caret_span(text, refused);
    if end > start {
        println!("    {color_bright_black}{text}{color_reset}");
        println!(
            "    {}{color_bright_red}{}{color_reset}",
            " ".repeat(start),
            "^".repeat(end - start)
        );
    }
}

/// Clamp the refusal's char span to the sentence so a stray offset never
/// panics the renderer; an empty span draws nothing.
fn caret_span(text: &str, refused: &Refused) -> (usize, usize) {
    let len = text.chars().count();
    let end = refused.end.min(len);
    let start = refused.start.min(end);
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigmakee_rs_sdk::celt::Refusal;

    fn refused(start: usize, end: usize) -> Refused {
        Refused::new(
            Refusal::UnknownWord {
                word: "x".to_string(),
            },
            start,
            end,
        )
    }

    #[test]
    fn caret_span_clamps_to_sentence() {
        assert_eq!(caret_span("Rex is a dog.", &refused(9, 12)), (9, 12));
        assert_eq!(caret_span("Rex is a dog.", &refused(9, 99)), (9, 13));
        assert_eq!(caret_span("Rex is a dog.", &refused(50, 60)), (13, 13));
        assert_eq!(caret_span("Rex is a dog.", &refused(5, 2)), (2, 2));
    }

    #[test]
    fn read_input_joins_positional_words() {
        assert_eq!(
            read_input(&["Rex".to_string(), "is".to_string()], None).unwrap(),
            "Rex is"
        );
    }

    #[test]
    fn read_input_missing_file_is_an_error() {
        let err = read_input(&[], Some(std::path::Path::new("/does/not/exist.txt"))).unwrap_err();
        assert!(err.contains("/does/not/exist.txt"), "{err}");
    }
}
