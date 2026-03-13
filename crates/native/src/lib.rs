pub mod config;
pub mod prover;
pub mod ask;
pub mod cache;
pub mod cli;

pub use ask::{ask, AskOptions, AskResult};
pub use cache::{load_cache, save_cache};

pub use sumo_parser_core::{
    KifError, KifStore as Store, KnowledgeBase as Kb, ParseError, SemanticError, TellResult,
};

// Error reporting macros

#[macro_export]
macro_rules! parse_error {
    ($span:expr, $e:expr) => {
        {
            use inline_colorization::*;
            log::error!(
                "{}{}{}, {}line {}{}\n{style_bold}{color_bright_red}{}{style_reset}\n",
                color_magenta,
                $span.file,
                color_reset,
                style_bold,
                $span.line,
                style_reset,
                $e
            );
        }
    };

    ($span:expr, $e:expr, $txt:expr) => {
        {
            use inline_colorization::*;
            let line_start = $txt[..$span.offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let line_end = $txt[$span.offset..].find('\n').map(|i| i + $span.offset).unwrap_or($txt.len());
            let width: usize = $span.col as usize + 9;
            log::error!(
                "{}{}{}\n\n {:<6}| {}\n{color_bright_red}{style_bold}{:>width$} {}{color_reset}\n",
                color_magenta,
                $span.file,
                color_reset,
                $span.line,
                &$txt[line_start..line_end],
                "^",
                $e,
            );
        }
    };
}

#[macro_export]
macro_rules! semantic_error {
    ($span:expr, $e:expr, $sid:expr, $kb:expr) => {
        {
            use inline_colorization::*;
            log::error!(
                "{}{}{}\n",
                color_magenta,
                $span.file,
                color_reset,
            );
            $e.pretty_print(&$kb.store, log::Level::Error);
            eprintln!()
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use sumo_parser_core::{KifStore, KnowledgeBase, load_kif};

    const BASE: &str = "
        (subclass Relation Entity)
        (subclass BinaryRelation Relation)
        (subclass Predicate Relation)
        (subclass BinaryPredicate Predicate)
        (subclass BinaryPredicate BinaryRelation)
        (instance subclass BinaryRelation)
        (domain subclass 1 Class)
        (domain subclass 2 Class)
        (instance instance BinaryPredicate)
        (domain instance 1 Entity)
        (domain instance 2 Class)
        (subclass Animal Entity)
        (subclass Human Entity)
        (subclass Human Animal)
    ";

    fn base_kb() -> KnowledgeBase {
        let mut store = KifStore::default();
        load_kif(&mut store, BASE, "base");
        KnowledgeBase::new(store)
    }

    #[test]
    fn ask_parse_error() {
        let mut kb = base_kb();
        let r = ask(&mut kb, "(subclass Cat", AskOptions::default());
        assert!(!r.proved);
        assert!(!r.errors.is_empty());
    }

    #[test]
    fn ask_generates_tptp_conjecture() {
        let mut kb = base_kb();
        let r = ask(
            &mut kb,
            "(subclass Human Animal)",
            AskOptions {
                keep_tmp_file: true,
                ..AskOptions::default()
            },
        );
        if let Some(ref p) = r.tmp_file {
            let content = std::fs::read_to_string(p).unwrap();
            assert!(
                content.contains("conjecture"),
                "missing conjecture in: {}",
                content
            );
            std::fs::remove_file(p).ok();
        }
    }

    #[test]
    fn cache_round_trip() {
        let mut store = KifStore::default();
        load_kif(&mut store, "(subclass Human Animal)", "test");

        let tmp = std::env::temp_dir().join("sumo_cache_test.json");
        save_cache(&store, &tmp).expect("save_cache");

        let restored = load_cache(&tmp).expect("load_cache");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(restored.roots.len(), store.roots.len());
        assert!(restored.symbols.contains_key("Human"));
        assert!(restored.symbols.contains_key("Animal"));
    }
}
