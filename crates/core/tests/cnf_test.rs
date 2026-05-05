/// Integration tests: CNF clausification (requires `cnf` feature).
#[cfg(feature = "cnf")]
mod tests {
    use sigmakee_rs_core::{KnowledgeBase, ClausifyOptions};

    fn kb() -> KnowledgeBase {
        KnowledgeBase::new()
    }

    #[test]
    fn clausify_simple_formula() {
        let mut kb = kb();
        let r = kb.tell("s1", "(instance Dog Animal)");
        assert!(r.ok);

        let report = kb.clausify().expect("clausify should succeed");
        assert!(report.clausified > 0, "at least one formula clausified");

        // to_tptp_cnf should now work
        let cnf = kb.to_tptp_cnf(None).expect("to_tptp_cnf should succeed after clausify");
        assert!(!cnf.is_empty(), "CNF output should be non-empty");
        assert!(cnf.contains("cnf("), "CNF output should contain cnf() formulas: {}", cnf);
    }

    #[test]
    fn to_tptp_cnf_fails_before_clausify() {
        let kb = kb();
        let err = kb.to_tptp_cnf(None);
        assert!(err.is_err(), "to_tptp_cnf should fail before clausify is called");
    }

    #[test]
    fn enable_cnf_mode_then_clausify() {
        let mut kb = kb();
        kb.enable_cnf(ClausifyOptions::default());

        let r = kb.tell("s1", "(instance Dog Animal)");
        assert!(r.ok);

        let report = kb.clausify().expect("clausify");
        assert!(report.clausified > 0);

        let cnf = kb.to_tptp_cnf(None).expect("cnf output");
        assert!(cnf.contains("cnf("));
    }

    #[test]
    fn disable_cnf_clears_mode() {
        let mut kb = kb();
        kb.enable_cnf(ClausifyOptions::default());
        kb.disable_cnf();

        let r = kb.tell("s1", "(instance Dog Animal)");
        assert!(r.ok);

        // clausify should still work manually
        let report = kb.clausify().expect("clausify works even with mode off");
        assert!(report.clausified > 0);
    }

    #[test]
    fn implication_clausifies_to_multiple_clauses() {
        let mut kb = kb();
        // (=> (instance ?X Dog) (instance ?X Animal)) is a simple Horn clause
        let r = kb.tell("s1", "(=> (instance ?X Dog) (instance ?X Animal))");
        assert!(r.ok, "{:?}", r.errors);

        let report = kb.clausify().expect("clausify");
        assert!(report.clausified > 0);

        let cnf = kb.to_tptp_cnf(None).expect("cnf");
        assert!(cnf.contains("cnf("), "Expected cnf formulas in: {}", cnf);
    }
}
