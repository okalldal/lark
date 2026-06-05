//! Compile-fail coverage for `include_lark!`.
//!
//! The headline guarantee of issue #49 is that an *invalid grammar is a compiler
//! error*, not a runtime panic. That behavior was previously only verified by
//! hand; this test pins it in CI without pulling in a snapshot-testing crate
//! (keeping `lark_proc` dependency-free).
//!
//! `tests/fixtures/bad_grammar_crate/` is a tiny standalone crate whose `main.rs`
//! does `include_lark!("bad.lark")` against a deliberately malformed grammar. We
//! build it with `cargo build` and assert that (a) the build fails and (b) the
//! failure is our grammar-validation error attributed to the macro — i.e. the
//! grammar was rejected at compile time, exactly as a user would experience it.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn invalid_grammar_is_a_compile_error() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/bad_grammar_crate/Cargo.toml");

    // A dedicated target dir (cargo hands integration tests `CARGO_TARGET_TMPDIR`
    // for exactly this) keeps the build from contending on the workspace target
    // lock held by the outer `cargo test`.
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("compile_fail_fixture");

    let output = Command::new(env!("CARGO"))
        .args(["build", "--manifest-path"])
        .arg(&fixture)
        .arg("--target-dir")
        .arg(&target_dir)
        .output()
        .expect("failed to spawn cargo for the fixture crate");

    assert!(
        !output.status.success(),
        "fixture crate with an invalid grammar unexpectedly compiled — \
         include_lark! failed to reject it at compile time"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("include_lark!: grammar") && stderr.contains("is invalid"),
        "build failed, but not with the expected grammar-validation error.\n\
         --- cargo stderr ---\n{stderr}"
    );
}
