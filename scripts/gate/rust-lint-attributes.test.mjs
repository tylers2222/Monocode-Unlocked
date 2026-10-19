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

test("an attribute forwarded through a macro counts as attr($name)", () => {
  const outer = "macro_rules! m { ($a:meta) => { #[$a] fn f() {} } }";
  const inner = "macro_rules! m { ($a:meta) => { mod n { #![$a] } } }";
  assert.deepEqual(lints(outer), ["attr($a)"]);
  assert.deepEqual(lints(inner), ["attr($a)"]);
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
