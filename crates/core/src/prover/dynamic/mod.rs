//! Dynamic relations: resolving atoms with embedded JavaScript during native
//! proof search.
//!
//! The registry ([`DynamicRegistry`]) says which relations are dynamic and
//! when they fire; this module runs their functions.  [`JsOracle`] owns a
//! QuickJS executor thread with one context per `#|@js |#` script, and
//! answers [`DynamicOracle::call`] with a per-ask memo, a per-call timeout,
//! and a per-ask call budget.  The saturation prover consumes it through
//! [`DynamicOracle`] only.
//!
//! [`DynamicRegistry`]: crate::semantics::caches::dynamic_relations::DynamicRegistry

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

use crate::semantics::caches::dynamic_relations::DynamicRegistry;

mod js;
mod native;

pub(crate) use native::library_path;

/// A ground KIF value passed to or returned by a dynamic function.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DynValue {
    /// A SUMO symbol (`kif.Name` on the JS side).
    Sym(String),
    /// A string literal's content, without quotes.
    Str(String),
    /// A numeral.
    Num(f64),
}

impl DynValue {
    /// A stable rendering for memo keys.
    pub(crate) fn key(&self) -> String {
        match self {
            DynValue::Sym(s) => format!("s:{s}"),
            DynValue::Str(s) => format!("q:{s}"),
            DynValue::Num(n) => format!("n:{}", crate::numeric::format_num(*n)),
        }
    }
}

/// One solution: a value per relation position, `None` = the caller's value.
pub(crate) type Solution = Vec<Option<DynValue>>;

/// The result of one dynamic call.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DynOutcome {
    /// Zero or more solutions.  Empty means "no answer", never "false".
    Answers(Vec<Solution>),
    /// The call did not complete (error, timeout, budget); the search must
    /// not treat its saturation as complete.
    Failed(String),
}

/// What the saturation prover calls to resolve a dynamic atom.
pub(crate) trait DynamicOracle: Send + Sync + std::fmt::Debug {
    /// Call the function exported for relation `function` with one argument
    /// per relation position (`None` = unbound).
    fn call(&self, function: &str, args: &[Option<DynValue>]) -> DynOutcome;

    /// Whether any call so far failed, timed out, or was refused by the
    /// budget -- i.e. whether the dynamic facts seen are possibly incomplete.
    fn incomplete(&self) -> bool;

    /// The distinct problems met so far, in order: why calls failed, and
    /// libraries of called scripts that could not be loaded (even when the
    /// script caught the error).
    fn problems(&self) -> Vec<String>;
}

/// Limits and library location for a [`JsOracle`].
#[derive(Debug, Clone)]
pub struct DynamicOpts {
    /// Directory the `//@use` libraries resolve in (`libDir` in config.xml).
    /// `None`: scripts that declare a library fail to load it.
    pub lib_dir: Option<PathBuf>,
    /// Wall-clock limit per call.
    pub call_ms: u64,
    /// Calls allowed per ask (distinct memo misses).
    pub max_calls: usize,
}

impl Default for DynamicOpts {
    fn default() -> Self {
        Self {
            lib_dir: None,
            call_ms: 2_000,
            max_calls: 1_000,
        }
    }
}

/// A [`DynamicOracle`] backed by a QuickJS executor thread.
pub(crate) struct JsOracle {
    requests: Mutex<mpsc::Sender<js::Request>>,
    memo: Mutex<HashMap<String, DynOutcome>>,
    calls: AtomicUsize,
    incomplete: AtomicBool,
    problems: Mutex<Vec<String>>,
    opts: DynamicOpts,
}

impl std::fmt::Debug for JsOracle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsOracle")
            .field("calls", &self.calls.load(Ordering::Relaxed))
            .field("incomplete", &self.incomplete.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl JsOracle {
    /// Start an executor for every script in `registry`.  `None` when the
    /// registry has no callable relation (nothing to run).
    pub(crate) fn start(registry: &DynamicRegistry, opts: DynamicOpts) -> Option<Self> {
        if registry.relations.is_empty() {
            return None;
        }
        let functions: HashMap<String, usize> = registry
            .relations
            .values()
            .map(|r| (r.name.clone(), r.script))
            .collect();
        let scripts = registry.scripts.iter().map(|(_, s)| s.clone()).collect();
        let requests = js::spawn(scripts, functions, opts.lib_dir.clone());
        Some(Self {
            requests: Mutex::new(requests),
            memo: Mutex::new(HashMap::new()),
            calls: AtomicUsize::new(0),
            incomplete: AtomicBool::new(false),
            problems: Mutex::new(Vec::new()),
            opts,
        })
    }

    fn fail(&self, why: impl Into<String>) -> DynOutcome {
        let why = why.into();
        self.incomplete.store(true, Ordering::Relaxed);
        self.note(std::slice::from_ref(&why));
        DynOutcome::Failed(why)
    }

    fn note(&self, problems: &[String]) {
        if let Ok(mut seen) = self.problems.lock() {
            for p in problems {
                if !seen.contains(p) {
                    seen.push(p.clone());
                }
            }
        }
    }
}

impl DynamicOracle for JsOracle {
    fn call(&self, function: &str, args: &[Option<DynValue>]) -> DynOutcome {
        let key = std::iter::once(function.to_string())
            .chain(
                args.iter()
                    .map(|a| a.as_ref().map_or_else(|| "_".to_string(), DynValue::key)),
            )
            .collect::<Vec<_>>()
            .join("\u{1f}");
        if let Some(hit) = self.memo.lock().ok().and_then(|m| m.get(&key).cloned()) {
            return hit;
        }
        if self.calls.fetch_add(1, Ordering::Relaxed) >= self.opts.max_calls {
            return self.fail("dynamic call budget exhausted");
        }
        let (reply, answer) = mpsc::channel();
        let deadline = Duration::from_millis(self.opts.call_ms);
        let sent = self.requests.lock().ok().is_some_and(|tx| {
            tx.send(js::Request {
                function: function.to_string(),
                args: args.to_vec(),
                timeout: deadline,
                reply,
            })
            .is_ok()
        });
        if !sent {
            return self.fail("dynamic executor is not running");
        }
        let outcome = match answer.recv_timeout(deadline + Duration::from_millis(50)) {
            Ok(reply) => {
                // A failure caused by a load error already names it.
                let why = match &reply.outcome {
                    DynOutcome::Failed(why) => why.as_str(),
                    DynOutcome::Answers(_) => "",
                };
                let fresh: Vec<String> = reply
                    .warnings
                    .into_iter()
                    .filter(|w| !why.contains(w.as_str()))
                    .collect();
                self.note(&fresh);
                reply.outcome
            }
            Err(_) => DynOutcome::Failed(format!("{function}: timed out")),
        };
        if let DynOutcome::Failed(why) = &outcome {
            self.incomplete.store(true, Ordering::Relaxed);
            self.note(std::slice::from_ref(why));
        }
        if let Ok(mut memo) = self.memo.lock() {
            memo.insert(key, outcome.clone());
        }
        outcome
    }

    fn incomplete(&self) -> bool {
        self.incomplete.load(Ordering::Relaxed)
    }

    fn problems(&self) -> Vec<String> {
        self.problems.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantics::caches::test_support::kif_layer;

    const CLASSES: &str = "
        (subclass DynamicRelation Relation)
        (subclass PartialDynamicRelation DynamicRelation)
        (instance weather PartialDynamicRelation)
    ";

    fn oracle(script: &str, opts: DynamicOpts) -> JsOracle {
        let layer = kif_layer(&format!("{CLASSES}#|@js\n{script}\n|#"));
        let registry = layer.dynamic_relations();
        assert!(registry.problems.is_empty(), "{:?}", registry.problems);
        JsOracle::start(&registry, opts).expect("callable relation")
    }

    fn sym(s: &str) -> Option<DynValue> {
        Some(DynValue::Sym(s.into()))
    }

    #[test]
    fn returns_symbols_strings_and_numbers() {
        let o = oracle(
            r#"export function weather(x, loc, day) {
                 if (loc.__kif_symbol !== "Boston") return [];
                 return [[kif.Rain, null, null], ["drizzle", null, 2.5]];
               }"#,
            DynamicOpts::default(),
        );
        let out = o.call("weather", &[None, sym("Boston"), Some(DynValue::Num(18.0))]);
        assert_eq!(
            out,
            DynOutcome::Answers(vec![
                vec![sym("Rain"), None, None],
                vec![
                    Some(DynValue::Str("drizzle".into())),
                    None,
                    Some(DynValue::Num(2.5))
                ],
            ])
        );
        assert_eq!(
            o.call("weather", &[None, sym("Paris"), None]),
            DynOutcome::Answers(vec![])
        );
        assert!(!o.incomplete());
    }

    #[test]
    fn memoizes_identical_calls() {
        let o = oracle(
            "let n = 0; export function weather(a, b, c) { n += 1; return [[n, null, null]]; }",
            DynamicOpts::default(),
        );
        let args = [None, sym("Boston"), None];
        let first = o.call("weather", &args);
        assert_eq!(o.call("weather", &args), first);
        assert_eq!(o.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn bad_returns_and_throws_fail_and_mark_incomplete() {
        let o = oracle(
            r#"export function weather(a, b, c) {
                 if (b.__kif_symbol === "Throw") throw new Error("boom");
                 return [[1, 2]];
               }"#,
            DynamicOpts::default(),
        );
        assert!(
            matches!(o.call("weather", &[None, sym("X"), None]), DynOutcome::Failed(m) if m.contains("3 positions"))
        );
        assert!(
            matches!(o.call("weather", &[None, sym("Throw"), None]), DynOutcome::Failed(m) if m.contains("boom"))
        );
        assert!(o.incomplete());
    }

    #[test]
    fn runaway_js_is_interrupted() {
        let o = oracle(
            "export function weather(a, b, c) { for (;;) {} }",
            DynamicOpts {
                call_ms: 100,
                ..DynamicOpts::default()
            },
        );
        let start = std::time::Instant::now();
        assert!(matches!(
            o.call("weather", &[None, sym("X"), None]),
            DynOutcome::Failed(_)
        ));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(o.incomplete());
    }

    #[test]
    fn budget_refuses_calls_past_the_cap() {
        let o = oracle(
            "export function weather(a, b, c) { return []; }",
            DynamicOpts {
                max_calls: 1,
                ..DynamicOpts::default()
            },
        );
        assert_eq!(
            o.call("weather", &[None, sym("A"), None]),
            DynOutcome::Answers(vec![])
        );
        assert!(matches!(
            o.call("weather", &[None, sym("B"), None]),
            DynOutcome::Failed(_)
        ));
        assert!(o.incomplete());
    }

    #[test]
    fn no_io_globals_are_exposed() {
        let o = oracle(
            r#"export function weather(a, b, c) {
                 return [[typeof fetch + typeof Date + typeof eval + typeof JSON, null, null]];
               }"#,
            DynamicOpts::default(),
        );
        assert_eq!(
            o.call("weather", &[None, sym("A"), None]),
            DynOutcome::Answers(vec![vec![
                Some(DynValue::Str("undefinedundefinedundefinedobject".into())),
                None,
                None
            ]])
        );
    }

    /// Compile a C shared library into a temp dir; `None` without a C compiler.
    fn c_library(name: &str, source: &str) -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().ok()?;
        let src = dir.path().join(format!("{name}.c"));
        std::fs::write(&src, source).ok()?;
        let ext = if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        };
        let out = dir.path().join(format!("lib{name}.{ext}"));
        let status = std::process::Command::new("cc")
            .args(["-shared", "-fPIC", "-o"])
            .arg(&out)
            .arg(&src)
            .status()
            .ok()?;
        status.success().then_some(dir)
    }

    #[test]
    fn calls_native_functions_with_int_float_and_buffer_strings() {
        let Some(dir) = c_library(
            "sensor",
            r#"
            #include <stdint.h>
            #include <string.h>
            #include <stddef.h>
            int64_t twice(int64_t x) { return 2 * x; }
            double half(double x) { return x / 2.0; }
            int64_t label(const char *place, char *out, size_t cap) {
                const char *prefix = "sky-over-";
                size_t need = strlen(prefix) + strlen(place);
                if (need > cap) return -(int64_t)need;
                memcpy(out, prefix, strlen(prefix));
                memcpy(out + strlen(prefix), place, strlen(place));
                return (int64_t)need;
            }
            int64_t big(char *out, size_t cap) {
                size_t need = 5000;
                if (need > cap) return -(int64_t)need;
                memset(out, 'x', need);
                return (int64_t)need;
            }
            "#,
        ) else {
            eprintln!("skipping: no C compiler");
            return;
        };
        let o = oracle(
            r#"//@use sensor { twice(int) -> int; half(float) -> float; label(string) -> string; big() -> string }
               export function weather(a, b, c) {
                 return [[sensor.twice(21), sensor.half(5), sensor.label(b.__kif_symbol)],
                         [sensor.big().length, null, null]];
               }"#,
            DynamicOpts {
                lib_dir: Some(dir.path().to_path_buf()),
                ..DynamicOpts::default()
            },
        );
        assert_eq!(
            o.call("weather", &[None, sym("Boston"), None]),
            DynOutcome::Answers(vec![
                vec![
                    Some(DynValue::Num(42.0)),
                    Some(DynValue::Num(2.5)),
                    Some(DynValue::Str("sky-over-Boston".into())),
                ],
                vec![Some(DynValue::Num(5000.0)), None, None],
            ])
        );
    }

    #[test]
    fn native_type_mismatch_throws_into_js() {
        let Some(dir) = c_library(
            "ints",
            "#include <stdint.h>\nint64_t inc(int64_t x) { return x + 1; }",
        ) else {
            return;
        };
        let o = oracle(
            r#"//@use ints { inc(int) -> int }
               export function weather(a, b, c) {
                 try { ints.inc(1.5); return []; } catch (e) { return [[String(e.message), null, null]]; }
               }"#,
            DynamicOpts {
                lib_dir: Some(dir.path().to_path_buf()),
                ..DynamicOpts::default()
            },
        );
        let DynOutcome::Answers(rows) = o.call("weather", &[None, sym("A"), None]) else {
            panic!("expected answers");
        };
        assert!(matches!(&rows[0][0], Some(DynValue::Str(m)) if m.contains("expected an integer")));
    }

    #[test]
    fn failures_and_caught_library_errors_are_reported_once() {
        let o = oracle(
            r#"//@use nowhere { f() -> int }
               export function weather(a, b, c) {
                 if (b.__kif_symbol === "Boston") throw new Error("sensor offline");
                 try { nowhere.f(); } catch (e) {}
                 return [];
               }"#,
            DynamicOpts::default(),
        );
        o.call("weather", &[None, sym("Paris"), None]);
        o.call("weather", &[None, sym("Lima"), None]);
        o.call("weather", &[None, sym("Boston"), None]);
        o.call("weather", &[None, sym("Boston"), sym("Today")]);
        assert_eq!(
            o.problems(),
            vec![
                "library `nowhere`: no libDir configured".to_string(),
                "weather: sensor offline".to_string(),
            ]
        );
    }

    #[test]
    fn missing_library_fails_the_call_not_the_script() {
        let o = oracle(
            r#"//@use nowhere { f() -> int }
               export function weather(a, b, c) { return [[nowhere.f(), null, null]]; }"#,
            DynamicOpts::default(),
        );
        assert!(
            matches!(o.call("weather", &[None, sym("A"), None]), DynOutcome::Failed(m) if m.contains("libDir"))
        );
    }
}
