// crates/native/src/cli/debug.rs
//
// `sumo debug <FILE>` — consistency-check a single loaded KIF file
// against the rest of the knowledge base via Vampire.
//
// The pipeline (see `args::Cmd::Debug` for the user-facing description):
//
//   1. Open the shared KB via `open_or_build_kb`.
//   2. Locate `<FILE>` in the KB's loaded tags.
//   3. Sample its root sentences by `thoroughness`.
//   4. Render the sample as a KIF blob and feed it to
//      `KnowledgeBase::sine_select_for_query` at tolerance `scope` —
//      this yields every axiom SInE considers relevant to the sample's
//      symbols.
//   5. Consistency-check the union (sample ∪ SInE-expanded) via
//      `KnowledgeBase::check_consistency` (no conjecture, FOF).
//   6. Render the verdict.  On `Inconsistent`, walk the proof and map
//      each axiom-role step back to its source `file:line` via
//      `KnowledgeBase::build_axiom_source_index`.
//   7. List the set of other files SInE pulled axioms from.
//
// Requires the `ask` feature (Vampire, SInE, axiom-source index).

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use inline_colorization::*;
use rand::seq::SliceRandom;

use sigmakee_rs_core::{
    KnowledgeBase, ProverStatus, SineParams, TptpLang, VampireRunner,
};

use crate::cli::args::KbArgs;
use crate::cli::proof::print_proof;
use crate::cli::util::{open_or_build_kb, resolve_vampire_path};

pub fn run_debug(
    file:         PathBuf,
    thoroughness: f32,
    scope:        Option<f32>,
    timeout:      u32,
    keep:         Option<PathBuf>,
    show_proof:   Option<String>,
    kb_args:      KbArgs,
) -> bool {
    // -- Arg validation -----------------------------------------------
    if !(thoroughness > 0.0 && thoroughness <= 1.0) {
        log::error!(
            "--thoroughness must be in (0.0, 1.0]; got {}",
            thoroughness
        );
        return false;
    }

    // -- KB load ------------------------------------------------------
    let vampire_candidate = kb_args
        .vampire
        .clone()
        .unwrap_or_else(|| PathBuf::from("vampire"));

    let kb = match open_or_build_kb(&kb_args) {
        Ok(kb) => kb,
        Err(()) => return false,
    };

    // -- File-tag resolution ------------------------------------------
    // KIF files are tagged by `path.display().to_string()` at load
    // time (see `crates/native/src/cli/util.rs::open_or_build_kb`).
    // The user could have loaded with any of `-f Economy.kif`,
    // `-f ./Economy.kif`, or `-c` (config.xml pulls absolute paths)
    // and then invoked `debug` with a different form.  Resolve in
    // three tiers:
    //
    //   1. Exact string match against loaded tags.
    //   2. Canonicalized absolute path — works when the user passed
    //      a relative `debug <rel>` but loaded absolute (or vice-versa)
    //      AND the file still exists on disk at that relative path.
    //   3. Basename suffix match — scan loaded tags for any ending in
    //      `/<basename>`.  This is the config-mode case: the user
    //      typed `debug Economy.kif`, Sigma's config loaded it as
    //      `/Users/.../sumo/Economy.kif`.  Unambiguous basenames
    //      resolve silently; duplicates list the candidates.
    let tag_primary = file.display().to_string();
    let tag_canonical = file
        .canonicalize()
        .ok()
        .map(|p| p.display().to_string());

    let resolved = resolve_file_tag(&kb, &file, &tag_primary, tag_canonical.as_deref());
    let (tag, sids): (String, Vec<_>) = match resolved {
        Ok(v) => v,
        Err(()) => return false,
    };

    let file_root_count = sids.len();

    // -- Sampling -----------------------------------------------------
    let sample: Vec<_> = if thoroughness >= 1.0 {
        sids.clone()
    } else {
        let mut rng = rand::thread_rng();
        let mut shuffled = sids.clone();
        shuffled.shuffle(&mut rng);
        let take = ((file_root_count as f32) * thoroughness).ceil() as usize;
        shuffled.truncate(take.max(1));
        shuffled
    };
    let sample_count = sample.len();

    log::info!(
        "debug: '{}' → {} root sentences, sampling {} (thoroughness {:.2})",
        tag, file_root_count, sample_count, thoroughness,
    );

    // -- SInE expansion ----------------------------------------------
    // Render the sample as a single KIF blob and feed it to SInE.
    // Going through `sentence_to_string` + re-parse is slightly
    // wasteful compared with a hypothetical `sine_select_from_sids`
    // helper, but it reuses the existing tested code path and the
    // sample is typically small.
    let default_tolerance = SineParams::default().tolerance;
    let tolerance = scope.unwrap_or(default_tolerance);
    let params = SineParams::benevolent(tolerance);

    // Use `sentence_kif_str` (the re-parseable plain-KIF formatter) —
    // `sentence_to_string`'s Sub arm double-wraps nested formulas
    // (emits `(not ((foo)))` instead of `(not (foo))`), which the
    // parser rejects.
    let query_kif: String = sample
        .iter()
        .map(|sid| kb.sentence_kif_str(*sid))
        .collect::<Vec<_>>()
        .join("\n");
    log::debug!(target: "sumo_native::debug",
        "query_kif ({} chars) for SInE:\n{}", query_kif.len(), query_kif);

    // SInE needs `&mut self` to potentially re-tune the D-relation
    // tolerance cache.  Unlock the KB briefly.
    let mut kb = kb;
    let selected = match kb.sine_select_for_query(&query_kif, params) {
        Ok(s) => s,
        Err(e) => {
            log::error!("SInE selection failed: {}", e);
            return false;
        }
    };

    // Union the sample with the SInE-selected axioms.  The sample sids
    // remain valid across the SInE call: they're permanent KB
    // sentences, not the ephemeral `__sine_query__` tag that SInE
    // rolls back internally.
    let mut check_set: HashSet<_> = sample.iter().copied().collect();
    check_set.extend(selected.iter().copied());
    let check_total = check_set.len();

    // -- File pull-in inventory --------------------------------------
    // Every SInE-selected sid whose owning file is *not* `<FILE>` and
    // *not* ephemeral is a file we pulled axioms in from.
    let mut files_pulled: BTreeSet<String> = BTreeSet::new();
    for sid in selected.iter() {
        let Some(sent) = kb.sentence(*sid) else { continue };
        if sent.file == tag { continue; }
        if sent.file.starts_with("__") { continue; }     // ephemeral tags
        files_pulled.insert(sent.file.clone());
    }

    log::info!(
        "debug: check set = {} sentences ({} sampled + {} SInE-expanded across {} other file(s))",
        check_total, sample_count,
        check_total.saturating_sub(sample_count),
        files_pulled.len(),
    );

    // -- Vampire --------------------------------------------------------
    let vampire_path = match resolve_vampire_path(&vampire_candidate) {
        Ok(p)   => p,
        Err(()) => return false,
    };
    let runner = VampireRunner {
        vampire_path,
        timeout_secs: timeout,
        tptp_dump_path: keep,
    };

    let result = kb.check_consistency(&check_set, &runner, TptpLang::Fof);

    // -- Report ----------------------------------------------------------
    print_header(&tag, file_root_count, sample_count, check_total, &files_pulled, tolerance);

    let (verdict, colour) = match result.status {
        ProverStatus::Consistent   => ("Consistent",   color_bright_green),
        ProverStatus::Inconsistent => ("Inconsistent", color_bright_red),
        ProverStatus::Timeout      => ("Timeout",      color_bright_yellow),
        ProverStatus::Unknown      => ("Unknown",      color_bright_yellow),
        // The prover was asked CheckConsistency, so Proved/Disproved
        // shouldn't surface — but render defensively.
        ProverStatus::Proved       => ("Proved (unexpected)",    color_bright_yellow),
        ProverStatus::Disproved    => ("Disproved (unexpected)", color_bright_yellow),
    };
    println!(
        "{style_bold}Verdict:{style_reset} {colour}{}{color_reset}",
        verdict,
    );

    if matches!(result.status, ProverStatus::Inconsistent) {
        print_contradiction(&kb, &result, &check_set);
        // `--proof` is complementary to the contradiction summary: the
        // summary lists *which* axioms the refutation touched, the
        // proof shows *how* they derive false.  Only fire when Vampire
        // actually produced a transcript; otherwise `print_proof`'s
        // empty-proof branch would print a misleading "(none)" right
        // after the summary.
        if let Some(format) = show_proof.as_deref() {
            print_proof(&kb, &result, format);
        }
    } else {
        log::debug!(target: "sumo_native::debug",
            "Vampire raw output:\n{}", result.raw_output);
        // Still honour `--proof` when consistency-checking with no
        // contradiction: Vampire emits a model / saturation transcript
        // in `raw_output` but `proof_kif` is empty, so `print_proof`
        // will say "(none — parser extracted zero steps)".  Keep the
        // dispatch so the user sees an explicit message rather than
        // wondering if the flag was silently dropped.
        if let Some(format) = show_proof.as_deref() {
            print_proof(&kb, &result, format);
        }
    }

    // -- Exit status ---------------------------------------------------
    // Inconsistent → false (exit 1) so CI gates can fail on contradictions.
    // Timeout / Unknown → false (exit 1) so they don't silently green-light.
    // Consistent → true (exit 0).
    matches!(result.status, ProverStatus::Consistent)
}

// ----------------------------------------------------------------------
// Reporting helpers
// ----------------------------------------------------------------------

fn print_header(
    tag:          &str,
    file_roots:   usize,
    sample:       usize,
    check_total:  usize,
    files_pulled: &BTreeSet<String>,
    tolerance:    f32,
) {
    println!();
    println!("{style_bold}File:{style_reset}          {}", tag);
    println!("{style_bold}Root sentences:{style_reset} {}", file_roots);
    println!(
        "{style_bold}Sampled:{style_reset}       {}  ({:.1}%)",
        sample,
        100.0 * (sample as f32) / (file_roots.max(1) as f32),
    );
    println!(
        "{style_bold}SInE-expanded:{style_reset} {}  (tolerance {:.2})",
        check_total.saturating_sub(sample), tolerance,
    );
    println!(
        "{style_bold}Total checked:{style_reset} {}",
        check_total,
    );

    if files_pulled.is_empty() {
        println!(
            "{style_bold}Files pulled:{style_reset}  {color_bright_black}(none — self-contained){color_reset}",
        );
    } else {
        println!("{style_bold}Files pulled:{style_reset}");
        for f in files_pulled {
            println!("  {color_cyan}{}{color_reset}", f);
        }
    }
    println!();
}

/// Render the contradiction: which axiom-role steps in the Vampire
/// refutation came directly from the KB, and where they live.
fn print_contradiction(
    kb:        &KnowledgeBase,
    result:    &sigmakee_rs_core::ProverResult,
    check_set: &HashSet<sigmakee_rs_core::SentenceId>,
) {
    // `ContradictoryAxioms` sometimes fires before Vampire emits an
    // SZS proof section, in which case `proof_kif` is empty.  Be
    // explicit about that rather than silently claiming "no axioms
    // found" — the contradiction is real, we just don't have a
    // traceback.
    if result.proof_kif.is_empty() {
        println!(
            "{color_bright_red}Contradiction detected{color_reset} — \
             but Vampire emitted no proof transcript we can parse.",
        );
        println!(
            "  (Common on `SZS status ContradictoryAxioms` — the conflict \
             was found during preprocessing, before proof search.)",
        );
        println!(
            "  Re-run with `-v` to see the raw prover output.",
        );
        print_check_set_files(kb, check_set);
        return;
    }

    // Build the canonical-fingerprint index once — it backs both the
    // sid-keyed direct path (O(1), preferred) and the canonical-hash
    // fallback (O(1) average, tolerant of alpha-renaming but not of
    // CNF shape drift).  Same two-strategy dispatch as
    // `crate::cli::proof::print_step_source` — keep the two in sync.
    let src_idx = kb.build_axiom_source_index();
    println!(
        "{color_bright_red}Contradiction detected.{color_reset}  \
         Axioms contributing to the refutation:",
    );

    let mut shown = 0;
    let mut seen_sids: HashSet<sigmakee_rs_core::SentenceId> = HashSet::new();
    for step in &result.proof_kif {
        if step.rule != "axiom" { continue; }

        // Collect every source this step resolves to.  The sid path
        // gives at most one (sids are unique); the hash path gives
        // one or more (cross-file duplicates).  Both paths skip
        // ephemeral files.
        let mut resolved: Vec<&sigmakee_rs_core::AxiomSource> = Vec::new();

        // Strategy 1 — direct sid lookup.  Survives CNF transforms
        // and quantifier-normalisation because it's keyed on the
        // preserved `kb_<sid>` name, not the formula shape.
        if let Some(sid) = step.source_sid {
            if let Some(src) = src_idx.lookup_by_sid(sid) {
                if !src.file.starts_with("__") {
                    resolved.push(src);
                }
            }
        }

        // Strategy 2 — canonical-hash fallback.  Only consulted when
        // the sid path produced nothing (no preserved name, or the
        // sid is no longer in the KB).  Adds any sources not already
        // covered so duplicates-across-files (rare but real in SUMO)
        // still surface.
        if resolved.is_empty() {
            for src in src_idx.lookup(&step.formula) {
                if !src.file.starts_with("__") {
                    resolved.push(src);
                }
            }
        }

        for src in resolved {
            if !seen_sids.insert(src.sid) { continue; }
            // Source trace on its own line, pretty-printed sentence
            // indented below — matches the `--proof kif` layout and
            // the man-page REFERENCES section.
            println!(
                "  {color_bright_black}{}:{}{color_reset}",
                src.file, src.line,
            );
            let pretty = kb.pretty_print_sentence(src.sid, 4);
            for line in pretty.lines() {
                println!("    {}", line);
            }
            shown += 1;
        }
    }
    if shown == 0 {
        println!(
            "  {color_bright_black}(no axiom-role steps in the transcript matched KB sources){color_reset}",
        );
        print_check_set_files(kb, check_set);
    }
}

/// Fallback inventory: when the proof transcript gives us nothing
/// traceable, at least tell the user which *files* contributed to the
/// check set — that narrows the hunt.
fn print_check_set_files(kb: &KnowledgeBase, check_set: &HashSet<sigmakee_rs_core::SentenceId>) {
    let mut files: BTreeSet<String> = BTreeSet::new();
    for sid in check_set {
        if let Some(s) = kb.sentence(*sid) {
            if !s.file.starts_with("__") {
                files.insert(s.file.clone());
            }
        }
    }
    if files.is_empty() { return; }
    println!(
        "  {color_bright_black}Files in the contradictory check set:{color_reset}",
    );
    for f in &files {
        println!("    - {}", f);
    }
}

/// Resolve the user-supplied `<FILE>` argument to a loaded KB tag.
///
/// Tries three strategies, stopping at the first hit:
///
///   1. Exact match of the raw input string against a loaded tag.
///   2. Canonicalized absolute-path match (`file.canonicalize()` — only
///      succeeds if the path exists on disk).
///   3. Basename suffix match: scan every loaded tag for one whose
///      last path component equals `<FILE>`'s last path component.
///      This is the common config-mode case where SigmaKEE loads the
///      file as an absolute path but the user invokes `debug` with
///      just the filename.
///
/// Returns `Ok((tag, sids))` with the resolved tag and its root
/// sentence ids on success.  On failure, logs an error and a short
/// inventory of candidates (the basename-matching tags if any exist,
/// else an "nothing loaded matches" note) and returns `Err(())`.
pub(crate) fn resolve_file_tag(
    kb:          &KnowledgeBase,
    file:        &std::path::Path,
    tag_primary: &str,
    tag_canonical: Option<&str>,
) -> Result<(String, Vec<sigmakee_rs_core::SentenceId>), ()> {
    // 1. Exact match.
    let roots_primary = kb.file_roots(tag_primary);
    if !roots_primary.is_empty() {
        return Ok((tag_primary.to_string(), roots_primary.to_vec()));
    }

    // 2. Canonicalized match.
    if let Some(canon) = tag_canonical {
        let roots_canon = kb.file_roots(canon);
        if !roots_canon.is_empty() {
            log::info!("resolved '{}' → '{}' (canonicalized)", tag_primary, canon);
            return Ok((canon.to_string(), roots_canon.to_vec()));
        }
    }

    // 3. Basename suffix match.
    let needle: Option<std::ffi::OsString> = file
        .file_name()
        .map(std::ffi::OsStr::to_os_string);
    let needle_str: Option<String> = needle
        .as_ref()
        .map(|n| n.to_string_lossy().into_owned());

    let mut basename_hits: Vec<&str> = Vec::new();
    if let Some(ref n) = needle_str {
        let slash_prefixed = format!("/{}", n);
        for loaded in kb.iter_files() {
            if loaded.starts_with("__") { continue; }
            // Either the whole tag equals the basename (rare: load with no path), or
            // it ends in `/<basename>` (the common case).
            if loaded == *n || loaded.ends_with(&slash_prefixed) {
                basename_hits.push(loaded);
            }
        }
    }

    match basename_hits.len() {
        1 => {
            let hit = basename_hits[0].to_string();
            log::info!("resolved '{}' → '{}' (basename match)", tag_primary, hit);
            let sids = kb.file_roots(&hit).to_vec();
            Ok((hit, sids))
        }
        0 => {
            log::error!(
                "file '{}' is not loaded in the KB (tag '{}')",
                file.display(), tag_primary,
            );
            if let Some(n) = &needle_str {
                let substring_hits: Vec<&str> = kb
                    .iter_files()
                    .filter(|t| !t.starts_with("__") && t.contains(n.as_str()))
                    .collect();
                if !substring_hits.is_empty() {
                    log::info!(
                        "loaded files whose path contains '{}':",
                        n,
                    );
                    let mut sorted = substring_hits;
                    sorted.sort_unstable();
                    for h in sorted.iter().take(10) {
                        log::info!("  - {}", h);
                    }
                    if sorted.len() > 10 {
                        log::info!("  ... and {} more", sorted.len() - 10);
                    }
                } else {
                    log::info!(
                        "no loaded file path contains '{}'.  Run `-v` with any other \
                         subcommand to see the full list of loaded files.",
                        n,
                    );
                }
            }
            Err(())
        }
        _ => {
            log::error!(
                "file '{}' is ambiguous — multiple loaded tags end in '/{}':",
                file.display(),
                needle_str.as_deref().unwrap_or(tag_primary),
            );
            let mut sorted = basename_hits;
            sorted.sort_unstable();
            for h in &sorted {
                log::error!("  - {}", h);
            }
            log::error!("  pass the full path to disambiguate.");
            Err(())
        }
    }
}
