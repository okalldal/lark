//! Compile and run the C smoke test (`tests/smoke.c`) against the built
//! `lark_h` shared library, proving the committed `lark.h` + the `#[no_mangle]`
//! surface actually link and behave from C. This is the issue #48 done-when:
//! "A C smoke-test parses JSON and checks the tree structure."
//!
//! The harness invokes the system C compiler (`$CC`, default `cc`) to build
//! `smoke.c` linked against the library cargo just built, then runs the resulting
//! executable and asserts a clean exit. It links the **staticlib**
//! (`liblark_h.a`) by default — that is the artifact `cargo test` reliably
//! produces — and falls back to the **cdylib** (`liblark_h.so`/`.dylib`) when a
//! plain `cargo build` has produced one. It is skipped (not failed) only if no C
//! compiler is available, so the Rust suite stays runnable on a box without a
//! toolchain while CI — which has `cc` — exercises it for real.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Directory holding the freshly built `liblark_h.*` (the cargo target/<profile>
/// dir), derived from this test binary's own path: it lives in `<target>/deps/`.
fn artifact_dir() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // drop the test binary name
    if p.ends_with("deps") {
        p.pop();
    }
    p
}

/// How the C executable links against the built lark_h library.
enum Linkage {
    /// `liblark_h.a` — the artifact `cargo test` reliably builds. Linked
    /// statically; the Rust std runtime is pulled in from the archive plus the
    /// usual system libraries.
    Static(PathBuf),
    /// `liblark_h.so` / `.dylib` — present after a plain `cargo build`. Linked
    /// dynamically with an embedded rpath so the exe finds it at run time.
    Shared(PathBuf),
}

/// Choose what to link against. Prefer the shared library when a `cargo build`
/// has produced one (it most closely mirrors how a real C consumer embeds
/// lark_h); otherwise use the static archive that `cargo test` always builds.
fn linkage(dir: &Path) -> Option<Linkage> {
    for name in ["liblark_h.so", "liblark_h.dylib", "lark_h.dll"] {
        let cand = dir.join(name);
        if cand.exists() {
            return Some(Linkage::Shared(cand));
        }
    }
    let archive = dir.join("liblark_h.a");
    archive.exists().then_some(Linkage::Static(archive))
}

/// Pick a C compiler: `$CC` if set, otherwise `cc`. Returns None if it can't run.
fn c_compiler() -> Option<String> {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let ok = Command::new(&cc)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    ok.then_some(cc)
}

#[test]
fn c_smoke_test() {
    let dir = artifact_dir();
    let link = linkage(&dir).unwrap_or_else(|| {
        panic!(
            "no liblark_h.{{a,so,dylib}} found in {} — cargo should build the \
             lark_h library before integration tests run.",
            dir.display()
        )
    });

    let cc = match c_compiler() {
        Some(cc) => cc,
        None => {
            eprintln!("no C compiler ($CC / cc) available — skipping C smoke test");
            return;
        }
    };

    let manifest = env!("CARGO_MANIFEST_DIR");
    let src = format!("{manifest}/tests/smoke.c");
    let exe = dir.join(if cfg!(windows) {
        "lark_h_smoke.exe"
    } else {
        "lark_h_smoke"
    });

    let mut cmd = Command::new(&cc);
    cmd.arg(&src)
        .arg("-I")
        .arg(manifest) // for lark.h
        .arg("-o")
        .arg(&exe);

    match &link {
        Linkage::Static(archive) => {
            eprintln!("static-linking {}", archive.display());
            // The archive carries the Rust std runtime; these are its usual
            // system dependencies on Unix. Harmless when the linker doesn't need
            // them (e.g. the toolchain already resolved them transitively).
            cmd.arg(archive);
            if !cfg!(windows) {
                cmd.args(["-lpthread", "-ldl", "-lm"]);
            }
        }
        Linkage::Shared(lib) => {
            eprintln!("dynamic-linking {}", lib.display());
            cmd.arg(format!("-L{}", dir.display()))
                .arg("-llark_h")
                // Embed the artifact dir as an rpath so the exe finds the .so at
                // run time without LD_LIBRARY_PATH.
                .arg(format!("-Wl,-rpath,{}", dir.display()));
        }
    }

    let status = cmd.status().expect("failed to invoke C compiler");
    assert!(status.success(), "C smoke test failed to compile/link");

    let run = Command::new(&exe)
        .status()
        .expect("failed to run compiled C smoke test");
    assert!(
        run.success(),
        "C smoke test exited with failure (status {:?})",
        run.code()
    );
}
