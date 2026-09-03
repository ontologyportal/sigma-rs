//! End-to-end: `sumo search` surfaces WordNet synonym hits once a
//! config.xml `<lexicon>` section names a real WordNetMappings directory --
//! the automatic SDK/CLI path (`KBManager::load_lexicon` ->
//! `Session::load_lexicon`, wired in `ingest_constituents`), no per-call
//! plumbing in the search command itself.
//!
//! Ignored by default: requires a local WordNetMappings directory. Defaults
//! to `~/projects/sumo/WordNetMappings`; override with `SIGMA_WN_DIR`.

use std::io::Write;
use std::process::Command;

#[test]
#[ignore = "requires a local WordNetMappings directory"]
fn search_surfaces_a_wordnet_synonym_hit() {
    let wn_dir = std::env::var("SIGMA_WN_DIR").unwrap_or_else(|_| {
        let home = std::env::var("HOME").expect("HOME set");
        format!("{home}/projects/sumo/WordNetMappings")
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let kif_path = tmp.path().join("test.kif");
    std::fs::write(
        &kif_path,
        "(instance Automobile SetOrClass)\n\
         (subclass Automobile Vehicle)\n\
         (documentation Automobile EnglishLanguage \"A wheeled motor vehicle used for transportation.\")\n",
    )
    .expect("write test.kif");

    let config_path = tmp.path().join("config.xml");
    let mut f = std::fs::File::create(&config_path).expect("create config.xml");
    write!(
        f,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<configuration>
  <preference name="sumokbname" value="TEST"/>
  <preference name="kbDir" value="{kb_dir}"/>
  <lexicon>
    <preference name="source.type" value="local"/>
    <preference name="source.path" value="{wn_dir}"/>
  </lexicon>
  <kb name="TEST"><constituent filename="test.kif"/></kb>
</configuration>"#,
        kb_dir = tmp.path().display(),
    )
    .expect("write config.xml");

    let out = Command::new(env!("CARGO_BIN_EXE_sumo"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--no-db",
            "-c",
            "search",
            "car",
        ])
        .output()
        .expect("run sumo search");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Automobile") && stdout.contains("wn"),
        "expected a WordNet-sourced Automobile hit in stdout, got:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
