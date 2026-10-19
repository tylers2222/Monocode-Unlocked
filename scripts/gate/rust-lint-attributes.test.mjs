import assert from "node:assert/strict";
import { test } from "node:test";
import { lintAttributes } from "./rust-lint-attributes.mjs";

const lints = (source) => [...lintAttributes(source)];

test("outer, inner and cfg_attr allows each count", () => {
  assert.deepEqual(lints("#[allow(dead_code)]\nfn f() {}"), ["allow(dead_code)"]);
  assert.deepEqual(lints('mod m {\n    #![allow(dead_code, reason = "probe")]\n}'), ["allow(dead_code)"]);
  assert.deepEqual(lints("#[cfg_attr(all(), allow(dead_code))]\nfn f() {}"), ["allow(dead_code)"]);
});

test("a spaced inner attribute counts", () => {
  const source = '#[rustfmt::skip]\nmod m {\n    # ! [allow(dead_code, reason = "probe")]\n}';
  assert.deepEqual(lints(source), ["allow(dead_code)"]);
});

test("each lint in a widened multi-line expect counts, tool lints included", () => {
  const source = `#[expect(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    rustdoc::broken_intra_doc_links,
    reason = "baseline 2026-10"
)]
fn f() {}`;
  assert.deepEqual(lints(source), [
    "expect(clippy::too_many_lines)",
    "expect(clippy::cognitive_complexity)",
    "expect(rustdoc::broken_intra_doc_links)",
  ]);
});

test("an attribute forwarded through a macro counts as attr(<body>)", () => {
  const named = "macro_rules! m { ($a:meta) => { #[$a] fn f() {} } }";
  const namedInner = "macro_rules! m { ($a:meta) => { mod n { #![$a] } } }";
  const repeated = "macro_rules! m { ($($t:tt)*) => { #[$($t)*] fn f() {} } }";
  const repeatedInner = "macro_rules! m { ($($t:tt)*) => { mod n { #![$($t)*] } } }";
  const inCfgAttr = "macro_rules! m { ($a:meta) => { #[cfg_attr(all(), $a)] fn f() {} } }";
  assert.deepEqual(lints(named), ["attr([$a])"]);
  assert.deepEqual(lints(namedInner), ["attr([$a])"]);
  assert.deepEqual(lints(repeated), ["attr([$($t)*])"]);
  assert.deepEqual(lints(repeatedInner), ["attr([$($t)*])"]);
  assert.deepEqual(lints(inCfgAttr), ["attr([cfg_attr(all(),$a)])"]);
  assert.deepEqual(lints("macro_rules! m { ($a:meta) => { # [ $a ] fn f() {} } }"), ["attr([$a])"]);
});

test("an allow built from separate # and [...] tokens in a macro counts", () => {
  const hashToken = 'macro_rules! m { ($h:tt) => { $h[allow(dead_code, reason = "probe")] fn f() {} } }\nm!(#);';
  const bracketToken = 'macro_rules! m { ($b:tt) => { # $b fn f() {} } }\nm!([allow(dead_code, reason = "probe")]);';
  assert.deepEqual(lints(hashToken), ["allow(dead_code)"]);
  assert.deepEqual(lints(bracketToken), ["allow(dead_code)"]);
});

test("allow( and expect( count only as whole names, not in other names, method calls, fn definitions or path calls", () => {
  const source = '#[disallow(dead_code)]\n#[my_expect(unused)]\n#[doc = x.expect(y)]\nfn f() {}';
  assert.deepEqual(lints(source), []);
  const definitions = 'impl P { fn allow(&self, t: &str) -> bool { P::allow(self, t) } fn expect(self) {} }';
  assert.deepEqual(lints(definitions), []);
});

test("attribute text inside comments, strings and char literals does not count", () => {
  const source = String.raw`
    // #[allow(dead_code)]
    /// #[allow(dead_code)]
    /* #[allow(dead_code)] */
    const A: &str = "#[allow(dead_code)] \" #[allow(unused)]";
    const B: &str = r#"#[allow(dead_code)] " #[allow(unused)]"#;
    const C: char = '"';
    fn f<'a>(x: &'a str) -> &'a str { x }
  `;
  assert.deepEqual(lints(source), []);
});

test("a hex char escape or a nested block comment does not hide the next attribute", () => {
  const hex = String.raw`const A: [char; 2] = ['\x41','"'];
#[expect(dead_code, reason = "probe")]
fn f() {}`;
  const nested = `/* a /* b */ " */
#[expect(dead_code, reason = "probe")]
fn f() {}`;
  assert.deepEqual(lints(hex), ["expect(dead_code)"]);
  assert.deepEqual(lints(nested), ["expect(dead_code)"]);
});
