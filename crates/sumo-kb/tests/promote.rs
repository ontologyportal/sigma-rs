/// Integration tests: promote_assertions_unchecked() + LMDB persistence.
///
/// These tests require the `persist` feature.
#[cfg(feature = "persist")]
mod tests {
    use sumo_kb::{KnowledgeBase, TellWarning};
    use std::path::Path;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("sumo-kb-test-{}-{}", name, std::process::id()));
        p
    }

    fn cleanup(p: &Path) {
        let _ = std::fs::remove_dir_all(p);
    }

    #[test]
    fn promote_then_reopen_loads_axioms() {
        let dir = tmp_dir("promote-reopen");
        cleanup(&dir);

        // --- Build and promote ---
        {
            let mut kb = KnowledgeBase::open(&dir).expect("open new DB");
            let r = kb.tell("s1", "(instance Dog Animal)");
            assert!(r.ok, "{:?}", r.errors);

            let report = kb.promote_assertions_unchecked("s1")
                .expect("promote should succeed");
            assert_eq!(report.promoted.len(), 1, "one sentence promoted");
            assert!(report.duplicates_removed.is_empty());
        }

        // --- Reopen and verify axiom is present ---
        {
            let mut kb = KnowledgeBase::open(&dir).expect("reopen DB");
            // Tell the same formula — should now warn DuplicateAxiom (loaded from DB)
            let r = kb.tell("s1", "(instance Dog Animal)");
            assert!(r.ok);
            let dup_axiom: Vec<_> = r.warnings.iter()
                .filter(|w| matches!(w, TellWarning::DuplicateAxiom { .. }))
                .collect();
            assert_eq!(dup_axiom.len(), 1,
                "after reopen, same formula should warn DuplicateAxiom; warnings={:?}", r.warnings);
        }

        cleanup(&dir);
    }

    #[test]
    fn promote_deduplicates_against_existing_axioms() {
        let dir = tmp_dir("promote-dedup-axioms");
        cleanup(&dir);

        // First round: promote formula A
        {
            let mut kb = KnowledgeBase::open(&dir).expect("open DB");
            let r = kb.tell("s1", "(subclass Dog Animal)");
            assert!(r.ok);
            kb.promote_assertions_unchecked("s1").expect("promote");
        }

        // Second round: tell the SAME formula + a new one → only new one is promoted
        {
            let mut kb = KnowledgeBase::open(&dir).expect("reopen DB");
            let r1 = kb.tell("s2", "(subclass Dog Animal)");  // dup of DB axiom
            assert!(r1.ok);
            let r2 = kb.tell("s2", "(subclass Cat Animal)");  // new
            assert!(r2.ok);

            let report = kb.promote_assertions_unchecked("s2")
                .expect("promote");
            // Cat was new; Dog was a dup of an axiom
            assert_eq!(report.promoted.len(), 1, "only Cat should be promoted; report={:?}", report);
            assert_eq!(report.duplicates_removed.len(), 0,
                "Dog dup was already removed at tell() time via DuplicateAxiom warning");
        }

        cleanup(&dir);
    }

    #[test]
    fn promote_empty_session_is_noop() {
        let dir = tmp_dir("promote-empty");
        cleanup(&dir);

        {
            let mut kb = KnowledgeBase::open(&dir).expect("open DB");
            let report = kb.promote_assertions_unchecked("nonexistent")
                .expect("promote empty session");
            assert!(report.promoted.is_empty());
            assert!(report.duplicates_removed.is_empty());
        }

        cleanup(&dir);
    }

    #[test]
    fn multiple_formulas_survive_round_trip() {
        let dir = tmp_dir("promote-multi");
        cleanup(&dir);

        let formulas = [
            "(subclass Dog Animal)",
            "(subclass Cat Animal)",
            "(instance Fido Dog)",
            "(instance Whiskers Cat)",
        ];

        {
            let mut kb = KnowledgeBase::open(&dir).expect("open DB");
            for f in &formulas {
                let r = kb.tell("batch", f);
                assert!(r.ok, "tell failed for '{}': {:?}", f, r.errors);
            }
            let report = kb.promote_assertions_unchecked("batch")
                .expect("promote");
            assert_eq!(report.promoted.len(), formulas.len(),
                "all formulas should be promoted");
        }

        // After reopen, all formulas should be recognised as axiom duplicates
        {
            let mut kb = KnowledgeBase::open(&dir).expect("reopen DB");
            let mut dup_count = 0usize;
            for f in &formulas {
                let r = kb.tell("check", f);
                assert!(r.ok);
                dup_count += r.warnings.iter()
                    .filter(|w| matches!(w, TellWarning::DuplicateAxiom { .. }))
                    .count();
            }
            assert_eq!(dup_count, formulas.len(),
                "all {} formulas should be DuplicateAxiom on reopen", formulas.len());
        }

        cleanup(&dir);
    }
}
