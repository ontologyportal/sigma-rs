//! The QuickJS executor thread behind [`JsOracle`](super::JsOracle).
//!
//! A QuickJS runtime is not `Send`, and the prover's portfolio lanes are
//! threads, so one thread owns the runtime, a context per script, and the
//! loaded libraries; callers talk to it over a channel.  Contexts carry only
//! the language core plus `JSON`, `Proxy` (for `kif`), and `Promise` (module
//! evaluation) -- no I/O of any kind.  The `Eval` intrinsic is the compiler
//! itself, so it is installed, but the global `eval` is deleted.  An interrupt handler aborts JS that
//! runs past its call's deadline; a hung native call cannot be interrupted.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rquickjs::context::intrinsic;
use rquickjs::function::Rest;
use rquickjs::{
    Array, Context, Ctx, Exception, Function, Module, Object, Persistent, Runtime, Value,
};

use super::native::{NativeLib, NativeValue};
use super::{DynOutcome, DynValue, Solution};
use crate::parse::script::{LibDecl, Script, ValType};

/// Property marking a JS object as a KIF symbol.
const SYMBOL_KEY: &str = "__kif_symbol";
/// Installed in every context before the script runs.
const PRELUDE: &str = r#"
delete globalThis.eval;
globalThis.kif = new Proxy({}, {
  get(_target, name) {
    return typeof name === "string" ? Object.freeze({ __kif_symbol: name }) : undefined;
  },
});
"#;
const MEMORY_LIMIT: usize = 64 << 20;
const STACK_LIMIT: usize = 1 << 20;

/// One call for the executor.
pub(super) struct Request {
    pub(super) function: String,
    pub(super) args: Vec<Option<DynValue>>,
    pub(super) timeout: Duration,
    pub(super) reply: mpsc::Sender<DynOutcome>,
}

/// Start the executor thread; dropping every sender stops it.
pub(super) fn spawn(
    scripts: Vec<Script>,
    functions: HashMap<String, usize>,
    lib_dir: Option<PathBuf>,
) -> mpsc::Sender<Request> {
    let (tx, rx) = mpsc::channel::<Request>();
    // A failed spawn drops `rx`, so every later send fails and is reported.
    let _ = std::thread::Builder::new()
        .name("sigma-dynamic".into())
        .spawn(move || run(&scripts, &functions, lib_dir.as_deref(), rx));
    tx
}

fn run(
    scripts: &[Script],
    functions: &HashMap<String, usize>,
    lib_dir: Option<&Path>,
    rx: mpsc::Receiver<Request>,
) {
    let runtime = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            for req in rx {
                let _ = req
                    .reply
                    .send(DynOutcome::Failed(format!("QuickJS runtime: {e}")));
            }
            return;
        }
    };
    runtime.set_memory_limit(MEMORY_LIMIT);
    runtime.set_max_stack_size(STACK_LIMIT);
    let deadline: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
    let watch = Rc::clone(&deadline);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        watch.get().is_some_and(|t| Instant::now() > t)
    })));

    let loaded: Vec<Result<Loaded, String>> = scripts
        .iter()
        .enumerate()
        .map(|(i, s)| Loaded::new(&runtime, i, s, lib_dir))
        .collect();

    for req in rx {
        let outcome = match functions.get(&req.function).map(|&i| &loaded[i]) {
            None => DynOutcome::Failed(format!("no function `{}`", req.function)),
            Some(Err(e)) => DynOutcome::Failed(e.clone()),
            Some(Ok(script)) => {
                deadline.set(Some(Instant::now() + req.timeout));
                let out = script.call(&req.function, &req.args);
                deadline.set(None);
                out
            }
        };
        let _ = req.reply.send(outcome);
    }
}

/// One evaluated script: its context and exported functions.
struct Loaded {
    context: Context,
    exports: HashMap<String, Persistent<Function<'static>>>,
}

impl Loaded {
    fn new(
        runtime: &Runtime,
        index: usize,
        script: &Script,
        lib_dir: Option<&Path>,
    ) -> Result<Self, String> {
        let context = Context::custom::<(
            intrinsic::Eval,
            intrinsic::Json,
            intrinsic::Proxy,
            intrinsic::Promise,
        )>(runtime)
        .map_err(|e| format!("script {index}: {e}"))?;
        let exports = context.with(|ctx| -> Result<_, String> {
            let err = |e: rquickjs::Error| format!("script {index}: {}", describe(&ctx, e));
            ctx.eval::<(), _>(PRELUDE).map_err(err)?;
            for decl in &script.libs {
                install_library(&ctx, decl, lib_dir).map_err(err)?;
            }
            let module =
                Module::declare(ctx.clone(), format!("script{index}"), script.code.clone())
                    .map_err(err)?;
            let (module, done) = module.eval().map_err(err)?;
            done.finish::<()>().map_err(err)?;
            let mut exports = HashMap::new();
            for name in &script.exports {
                let f: Function = module.get(name.as_str()).map_err(err)?;
                exports.insert(name.clone(), Persistent::save(&ctx, f));
            }
            Ok(exports)
        })?;
        Ok(Self { context, exports })
    }

    fn call(&self, function: &str, args: &[Option<DynValue>]) -> DynOutcome {
        let Some(f) = self.exports.get(function) else {
            return DynOutcome::Failed(format!("`{function}` is not exported"));
        };
        self.context.with(|ctx| {
            let run = || -> rquickjs::Result<Result<Vec<Solution>, String>> {
                let f = f.clone().restore(&ctx)?;
                let js_args = args
                    .iter()
                    .map(|a| to_js(&ctx, a.as_ref()))
                    .collect::<rquickjs::Result<Vec<_>>>()?;
                let result: Value = f.call((Rest(js_args),))?;
                Ok(solutions(&result, args.len()))
            };
            match run() {
                Ok(Ok(solutions)) => DynOutcome::Answers(solutions),
                Ok(Err(e)) => DynOutcome::Failed(format!("{function}: {e}")),
                Err(e) => DynOutcome::Failed(format!("{function}: {}", describe(&ctx, e))),
            }
        })
    }
}

/// Render a QuickJS error, pulling the pending exception's message.
fn describe(ctx: &Ctx<'_>, e: rquickjs::Error) -> String {
    if matches!(e, rquickjs::Error::Exception) {
        let caught = ctx.catch();
        if let Some(ex) = caught.as_exception() {
            return ex.message().unwrap_or_else(|| "exception".into());
        }
        if let Ok(s) = caught.get::<String>() {
            return s;
        }
    }
    e.to_string()
}

/// Define `globalThis[decl.name]` with one JS function per declared native
/// function.  A library that cannot be loaded still defines its functions;
/// calling one throws the load error.
fn install_library<'js>(
    ctx: &Ctx<'js>,
    decl: &LibDecl,
    lib_dir: Option<&Path>,
) -> rquickjs::Result<()> {
    let lib: Result<Rc<NativeLib>, String> = match lib_dir {
        Some(dir) => NativeLib::load(dir, decl).map(Rc::new),
        None => Err(format!("library `{}`: no libDir configured", decl.name)),
    };
    let object = Object::new(ctx.clone())?;
    for (i, sig) in decl.functions.iter().enumerate() {
        let lib = lib.clone();
        let params = sig.params.clone();
        let name = sig.name.clone();
        let f = Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<Value<'js>> {
                let lib = lib
                    .as_ref()
                    .map_err(|e| Exception::throw_message(&ctx, e))?;
                if args.0.len() != params.len() {
                    return Err(Exception::throw_type(
                        &ctx,
                        &format!("{name} expects {} argument(s)", params.len()),
                    ));
                }
                let native = args
                    .0
                    .iter()
                    .zip(&params)
                    .map(|(v, t)| to_native(v, *t))
                    .collect::<Result<Vec<_>, String>>()
                    .map_err(|e| Exception::throw_type(&ctx, &format!("{name}: {e}")))?;
                let out = lib.functions[i]
                    .call(&native)
                    .map_err(|e| Exception::throw_message(&ctx, &e))?;
                Ok(match out {
                    NativeValue::Int(i) => Value::new_number(ctx.clone(), i as f64),
                    NativeValue::Float(f) => Value::new_number(ctx.clone(), f),
                    NativeValue::Str(s) => {
                        rquickjs::String::from_str(ctx.clone(), &s)?.into_value()
                    }
                })
            },
        )?;
        object.set(sig.name.as_str(), f)?;
    }
    ctx.globals().set(decl.name.as_str(), object)
}

/// Convert a JS argument to the declared native type.
fn to_native(v: &Value<'_>, t: ValType) -> Result<NativeValue, String> {
    match t {
        ValType::Int => {
            let n = v.as_number().ok_or("expected an integer")?;
            if n.fract() != 0.0 || !n.is_finite() || n.abs() > i64::MAX as f64 {
                return Err(format!("expected an integer, got {n}"));
            }
            Ok(NativeValue::Int(n as i64))
        }
        ValType::Float => v
            .as_number()
            .map(NativeValue::Float)
            .ok_or_else(|| "expected a number".to_string()),
        ValType::Str => v
            .as_string()
            .and_then(|s| s.to_string().ok())
            .map(NativeValue::Str)
            .ok_or_else(|| "expected a string".to_string()),
    }
}

fn to_js<'js>(ctx: &Ctx<'js>, v: Option<&DynValue>) -> rquickjs::Result<Value<'js>> {
    Ok(match v {
        None => Value::new_null(ctx.clone()),
        Some(DynValue::Num(n)) => Value::new_number(ctx.clone(), *n),
        Some(DynValue::Str(s)) => rquickjs::String::from_str(ctx.clone(), s)?.into_value(),
        Some(DynValue::Sym(s)) => {
            let o = Object::new(ctx.clone())?;
            o.set(SYMBOL_KEY, s.as_str())?;
            o.into_value()
        }
    })
}

/// Read a function's return value: an array of solutions, each an array
/// with one entry per relation position.
fn solutions(result: &Value<'_>, arity: usize) -> Result<Vec<Solution>, String> {
    let rows = result
        .as_array()
        .ok_or("must return an array of solutions")?;
    rows.iter::<Value>()
        .map(|row| {
            let row = row.map_err(|e| e.to_string())?;
            let row: &Array = row.as_array().ok_or("each solution must be an array")?;
            if row.len() != arity {
                return Err(format!(
                    "a solution has {} entries, the relation has {arity} positions",
                    row.len()
                ));
            }
            row.iter::<Value>()
                .map(|v| from_js(&v.map_err(|e| e.to_string())?))
                .collect()
        })
        .collect()
}

fn from_js(v: &Value<'_>) -> Result<Option<DynValue>, String> {
    if v.is_null() || v.is_undefined() {
        return Ok(None);
    }
    if let Some(n) = v.as_number() {
        return if n.is_finite() {
            Ok(Some(DynValue::Num(n)))
        } else {
            Err(format!("non-finite number {n}"))
        };
    }
    if let Some(s) = v.as_string() {
        return s
            .to_string()
            .map(|s| Some(DynValue::Str(s)))
            .map_err(|e| e.to_string());
    }
    if let Some(o) = v.as_object() {
        if let Ok(name) = o.get::<_, String>(SYMBOL_KEY) {
            return Ok(Some(DynValue::Sym(name)));
        }
    }
    Err("values must be numbers, strings, or kif symbols".into())
}
