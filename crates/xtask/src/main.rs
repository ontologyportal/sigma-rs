//! `cargo xtask gen-flows` — regenerate the SDK Op activity-flow doc and
//! Mermaid diagrams from `crates/sigmakee-rs-sdk/src/<op>.rs` source.
//!
//! `cargo xtask check-flows` — exit non-zero if regenerating would change
//! the on-disk artifacts (CI gate).
//!
//! Walks each Op file with `syn`, finds `impl <Op> { fn run(...) { ... } }`,
//! collects every `kb.METHOD(...)` call plus inlined helper bodies, and
//! emits a structured tree per Op.  Control-flow context (`match`, `if`,
//! `for`) is preserved; closures and unrelated calls are dropped as noise.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use quote::ToTokens;
use syn::{
    parse_file, Block, Expr, ImplItem, Item, ItemFn, Pat, Stmt,
};

const SDK_SRC: &str = "crates/sigmakee-rs-sdk/src";

const OP_FILES: &[(&str, &str)] = &[
    ("ingest",    "IngestOp"),
    ("validate",  "ValidateOp"),
    ("translate", "TranslateOp"),
    ("load",      "LoadOp"),
    ("ask",       "AskOp"),
    ("test",      "TestOp"),
];

const OUT_MD:   &str = "docs/activity-flows.generated.md";
const OUT_HTML: &str = "docs/activity-diagrams.generated.html";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("gen-flows");
    match cmd {
        "gen-flows" => match gen_flows() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => { eprintln!("error: {e}"); ExitCode::FAILURE }
        },
        "check-flows" => match check_flows() {
            Ok(false) => { println!("flows up to date"); ExitCode::SUCCESS }
            Ok(true)  => {
                eprintln!("flows out of date — run `cargo xtask gen-flows`");
                ExitCode::from(2)
            }
            Err(e) => { eprintln!("error: {e}"); ExitCode::FAILURE }
        },
        _ => {
            eprintln!("usage: cargo xtask [gen-flows | check-flows]");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn gen_flows() -> Result<(), String> {
    let root = workspace_root()?;
    let report = build_report(&root)?;
    fs::write(root.join(OUT_MD),   render_markdown(&report)).map_err(io_err(OUT_MD))?;
    fs::write(root.join(OUT_HTML), render_html(&report)    ).map_err(io_err(OUT_HTML))?;
    println!("wrote {OUT_MD}");
    println!("wrote {OUT_HTML}");
    Ok(())
}

fn check_flows() -> Result<bool, String> {
    let root = workspace_root()?;
    let report = build_report(&root)?;
    let want_md   = render_markdown(&report);
    let want_html = render_html(&report);
    let have_md   = fs::read_to_string(root.join(OUT_MD)  ).unwrap_or_default();
    let have_html = fs::read_to_string(root.join(OUT_HTML)).unwrap_or_default();
    Ok(want_md != have_md || want_html != have_html)
}

fn workspace_root() -> Result<PathBuf, String> {
    // Resolve relative to the xtask crate manifest, NOT cwd — `cargo xtask`
    // sets CARGO_MANIFEST_DIR to .../crates/xtask.
    let mfst = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| "CARGO_MANIFEST_DIR unset; run via `cargo xtask`".to_string())?;
    Path::new(&mfst).parent().and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .ok_or_else(|| "can't derive workspace root".to_string())
}

fn io_err(label: &'static str) -> impl Fn(std::io::Error) -> String {
    move |e| format!("write {label}: {e}")
}

// ---------------------------------------------------------------------------
// Build report
// ---------------------------------------------------------------------------

struct OpReport {
    op_name: String,
    file:    String,
    nodes:   Vec<Node>,
}

fn build_report(root: &Path) -> Result<Vec<OpReport>, String> {
    let mut reports = Vec::new();
    for (file, op_name) in OP_FILES {
        let path = root.join(SDK_SRC).join(format!("{file}.rs"));
        let src  = fs::read_to_string(&path).map_err(|e| format!("read {path:?}: {e}"))?;
        let ast  = parse_file(&src).map_err(|e| format!("parse {path:?}: {e}"))?;
        let helpers = collect_helpers(&ast);
        let run     = find_run_block(&ast, op_name)
            .ok_or_else(|| format!("{op_name}: no impl ... {{ fn run }} found in {file}.rs"))?;
        let mut walker = Walker::new(&helpers);
        let nodes = walker.walk_block(run);
        reports.push(OpReport {
            op_name: op_name.to_string(),
            file:    format!("{file}.rs"),
            nodes,
        });
    }

    // manpage_view is a free fn, not an Op impl — handle separately.
    let path = root.join(SDK_SRC).join("man.rs");
    let src  = fs::read_to_string(&path).map_err(|e| format!("read {path:?}: {e}"))?;
    let ast  = parse_file(&src).map_err(|e| format!("parse {path:?}: {e}"))?;
    let helpers = collect_helpers(&ast);
    let mp = ast.items.iter().find_map(|i| match i {
        Item::Fn(f) if f.sig.ident == "manpage_view" => Some(f),
        _ => None,
    }).ok_or("manpage_view fn not found in man.rs")?;
    let mut walker = Walker::new(&helpers);
    let nodes = walker.walk_block(&mp.block);
    reports.push(OpReport {
        op_name: "manpage_view".to_string(),
        file:    "man.rs".to_string(),
        nodes,
    });

    Ok(reports)
}

fn collect_helpers(ast: &syn::File) -> HashMap<String, ItemFn> {
    let mut map = HashMap::new();
    for it in &ast.items {
        if let Item::Fn(f) = it {
            map.insert(f.sig.ident.to_string(), f.clone());
        }
    }
    map
}

fn find_run_block<'a>(ast: &'a syn::File, op_name: &str) -> Option<&'a Block> {
    for it in &ast.items {
        if let Item::Impl(imp) = it {
            let ty_str = imp.self_ty.to_token_stream().to_string();
            if !ty_str.contains(op_name) { continue; }
            for m in &imp.items {
                if let ImplItem::Fn(f) = m {
                    if f.sig.ident == "run" {
                        return Some(&f.block);
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Walker
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Node {
    /// `kb.method(args)` — the receiver was the identifier `kb`.
    KbCall(String),
    /// Call to a same-file helper fn; body inlined.
    Helper { name: String, body: Vec<Node> },
    /// `if <cond> { then } else { else_ }`.
    If     { cond: String, then: Vec<Node>, else_: Vec<Node> },
    /// `match <scrutinee> { pat => body, ... }`.
    Match  { scrutinee: String, arms: Vec<(String, Vec<Node>)> },
    /// `for <pat> in <iter> { body }`.
    For    { binder: String, iter: String, body: Vec<Node> },
    /// Bare `return`/early `?` exit point.
    Return,
}

struct Walker<'a> {
    helpers:  &'a HashMap<String, ItemFn>,
    /// Re-entrancy guard so a recursive helper doesn't blow the stack.
    inlined:  Vec<String>,
}

impl<'a> Walker<'a> {
    fn new(helpers: &'a HashMap<String, ItemFn>) -> Self {
        Self { helpers, inlined: Vec::new() }
    }

    fn walk_block(&mut self, b: &Block) -> Vec<Node> {
        let mut out = Vec::new();
        for s in &b.stmts {
            self.walk_stmt(s, &mut out);
        }
        out
    }

    fn walk_stmt(&mut self, s: &Stmt, out: &mut Vec<Node>) {
        match s {
            Stmt::Local(loc) => {
                if let Some(init) = &loc.init {
                    self.walk_expr(&init.expr, out);
                    if let Some((_, div)) = &init.diverge {
                        self.walk_expr(div, out);
                    }
                }
            }
            Stmt::Expr(e, _) => self.walk_expr(e, out),
            Stmt::Item(_) | Stmt::Macro(_) => {}
        }
    }

    fn walk_expr(&mut self, e: &Expr, out: &mut Vec<Node>) {
        match e {
            Expr::MethodCall(mc) => {
                if let Expr::Path(p) = &*mc.receiver {
                    if p.path.is_ident("kb") {
                        out.push(Node::KbCall(format!("kb.{}", mc.method)));
                    }
                }
                self.walk_expr(&mc.receiver, out);
                for a in &mc.args { self.walk_expr(a, out); }
            }
            Expr::Call(c) => {
                if let Expr::Path(p) = &*c.func {
                    if let Some(seg) = p.path.segments.last() {
                        let name = seg.ident.to_string();
                        if let Some(helper) = self.helpers.get(&name) {
                            if !self.inlined.contains(&name) {
                                self.inlined.push(name.clone());
                                let body = self.walk_block(&helper.block);
                                self.inlined.pop();
                                out.push(Node::Helper { name, body });
                                // Skip recursing into c.args — they're just bindings.
                                return;
                            }
                        }
                    }
                }
                for a in &c.args { self.walk_expr(a, out); }
            }
            Expr::If(ie) => {
                let cond  = compact(&ie.cond.to_token_stream().to_string());
                let then  = self.walk_block(&ie.then_branch);
                let else_ = match &ie.else_branch {
                    Some((_, e2)) => {
                        let mut v = Vec::new();
                        self.walk_expr(e2, &mut v);
                        v
                    }
                    None => Vec::new(),
                };
                out.push(Node::If { cond, then, else_ });
            }
            Expr::Match(m) => {
                let scrutinee = compact(&m.expr.to_token_stream().to_string());
                let mut arms = Vec::new();
                for arm in &m.arms {
                    let pat  = compact(&pat_to_string(&arm.pat));
                    let mut body = Vec::new();
                    self.walk_expr(&arm.body, &mut body);
                    arms.push((pat, body));
                }
                out.push(Node::Match { scrutinee, arms });
            }
            Expr::ForLoop(fl) => {
                let binder = compact(&pat_to_string(&fl.pat));
                let iter   = compact(&fl.expr.to_token_stream().to_string());
                let body   = self.walk_block(&fl.body);
                out.push(Node::For { binder, iter, body });
            }
            Expr::Block(eb)    => out.extend(self.walk_block(&eb.block)),
            Expr::Unsafe(u)    => out.extend(self.walk_block(&u.block)),
            Expr::Async(a)     => out.extend(self.walk_block(&a.block)),
            Expr::Loop(l)      => out.extend(self.walk_block(&l.body)),
            Expr::While(w)     => { self.walk_expr(&w.cond, out); out.extend(self.walk_block(&w.body)); }
            Expr::Try(t)       => self.walk_expr(&t.expr, out),
            Expr::Reference(r) => self.walk_expr(&r.expr, out),
            Expr::Paren(p)     => self.walk_expr(&p.expr, out),
            Expr::Group(g)     => self.walk_expr(&g.expr, out),
            Expr::Field(f)     => self.walk_expr(&f.base, out),
            Expr::Cast(c)      => self.walk_expr(&c.expr, out),
            Expr::Unary(u)     => self.walk_expr(&u.expr, out),
            Expr::Binary(b)    => { self.walk_expr(&b.left, out); self.walk_expr(&b.right, out); }
            Expr::Assign(a)    => { self.walk_expr(&a.left, out); self.walk_expr(&a.right, out); }
            Expr::Index(i)     => { self.walk_expr(&i.expr, out); self.walk_expr(&i.index, out); }
            Expr::Tuple(t)     => for el in &t.elems { self.walk_expr(el, out); },
            Expr::Array(a)     => for el in &a.elems { self.walk_expr(el, out); },
            Expr::Return(r)    => {
                out.push(Node::Return);
                if let Some(e2) = &r.expr { self.walk_expr(e2, out); }
            }
            // Closures aren't always invoked; skip their bodies to avoid
            // false call edges.  Path/lit/struct/range/macro are leaves we
            // don't care about.
            _ => {}
        }
    }
}

fn pat_to_string(p: &Pat) -> String {
    p.to_token_stream().to_string()
}

fn compact(s: &str) -> String {
    s.replace('\n', " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Markdown emit
// ---------------------------------------------------------------------------

fn render_markdown(reports: &[OpReport]) -> String {
    let mut out = String::new();
    out.push_str("# SDK Op activity flows (generated)\n\n");
    out.push_str("DO NOT EDIT — regenerate via `cargo xtask gen-flows`.\n");
    out.push_str("This file is compared by `cargo xtask check-flows` in CI.\n\n");
    for r in reports {
        out.push_str(&format!("## {}.run (`crates/sigmakee-rs-sdk/src/{}`)\n\n", r.op_name, r.file));
        if r.nodes.is_empty() {
            out.push_str("_no kb calls or helpers detected_\n\n");
            continue;
        }
        emit_md(&r.nodes, 0, &mut out);
        out.push('\n');
    }
    out
}

fn emit_md(nodes: &[Node], indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    for n in nodes {
        match n {
            Node::KbCall(s) => out.push_str(&format!("{pad}- → `{s}`\n")),
            Node::Helper { name, body } => {
                out.push_str(&format!("{pad}- → `{name}()` _(helper, inlined)_\n"));
                emit_md(body, indent + 1, out);
            }
            Node::If { cond, then, else_ } => {
                out.push_str(&format!("{pad}- if `{}`:\n", md_inline(cond)));
                emit_md(then, indent + 1, out);
                if !else_.is_empty() {
                    out.push_str(&format!("{pad}- else:\n"));
                    emit_md(else_, indent + 1, out);
                }
            }
            Node::Match { scrutinee, arms } => {
                out.push_str(&format!("{pad}- match `{}`:\n", md_inline(scrutinee)));
                for (pat, body) in arms {
                    out.push_str(&format!("{pad}  - `{}` →\n", md_inline(pat)));
                    if body.is_empty() {
                        out.push_str(&format!("{pad}    - _(no kb calls)_\n"));
                    } else {
                        emit_md(body, indent + 2, out);
                    }
                }
            }
            Node::For { binder, iter, body } => {
                out.push_str(&format!("{pad}- for `{}` in `{}`:\n", md_inline(binder), md_inline(iter)));
                emit_md(body, indent + 1, out);
            }
            Node::Return => out.push_str(&format!("{pad}- ⏎ early return\n")),
        }
    }
}

fn md_inline(s: &str) -> String {
    s.replace('`', "\u{2032}")
}

// ---------------------------------------------------------------------------
// Mermaid emit
// ---------------------------------------------------------------------------

fn render_html(reports: &[OpReport]) -> String {
    let mut out = String::new();
    out.push_str("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n");
    out.push_str("<title>SDK Op activity diagrams (generated)</title>\n");
    out.push_str("<style>\n");
    out.push_str("body{font-family:-apple-system,system-ui,sans-serif;max-width:1100px;margin:2em auto;padding:0 1em;color:#222}\n");
    out.push_str("h1{border-bottom:2px solid #444;padding-bottom:.3em}\n");
    out.push_str("h2{margin-top:2em;border-bottom:1px solid #ccc;padding-bottom:.2em}\n");
    out.push_str(".legend{background:#f6f6f6;padding:1em;border-radius:6px;font-size:.9em}\n");
    out.push_str(".mermaid{background:#fafafa;padding:1em;border-radius:6px;margin:1em 0;overflow:auto}\n");
    out.push_str("</style></head><body>\n");
    out.push_str("<h1>SDK Op activity diagrams (generated)</h1>\n");
    out.push_str("<div class=\"legend\">Generated by <code>cargo xtask gen-flows</code> from <code>crates/sigmakee-rs-sdk/src/&lt;op&gt;.rs</code>. Do not edit.</div>\n");

    for r in reports {
        out.push_str(&format!("<h2>{}.run <small>(<code>crates/sigmakee-rs-sdk/src/{}</code>)</small></h2>\n", r.op_name, r.file));
        out.push_str("<div class=\"mermaid\">\n");
        out.push_str(&render_mermaid(&r.op_name, &r.nodes));
        out.push_str("</div>\n");
    }

    out.push_str("<script type=\"module\">\n");
    out.push_str("import mermaid from 'https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs';\n");
    out.push_str("mermaid.initialize({startOnLoad:true,theme:'neutral',flowchart:{useMaxWidth:true,htmlLabels:true}});\n");
    out.push_str("</script>\n</body></html>\n");
    out
}

struct Mermaid {
    out:    String,
    next:   usize,
}

/// Source endpoints in flight: pairs of (parent_node_id, optional edge label
/// to use on the edge from that parent to the next node).
type Tail = (String, Option<String>);

impl Mermaid {
    fn new() -> Self { Self { out: String::new(), next: 0 } }

    fn fresh(&mut self) -> String {
        self.next += 1;
        format!("N{}", self.next)
    }

    fn emit_node(&mut self, id: &str, shape: char, label: &str) {
        let l = label_escape(label);
        let line = match shape {
            'd' => format!("  {id}{{\"{l}\"}}\n"),    // diamond
            's' => format!("  {id}([\"{l}\"])\n"),    // start/end
            _   => format!("  {id}[\"{l}\"]\n"),      // rect
        };
        self.out.push_str(&line);
    }

    fn connect(&mut self, tails: &[Tail], to: &str) {
        for (p, edge) in tails {
            match edge {
                Some(lbl) => self.out.push_str(&format!("  {p} -- {} --> {to}\n", label_escape(lbl))),
                None      => self.out.push_str(&format!("  {p} --> {to}\n")),
            }
        }
    }

    fn walk(&mut self, nodes: &[Node], entries: Vec<Tail>) -> Vec<Tail> {
        let mut tails = entries;
        for n in nodes {
            tails = self.walk_one(n, tails);
        }
        tails
    }

    fn walk_one(&mut self, n: &Node, entries: Vec<Tail>) -> Vec<Tail> {
        match n {
            Node::KbCall(label) => {
                let id = self.fresh();
                self.emit_node(&id, 'r', label);
                self.connect(&entries, &id);
                vec![(id, None)]
            }
            Node::Helper { name, body } => {
                let id = self.fresh();
                self.emit_node(&id, 'r', &format!("{name}() helper"));
                self.connect(&entries, &id);
                if body.is_empty() {
                    vec![(id, None)]
                } else {
                    self.walk(body, vec![(id, None)])
                }
            }
            Node::If { cond, then, else_ } => {
                let did = self.fresh();
                self.emit_node(&did, 'd', cond);
                self.connect(&entries, &did);
                let then_tails = if then.is_empty() {
                    vec![(did.clone(), Some("yes".into()))]
                } else {
                    self.walk(then, vec![(did.clone(), Some("yes".into()))])
                };
                let else_tails = if else_.is_empty() {
                    vec![(did.clone(), Some("no".into()))]
                } else {
                    self.walk(else_, vec![(did.clone(), Some("no".into()))])
                };
                then_tails.into_iter().chain(else_tails).collect()
            }
            Node::Match { scrutinee, arms } => {
                let did = self.fresh();
                self.emit_node(&did, 'd', scrutinee);
                self.connect(&entries, &did);
                let mut all = Vec::new();
                for (pat, body) in arms {
                    let arm_entries = vec![(did.clone(), Some(pat.clone()))];
                    let tails = if body.is_empty() { arm_entries } else { self.walk(body, arm_entries) };
                    all.extend(tails);
                }
                all
            }
            Node::For { binder, iter, body } => {
                let id = self.fresh();
                self.emit_node(&id, 'd', &format!("for {binder} in {iter}"));
                self.connect(&entries, &id);
                if body.is_empty() {
                    return vec![(id, Some("done".into()))];
                }
                let body_tails = self.walk(body, vec![(id.clone(), Some("each".into()))]);
                // Loop edge back to header.
                self.connect(&body_tails, &id);
                vec![(id, Some("done".into()))]
            }
            Node::Return => {
                let id = self.fresh();
                self.emit_node(&id, 's', "return");
                self.connect(&entries, &id);
                Vec::new()
            }
        }
    }
}

fn render_mermaid(op_name: &str, nodes: &[Node]) -> String {
    let mut m = Mermaid::new();
    m.out.push_str("flowchart TD\n");
    let start = m.fresh();
    m.emit_node(&start, 's', &format!("{op_name}.run"));
    let tails = m.walk(nodes, vec![(start, None)]);
    let end = m.fresh();
    m.emit_node(&end, 's', "end");
    m.connect(&tails, &end);
    m.out
}

fn label_escape(s: &str) -> String {
    // Mermaid quoted labels accept most chars; collapse and trim.
    let s = compact(s);
    // Hard-cap label length so wide diagrams stay legible.
    let s = if s.len() > 80 { format!("{}…", &s[..78]) } else { s };
    s.replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}
