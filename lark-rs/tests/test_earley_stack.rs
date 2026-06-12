//! Issue #33 regression net: the SPPF forest→tree walk must not recurse to
//! forest depth.
//!
//! The walk used to be naturally recursive — O(input length) deep for any
//! list-like rule — and ran on a dedicated thread with a 256 MB stack as a
//! band-aid. That thread is gone (and `std::thread` does not exist on WASM,
//! #47), so these tests run deep parses on a thread with a deliberately *small*
//! stack: if recursion proportional to input depth ever creeps back into the
//! walk (value building, priority summing, or `_ambig` dedup-keying), they
//! crash with a stack overflow rather than fail an assert — which is the point.
//!
//! Inputs are sized so the former recursive walk (several frames of a few
//! hundred bytes per forest level) would need well over [`STACK`] bytes, while
//! everything that legitimately stays on the native stack fits comfortably.

use lark_rs::tree::{Child, ParseTree, Tree};
use lark_rs::{Ambiguity, Lark, LarkOptions, LexerType, ParserAlgorithm};

/// Small enough that a per-forest-level recursion of the old walk's shape
/// overflows it at the input sizes below; large enough for the lexer/chart
/// machinery, which was never the problem.
const STACK: usize = 512 * 1024;

fn earley(grammar: &str, ambiguity: Ambiguity) -> Lark {
    Lark::new(
        grammar,
        LarkOptions {
            start: vec!["start".to_string()],
            parser: ParserAlgorithm::Earley,
            lexer: LexerType::Basic,
            ambiguity,
            ..LarkOptions::default()
        },
    )
    .expect("grammar builds")
}

/// Run `f` on a thread whose stack is [`STACK`] bytes and hand its result back.
/// A recursive walk overflows in `f`; the result is *returned* so any deep tree
/// is dropped on the test thread's normal-sized stack (drop depth of the
/// caller-owned result tree is the caller's property, same as for LALR — the
/// guarantee under test is the walk, not the returned value's `Drop`). The
/// `Lark` is *moved* into `f` (it is `Send`, not `Sync`).
fn on_small_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn_scoped(s, f)
            .expect("spawn small-stack thread")
            .join()
            .expect("the forest walk must not overflow a small stack")
    })
}

/// Resolve mode, transparent-helper chain (`x+`): the SPPF is a chain of
/// `__anon_plus` nodes one per token — the deepest *walk* with the flattest
/// *tree* (all children splice into one `start` node), i.e. exactly the shape
/// the streaming assembly (#54/#55) walks and the old recursion died on.
#[test]
fn resolve_walk_is_iterative_on_transparent_chain() {
    const N: usize = 30_000;
    let lark = earley("start: X+\nX: \"x\"\n", Ambiguity::Resolve);
    let input = "x".repeat(N);
    let tree = on_small_stack(move || lark.parse(&input).expect("deep x+ parses"));
    let ParseTree::Tree(t) = tree else {
        panic!("expected a tree at the root");
    };
    assert_eq!(t.data, "start");
    assert_eq!(t.children.len(), N, "x+ splices flat: one token per x");
}

/// Explicit mode walks a different frame family (`Derivs`/`ExpandPacked`/
/// `ExpandInter`) plus the `_ambig` dedup keying — pin that path too. The
/// grammar is unambiguous, so the result is identical to resolve; the input is
/// smaller because the explicit walk's per-node value materialization is the
/// known O(n²) of #59.
#[test]
fn explicit_walk_is_iterative_on_transparent_chain() {
    const N: usize = 3_000;
    let lark = earley("start: X+\nX: \"x\"\n", Ambiguity::Explicit);
    let input = "x".repeat(N);
    let tree = on_small_stack(move || lark.parse(&input).expect("deep x+ parses"));
    let ParseTree::Tree(t) = tree else {
        panic!("expected a tree at the root");
    };
    assert_eq!(t.data, "start");
    assert_eq!(t.children.len(), N);
}

/// Resolve mode, *non-transparent* right recursion: every level is a real
/// symbol node, so the walk goes through `Eval`'s per-node buffer push (and the
/// lazy priority sum descends the same chain) rather than the splice path. The
/// result tree is genuinely N deep; it is dropped outside the small stack.
#[test]
fn resolve_walk_is_iterative_on_nested_chain() {
    const N: usize = 10_000;
    let lark = earley("start: a\na: X a | X\nX: \"x\"\n", Ambiguity::Resolve);
    let input = "x".repeat(N);
    let tree = on_small_stack(move || lark.parse(&input).expect("deep nesting parses"));
    let ParseTree::Tree(root) = tree else {
        panic!("expected a tree at the root");
    };
    // Count the nesting depth iteratively (the tree is too deep to recurse on).
    let mut depth = 0usize;
    let mut cur: &Tree = &root;
    loop {
        depth += 1;
        match cur.children.iter().find_map(|c| match c {
            Child::Tree(t) => Some(t),
            _ => None,
        }) {
            Some(t) => cur = t,
            None => break,
        }
    }
    // start → a (×N): the innermost `a` has only a token child.
    assert_eq!(depth, N + 1, "right recursion nests one `a` per token");
    drop_deep(root);
}

/// Drop a deep tree without recursing. `Tree`'s compiler-generated drop glue
/// recurses to tree depth — a property of the caller-owned *result* value on
/// every engine (LALR returns the same `Tree` type), unchanged by #33, which is
/// about the walk. Handled explicitly here so this test exercises exactly the
/// guarantee under test and nothing else.
fn drop_deep(mut t: Tree) {
    let mut stack = std::mem::take(&mut t.children);
    while let Some(c) = stack.pop() {
        if let Child::Tree(mut sub) = c {
            // `sub`'s children move into the work list; `sub` then drops flat.
            stack.append(&mut sub.children);
        }
    }
}
