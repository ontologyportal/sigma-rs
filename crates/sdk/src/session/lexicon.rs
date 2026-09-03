//! WordNet lexicon installation on a [`Session`] -- separate from KIF
//! ingestion (`Session::ingest` / `Session::load_kif`): the lexicon is
//! static reference data held as a sibling to the KB rather than inside it
//! (see `sigmakee_rs_core::lexicon`'s module docs).

use std::sync::Arc;

use sigmakee_rs_core::lexicon::WordNet;
use sigmakee_rs_core::TopLayer;

use crate::manager::KBManager;

use super::Session;

impl<L: TopLayer> Session<L> {
    /// Install a WordNet lexicon directly, replacing any previously loaded
    /// one. `None` clears it -- subsequent `search` calls stop returning
    /// WordNet hits, same as if none had ever been loaded.
    pub fn set_lexicon(&mut self, lexicon: Option<Arc<WordNet>>) {
        self.lexicon = lexicon;
    }

    /// The currently installed WordNet lexicon, if any.
    pub fn lexicon(&self) -> Option<&Arc<WordNet>> {
        self.lexicon.as_ref()
    }

    /// Load the WordNet lexicon per `manager`'s `<lexicon>` config
    /// ([`KBManager::load_lexicon`]) and install it -- the session-level
    /// entry point for [`KBManager::load_lexicons`]. Best-effort: a failed
    /// or disabled load clears any previously installed lexicon rather than
    /// leaving a stale one in place, matching `KBManager::load_lexicon`'s
    /// own graceful-degradation contract (never an error).
    pub fn load_lexicon(&mut self, manager: &KBManager) {
        self.lexicon = manager.load_lexicon().map(Arc::new);
    }
}

#[cfg(all(test, feature = "native-prover"))]
mod tests {
    use sigmakee_rs_core::ProverLayer;

    use super::*;
    use crate::manager::{LexiconConfig, LexiconSource};

    #[test]
    fn set_lexicon_installs_and_clears() {
        let mut s = Session::<ProverLayer>::new("t".into());
        assert!(s.lexicon().is_none());

        let wn = Arc::new(WordNet::from_texts(
            [(
                "02084071 05 n 01 dog 0 001 @ 02083346 n 0000 | a dog &%Canine+\n",
                sigmakee_rs_core::lexicon::Pos::Noun,
            )],
            None,
            None,
        ));
        s.set_lexicon(Some(wn.clone()));
        assert!(s.lexicon().is_some());

        s.set_lexicon(None);
        assert!(s.lexicon().is_none());
    }

    #[test]
    fn load_lexicon_from_manager_disabled_is_a_noop() {
        let mut s = Session::<ProverLayer>::new("t".into());
        let mut manager = KBManager::default();
        manager.load_lexicons = false;
        s.load_lexicon(&manager);
        assert!(s.lexicon().is_none());
    }

    #[test]
    fn search_surfaces_wordnet_hits_once_a_lexicon_is_installed() {
        use crate::{SearchOpts, Source};

        let mut s = Session::<ProverLayer>::new("t".into());
        s.ingest(
            Source::Reader {
                name: "t.kif".into(),
                reader: Box::new(std::io::Cursor::new(Vec::from(
                    "(documentation Canine EnglishLanguage \"A carnivorous mammal.\")",
                ))),
            },
            true,
        );

        let plain = s.search("dog", &SearchOpts::default()).unwrap();
        assert!(
            plain.is_empty(),
            "no lexicon installed -> no hits, got {plain:?}"
        );

        let wn = Arc::new(WordNet::from_texts(
            [(
                "02084071 05 n 01 dog 0 001 @ 02083346 n 0000 | a dog &%Canine+\n",
                sigmakee_rs_core::lexicon::Pos::Noun,
            )],
            None,
            None,
        ));
        s.set_lexicon(Some(wn));

        // Even a caller who forgets to set `opts.lexicon` still gets WordNet
        // hits -- `Session::search` overrides it from the installed lexicon.
        let hits = s.search("dog", &SearchOpts::default()).unwrap();
        assert!(
            hits.iter().any(|h| h.symbol == "Canine"),
            "expected a WordNet-sourced Canine hit, got {:?}",
            hits.iter().map(|h| &h.symbol).collect::<Vec<_>>()
        );
    }

    #[test]
    fn load_lexicon_from_manager_missing_directory_clears() {
        let mut s = Session::<ProverLayer>::new("t".into());
        let mut manager = KBManager::default();
        manager.lexicon = LexiconConfig {
            source: Some(LexiconSource::Local {
                path: "/does/not/exist".into(),
            }),
            ..Default::default()
        };
        s.load_lexicon(&manager);
        assert!(
            s.lexicon().is_none(),
            "a missing lexicon source must clear, not panic or error"
        );
    }
}
