//! Must fail to compile: the grammar is invalid, so `include_lark!` expands to a
//! `compile_error!`. Driven by `lark_proc/tests/compile_fail.rs`.

lark_proc::include_lark!("bad.lark");

fn main() {}
