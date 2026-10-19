// Finds the lint suppressions in Rust source: each lint named in an allow( or
// expect( anywhere in code (comments and strings stripped), so an attribute counts
// however it is spelled or assembled, including through a macro that builds the
// # and the [...] from separate tokens. An attribute whose body holds a macro
// metavariable ($meta, $($t)*) forwards whatever the caller passes, so it also
// counts as attr(<body>), whitespace removed.

const RAW_STRING = /r(#*)"/y;
const LINE_COMMENT = /\/\/[^\n]*/y;
const STRING = /"(?:\\[\s\S]|[^\\"])*"/y;
const CHAR = /'(?:\\u\{[\da-fA-F]+\}|\\x[\da-fA-F]{2}|\\.|[^\\'\n])'/uy;

function blockCommentEnd(source, start) {
  let depth = 0;
  let i = start;
  while (i < source.length) {
    if (source.startsWith("/*", i)) {
      depth++;
      i += 2;
    } else if (source.startsWith("*/", i)) {
      i += 2;
      if (--depth === 0) return i;
    } else {
      i++;
    }
  }
  return source.length;
}

function noiseEnd(source, i) {
  if (source.startsWith("/*", i)) return blockCommentEnd(source, i);
  RAW_STRING.lastIndex = i;
  const raw = RAW_STRING.exec(source);
  if (raw) {
    const close = source.indexOf(`"${raw[1]}`, RAW_STRING.lastIndex);
    return close < 0 ? source.length : close + 1 + raw[1].length;
  }
  for (const pattern of [LINE_COMMENT, STRING, CHAR]) {
    pattern.lastIndex = i;
    if (pattern.exec(source)) return pattern.lastIndex;
  }
  return i;
}

// The source with every comment, string and char literal replaced by a space.
function codeOnly(source) {
  const parts = [];
  let start = 0;
  for (let i = 0; i < source.length; i++) {
    if (!"/\"'r".includes(source[i])) continue;
    const end = noiseEnd(source, i);
    if (end === i) continue;
    parts.push(source.slice(start, i), " ");
    start = end;
    i = end - 1;
  }
  parts.push(source.slice(start));
  return parts.join("");
}

function closing(text, open) {
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    if (text[i] === "[" || text[i] === "(") depth++;
    else if ((text[i] === "]" || text[i] === ")") && --depth === 0) return i;
  }
  return text.length;
}

export function* lintAttributes(source) {
  const code = codeOnly(source);
  for (const attribute of code.matchAll(/#\s*!?\s*\[/g)) {
    const open = attribute.index + attribute[0].length - 1;
    const body = code.slice(open, closing(code, open));
    if (body.includes("$")) yield `attr(${body.replace(/\s+/g, "")}])`;
  }
  for (const call of code.matchAll(/(?<![\w.])(allow|expect)\s*\(/g)) {
    const args = call.index + call[0].length - 1;
    for (const item of code.slice(args + 1, closing(code, args)).split(",")) {
      const lint = item.replace(/\s+/g, "");
      if (lint && !lint.startsWith("reason")) yield `${call[1]}(${lint})`;
    }
  }
}
