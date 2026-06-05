//! Compile the C smoke test (`csrc/smoke.c`) and link it into the crate so the
//! unit test in `src/lib.rs` can call it over FFI under a plain `cargo test`.
//! This exercises the committed `lark.h` and the `#[no_mangle]` surface from real
//! C compiled by the system C compiler — without depending on cargo emitting a
//! standalone `.a`/`.so` (it builds only the rlib for tests). The link directive
//! a build script emits applies to the crate's own targets (including its unit
//! tests), which is why the test lives in `src/lib.rs` rather than `tests/`.

fn main() {
    println!("cargo:rerun-if-changed=csrc/smoke.c");
    println!("cargo:rerun-if-changed=lark.h");
    cc::Build::new()
        .file("csrc/smoke.c")
        .include(".") // for lark.h
        .compile("lark_h_smoke");
}
