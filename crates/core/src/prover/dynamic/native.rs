//! Native shared-library calls for dynamic relations.
//!
//! A script declares each function it calls with `//@use lib { f(int, float,
//! string) -> type }`; the declaration is the only source of the C
//! signature, so a mismatch with the library is undefined behavior the
//! declaring script owns.  Values are `int` (`int64_t`), `float` (`double`),
//! and `string` (`const char *` in, caller-buffer out).
//!
//! Libraries never allocate for the caller: a `string`-returning function
//! takes two trailing arguments `char *out, size_t cap` and returns
//! `int64_t` -- the bytes written, or `-needed` when `cap` is too small, in
//! which case the call is retried once with `needed` bytes.

use std::ffi::{c_char, c_void, CString};
use std::path::{Path, PathBuf};

use libffi::middle::{Arg, Cif, CodePtr, Type};
use libloading::Library;

use crate::parse::script::{LibDecl, LibFn, ValType};

/// Initial capacity of a string return buffer.
const STRING_BUF: usize = 4096;

/// A value crossing the library boundary.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NativeValue {
    Int(i64),
    Float(f64),
    Str(String),
}

/// One loaded library with its declared, resolved functions.
pub(crate) struct NativeLib {
    // Keeps the code the `functions` pointers refer to mapped.
    _library: Library,
    pub(crate) functions: Vec<NativeFn>,
}

/// A resolved function pointer plus its declared signature.
pub(crate) struct NativeFn {
    pub(crate) sig: LibFn,
    ptr: *const c_void,
}

/// The platform file name for library `name`.
fn library_file(name: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("lib{name}.dylib")
    } else {
        format!("lib{name}.so")
    }
}

/// Resolve library `name` to a file directly inside `dir`.
///
/// `dir` must be absolute, `name` a plain ASCII identifier, and the
/// canonical file (symlinks followed) must sit directly in the canonical
/// `dir`.  Never falls back to the loader's search path.
pub(crate) fn library_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let plain = name
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !plain {
        return Err(format!("invalid library name `{name}`"));
    }
    if !dir.is_absolute() {
        return Err(format!(
            "libDir `{}` must be an absolute path",
            dir.display()
        ));
    }
    let root = dir
        .canonicalize()
        .map_err(|e| format!("libDir `{}`: {e}", dir.display()))?;
    let file = root.join(library_file(name));
    let resolved = file
        .canonicalize()
        .map_err(|e| format!("cannot load {}: {e}", file.display()))?;
    if resolved.parent() != Some(root.as_path()) {
        return Err(format!(
            "{} resolves outside libDir (to {})",
            file.display(),
            resolved.display()
        ));
    }
    Ok(resolved)
}

impl NativeLib {
    /// Load `decl.name` from `dir` and resolve every declared function.
    pub(crate) fn load(dir: &Path, decl: &LibDecl) -> Result<Self, String> {
        let path = library_path(dir, &decl.name)?;
        // SAFETY: loading a library runs its initializers.  The library comes
        // from the configured `libDir`, which the operator controls; enabling
        // the `dynamic` feature is the opt-in to running that code.
        let library = unsafe { Library::new(&path) }
            .map_err(|e| format!("cannot load {}: {e}", path.display()))?;
        let mut functions = Vec::with_capacity(decl.functions.len());
        for sig in &decl.functions {
            let mut symbol = sig.name.clone().into_bytes();
            symbol.push(0);
            // SAFETY: the symbol is only used as an opaque address here; it is
            // called through `NativeFn::call` with the declared signature.
            let ptr = unsafe { library.get::<unsafe extern "C" fn()>(&symbol[..]) }
                .map(|f| *f as *const c_void)
                .map_err(|e| format!("{}: no symbol `{}`: {e}", decl.name, sig.name))?;
            functions.push(NativeFn {
                sig: sig.clone(),
                ptr,
            });
        }
        Ok(Self {
            _library: library,
            functions,
        })
    }
}

fn ffi_type(t: ValType) -> Type {
    match t {
        ValType::Int => Type::i64(),
        ValType::Float => Type::f64(),
        ValType::Str => Type::pointer(),
    }
}

/// A call argument in its C representation, kept alive for the call.
enum CArg {
    Int(i64),
    Float(f64),
    Str { _owned: CString, ptr: *const c_char },
}

impl NativeFn {
    /// Call with `args`, which must already match the declared parameter
    /// types (the JS bridge converts and checks them).
    pub(crate) fn call(&self, args: &[NativeValue]) -> Result<NativeValue, String> {
        if args.len() != self.sig.params.len() {
            return Err(format!(
                "{} expects {} argument(s), got {}",
                self.sig.name,
                self.sig.params.len(),
                args.len()
            ));
        }
        let mut cargs = Vec::with_capacity(args.len());
        for (arg, ty) in args.iter().zip(&self.sig.params) {
            cargs.push(match (arg, ty) {
                (NativeValue::Int(i), ValType::Int) => CArg::Int(*i),
                (NativeValue::Float(f), ValType::Float) => CArg::Float(*f),
                (NativeValue::Str(s), ValType::Str) => {
                    let c = CString::new(s.as_str())
                        .map_err(|_| format!("{}: string argument contains NUL", self.sig.name))?;
                    let ptr = c.as_ptr();
                    CArg::Str { _owned: c, ptr }
                }
                _ => return Err(format!("{}: argument type mismatch", self.sig.name)),
            });
        }
        let base_args = || -> Vec<Arg> {
            cargs
                .iter()
                .map(|a| match a {
                    CArg::Int(i) => Arg::new(i),
                    CArg::Float(f) => Arg::new(f),
                    CArg::Str { ptr, .. } => Arg::new(ptr),
                })
                .collect()
        };
        let mut params: Vec<Type> = self.sig.params.iter().copied().map(ffi_type).collect();
        let code = CodePtr::from_ptr(self.ptr);
        match self.sig.ret {
            ValType::Int => {
                let cif = Cif::new(params, Type::i64());
                // SAFETY: `cif` is built from the script's declared signature,
                // and every argument was converted to that declared type.
                Ok(NativeValue::Int(unsafe {
                    cif.call::<i64>(code, &base_args())
                }))
            }
            ValType::Float => {
                let cif = Cif::new(params, Type::f64());
                // SAFETY: as above.
                Ok(NativeValue::Float(unsafe {
                    cif.call::<f64>(code, &base_args())
                }))
            }
            ValType::Str => {
                params.push(Type::pointer());
                params.push(Type::usize());
                let cif = Cif::new(params, Type::i64());
                let mut cap = STRING_BUF;
                for _ in 0..2 {
                    let mut buf = vec![0u8; cap];
                    let out = buf.as_mut_ptr();
                    let len = cap;
                    let mut call_args = base_args();
                    call_args.push(Arg::new(&out));
                    call_args.push(Arg::new(&len));
                    // SAFETY: as above, plus the buffer contract: the callee
                    // writes at most `len` bytes into `out`, which lives for
                    // the whole call.
                    let n = unsafe { cif.call::<i64>(code, &call_args) };
                    if n >= 0 {
                        let n = (n as usize).min(cap);
                        return Ok(NativeValue::Str(
                            String::from_utf8_lossy(&buf[..n]).into_owned(),
                        ));
                    }
                    cap = n.unsigned_abs() as usize;
                }
                Err(format!(
                    "{}: string result still too large after retry",
                    self.sig.name
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(library_file(name));
        std::fs::write(&p, b"").expect("write");
        p
    }

    #[test]
    fn resolves_a_library_directly_inside_lib_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = touch(dir.path(), "sensor");
        let got = library_path(dir.path(), "sensor").expect("path");
        assert_eq!(got, p.canonicalize().expect("canon"));
    }

    #[test]
    fn rejects_path_like_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in [
            "../sensor",
            "/etc/sensor",
            "a/b",
            "a.b",
            "",
            "1lib",
            "caf\u{e9}",
        ] {
            assert!(library_path(dir.path(), name).is_err(), "accepted `{name}`");
        }
    }

    #[test]
    fn rejects_a_relative_lib_dir() {
        let err = library_path(Path::new("libs"), "sensor").expect_err("relative");
        assert!(err.contains("absolute"), "{err}");
        assert!(library_path(Path::new(""), "sensor").is_err());
    }

    #[test]
    fn rejects_a_symlink_escaping_lib_dir() {
        let outside = tempfile::tempdir().expect("tempdir");
        let target = touch(outside.path(), "evil");
        let dir = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink(&target, dir.path().join(library_file("sensor")))
            .expect("symlink");
        let err = library_path(dir.path(), "sensor").expect_err("escape");
        assert!(err.contains("outside libDir"), "{err}");
    }

    #[test]
    fn accepts_a_lib_dir_reached_through_a_symlink() {
        let real = tempfile::tempdir().expect("tempdir");
        touch(real.path(), "sensor");
        let links = tempfile::tempdir().expect("tempdir");
        let dir = links.path().join("libs");
        std::os::unix::fs::symlink(real.path(), &dir).expect("symlink");
        assert!(library_path(&dir, "sensor").is_ok());
    }

    #[test]
    fn a_missing_library_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(library_path(dir.path(), "absent").is_err());
    }
}
