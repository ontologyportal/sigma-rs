// crates/core/src/parse/script.rs
//
// Embedded `#|@js ... |#` code blocks: the lexical half of dynamic relations.
// A block is recognized from a verbatim `CommentBlock` whose text opens with
// the `@js` tag; its `//@use` lines declare the native library functions the
// code may call.  Nothing is evaluated: the code is only compiled, to report
// syntax errors.

use serde::{Deserialize, Serialize};

use crate::parse::doc::CommentBlock;
use crate::parse::Span;

/// Tag opening an embedded JavaScript block: `#|@js`.
const JS_TAG: &str = "@js";
/// Prefix of a library declaration line inside a block.
const USE_PREFIX: &str = "//@use";
/// The global holding KIF symbols (`kif.Name`); no library may take it.
const KIF_GLOBAL: &str = "kif";

/// A value type crossing the native-library boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ValType {
    /// `int`: a C `int64_t`.
    Int,
    /// `float`: a C `double`.
    Float,
    /// `string`: a NUL-terminated `const char *` argument, or a returned
    /// string written into a caller-supplied buffer.
    Str,
}

impl ValType {
    fn parse(word: &str) -> Option<Self> {
        match word {
            "int" => Some(Self::Int),
            "float" => Some(Self::Float),
            "string" => Some(Self::Str),
            _ => None,
        }
    }
}

/// One declared native function: `name(params) -> ret`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct LibFn {
    pub(crate) name: String,
    pub(crate) params: Vec<ValType>,
    pub(crate) ret: ValType,
}

/// One `//@use lib { ... }` declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct LibDecl {
    /// Library name: resolved as `lib<name>.so` / `lib<name>.dylib`.
    pub(crate) name: String,
    pub(crate) functions: Vec<LibFn>,
    /// Line of the declaration within the block (1-based, after the tag).
    #[serde(default)]
    pub(crate) line: usize,
}

/// What is wrong with a script, found without running it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ProblemKind {
    /// A malformed or conflicting `//@use` line.
    BadUse,
    /// The code does not compile.
    Syntax,
}

impl ProblemKind {
    /// The diagnostic code reported for this kind.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::BadUse => "bad-script-use",
            Self::Syntax => "script-syntax",
        }
    }
}

/// One problem in a script, at a line within the block (1-based, after the
/// tag).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ScriptProblem {
    pub(crate) line: usize,
    pub(crate) kind: ProblemKind,
    pub(crate) message: String,
}

/// A parsed `#|@js ... |#` block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Script {
    /// Span of the whole block comment.
    pub(crate) span: Span,
    /// The code after the `@js` tag line, evaluated as an ES module.
    pub(crate) code: String,
    /// Declared native libraries.
    pub(crate) libs: Vec<LibDecl>,
    /// Names of the `export function`s, in source order.
    pub(crate) exports: Vec<String>,
    /// Problems found without running the code.
    pub(crate) errors: Vec<ScriptProblem>,
}

impl Script {
    /// Parse `block` as an embedded script; `None` when it is not one.
    pub(crate) fn parse(block: &CommentBlock) -> Option<Self> {
        if !block.verbatim {
            return None;
        }
        let rest = block.text.trim_start().strip_prefix(JS_TAG)?;
        let (tag_tail, code) = rest.split_once('\n').unwrap_or((rest, ""));
        if !tag_tail.trim().is_empty() {
            return None;
        }
        let mut libs: Vec<LibDecl> = Vec::new();
        let mut errors = Vec::new();
        let bad_use = |line, message| ScriptProblem {
            line,
            kind: ProblemKind::BadUse,
            message,
        };
        for (i, line) in code.lines().enumerate() {
            let Some(decl) = line.trim().strip_prefix(USE_PREFIX) else {
                continue;
            };
            match parse_use(decl) {
                Ok(lib) if lib.name == KIF_GLOBAL => errors.push(bad_use(
                    i + 1,
                    format!("library name `{KIF_GLOBAL}` is reserved"),
                )),
                Ok(lib) if libs.iter().any(|l| l.name == lib.name) => errors.push(bad_use(
                    i + 1,
                    format!("library `{}` is declared twice", lib.name),
                )),
                Ok(lib) => libs.push(LibDecl { line: i + 1, ..lib }),
                Err(msg) => errors.push(bad_use(i + 1, msg)),
            }
        }
        if let Some((line, message)) = syntax_error(code) {
            errors.push(ScriptProblem {
                line,
                kind: ProblemKind::Syntax,
                message,
            });
        }
        Some(Self {
            span: block.span.clone(),
            exports: exported_functions(code),
            code: code.to_string(),
            libs,
            errors,
        })
    }
}

/// Parse the body of a `//@use` line: `lib { f(int, string) -> float; ... }`.
fn parse_use(decl: &str) -> Result<LibDecl, String> {
    let (name, body) = decl
        .split_once('{')
        .ok_or_else(|| "expected `lib { ... }`".to_string())?;
    let name = name.trim();
    if !is_identifier(name) {
        return Err(format!("invalid library name `{name}`"));
    }
    let body = body
        .trim_end()
        .strip_suffix('}')
        .ok_or_else(|| "missing closing `}`".to_string())?;
    let functions = body
        .split(';')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(parse_fn)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(LibDecl {
        name: name.to_string(),
        functions,
        line: 0,
    })
}

/// Parse one `name(types) -> type` signature.
fn parse_fn(sig: &str) -> Result<LibFn, String> {
    let bad = || format!("expected `name(types) -> type`, found `{sig}`");
    let (name, rest) = sig.split_once('(').ok_or_else(bad)?;
    let (params, ret) = rest.split_once(')').ok_or_else(bad)?;
    let ret = ret.trim().strip_prefix("->").ok_or_else(bad)?.trim();
    let name = name.trim();
    if !is_identifier(name) {
        return Err(format!("invalid function name `{name}`"));
    }
    let ty = |w: &str| ValType::parse(w).ok_or_else(|| format!("unknown type `{w}`"));
    let params = params
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(ty)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(LibFn {
        name: name.to_string(),
        params,
        ret: ty(ret)?,
    })
}

/// The first compile error in `code` as an ES module, with its line
/// (1-based), or `None` when it compiles.  The module is never evaluated.
fn syntax_error(code: &str) -> Option<(usize, String)> {
    use rquickjs::{context::intrinsic, Context, Module, Runtime};
    let runtime = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return Some((1, format!("QuickJS runtime: {e}"))),
    };
    let context = match Context::custom::<intrinsic::Eval>(&runtime) {
        Ok(cx) => cx,
        Err(e) => return Some((1, format!("QuickJS context: {e}"))),
    };
    context.with(|ctx| {
        let err = Module::declare(ctx.clone(), "script", code.to_string()).err()?;
        let caught = ctx.catch();
        let Some(ex) = caught.as_exception() else {
            return Some((1, err.to_string()));
        };
        // QuickJS reports the position only in the stack: `at script:LINE:COL`.
        let line = ex
            .stack()
            .and_then(|st| {
                let at = st.split("script:").nth(1)?;
                at.split(':').next()?.parse::<usize>().ok()
            })
            .filter(|n| *n >= 1)
            .unwrap_or(1);
        Some((line, ex.message().unwrap_or_else(|| err.to_string())))
    })
}

/// The names declared by `export function NAME(` in `code`.
fn exported_functions(code: &str) -> Vec<String> {
    const EXPORT: &str = "export function";
    code.match_indices(EXPORT)
        .filter(|(at, _)| {
            code[..*at]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '$'))
        })
        .map(|(at, _)| &code[at + EXPORT.len()..])
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .filter_map(|rest| {
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(text: &str, verbatim: bool) -> CommentBlock {
        CommentBlock {
            text: text.to_string(),
            span: Span::default(),
            verbatim,
        }
    }

    #[test]
    fn parses_tag_uses_and_exports() {
        let s = Script::parse(&block(
            "@js\n//@use weatherlib { forecast(string, int) -> string; temp() -> float }\nexport function weather(x, y, z) {\n  return [];\n}\n",
            true,
        ))
        .expect("script");
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert_eq!(s.exports, vec!["weather"]);
        assert_eq!(
            s.libs,
            vec![LibDecl {
                line: 1,
                name: "weatherlib".into(),
                functions: vec![
                    LibFn {
                        name: "forecast".into(),
                        params: vec![ValType::Str, ValType::Int],
                        ret: ValType::Str,
                    },
                    LibFn {
                        name: "temp".into(),
                        params: vec![],
                        ret: ValType::Float,
                    },
                ],
            }]
        );
    }

    #[test]
    fn non_script_comments_are_not_scripts() {
        assert!(Script::parse(&block("@js\nexport function f() {}", false)).is_none());
        assert!(Script::parse(&block(" just prose ", true)).is_none());
        assert!(Script::parse(&block("@jsx\n", true)).is_none());
        assert!(Script::parse(&block("@js extra words\n", true)).is_none());
    }

    #[test]
    fn malformed_use_lines_are_reported_with_their_line() {
        let s = Script::parse(&block(
            "@js\n\n//@use lib { f(bool) -> int }\n//@use { g() -> int }\n//@use ok { h(int) }\n",
            true,
        ))
        .expect("script");
        let lines: Vec<usize> = s.errors.iter().map(|p| p.line).collect();
        assert_eq!(lines, vec![2, 3, 4]);
        assert!(s.libs.is_empty());
    }

    #[test]
    fn syntax_errors_are_reported_with_their_line() {
        let s = Script::parse(&block(
            "@js\nexport function weather(a) {\n  return [[;\n}\n",
            true,
        ))
        .expect("script");
        assert_eq!(s.errors.len(), 1, "{:?}", s.errors);
        assert_eq!(s.errors[0].kind, ProblemKind::Syntax);
        assert_eq!(s.errors[0].line, 2, "{:?}", s.errors);
    }

    #[test]
    fn valid_code_is_compiled_but_not_run() {
        let s = Script::parse(&block(
            "@js\nthrow new Error('ran');\nexport function f() { return []; }\n",
            true,
        ))
        .expect("script");
        assert!(s.errors.is_empty(), "{:?}", s.errors);
    }

    #[test]
    fn duplicate_and_reserved_libraries_are_rejected() {
        let s = Script::parse(&block(
            "@js\n//@use a { f() -> int }\n//@use a { g() -> int }\n//@use kif { h() -> int }\n",
            true,
        ))
        .expect("script");
        let lines: Vec<usize> = s.errors.iter().map(|p| p.line).collect();
        assert_eq!(lines, vec![2, 3]);
        assert_eq!(s.libs.len(), 1);
    }

    #[test]
    fn library_names_cannot_be_paths() {
        for name in ["../evil", "/abs/lib", "sub/lib", "lib.so", "~home", ""] {
            let s = Script::parse(&block(
                &format!("@js\n//@use {name} {{ f() -> int }}\n"),
                true,
            ))
            .expect("script");
            assert!(s.libs.is_empty(), "accepted `{name}`");
            assert_eq!(s.errors.len(), 1, "`{name}`");
        }
    }
}
