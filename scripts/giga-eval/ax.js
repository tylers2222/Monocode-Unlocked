// osascript -l JavaScript ax.js <command> <pid> [args...]
//   find <pid> <label> [role]   first element whose AX description, title or value is <label> ("x*" = prefix); prints JSON with its frame
//   count <pid> <label> [role]       number of elements whose description, title or value is <label> ("x*" = prefix)
//   checked <pid> <label>       AXValue of the element found by label (a switch reads 1 when on)
//   dump <pid> [depth]          role | description | title | value, indented
//   windowid <pid>              CGWindowID of the process's largest on-screen window
//   front <pid>                 bring the process to the front
//
// Element-by-element Apple Events take minutes on a webview, so the tree is
// read one depth level per event: `uiElements.uiElements.role()` returns the
// roles of every grandchild as nested arrays.
ObjC.import("CoreGraphics");

// By path: a lookup by name can fail with -2700 (Application can't be
// found) while System Events runs.
const se = Application("/System/Library/CoreServices/System Events.app");
const MAX_DEPTH = 40;

// JXA's whose({unixId}) can return another process with the same name (the
// installed MonoCode), so resolve the System Events id from the bulk lists,
// address the process by that id, and verify.
function proc(pid) {
  const unixIds = se.processes.unixId();
  const ids = se.processes.id();
  const i = unixIds.indexOf(Number(pid));
  if (i < 0) throw new Error(`no process ${pid}`);
  const p = se.processes.byId(ids[i]);
  if (p.unixId() !== Number(pid)) throw new Error(`resolved ${p.unixId()}, wanted ${pid}`);
  return p;
}

function bulk(spec, prop) {
  try {
    return spec[prop]();
  } catch (e) {
    return null;
  }
}

// Flatten nested arrays from a bulk query into [indexPath, value] pairs.
function flatten(value, path, out) {
  if (Array.isArray(value)) value.forEach((v, i) => flatten(v, path.concat(i), out));
  else out.push([path, value]);
  return out;
}

// Bulk values can hold object specifiers (a scroll bar's value), which do not convert.
function str(v) {
  return ["string", "number", "boolean"].includes(typeof v) ? String(v) : "";
}

// Yields one level at a time: [{path, role, desc, title, value}]
function* levels(win, maxDepth) {
  let spec = win;
  for (let d = 0; d < maxDepth; d++) {
    spec = spec.uiElements;
    const roles = bulk(spec, "role");
    if (!roles) return;
    const flat = flatten(roles, [], []);
    if (flat.length === 0) return;
    const get = (prop) => {
      const v = bulk(spec, prop);
      return v ? flatten(v, [], []).map(([, x]) => str(x)) : flat.map(() => "");
    };
    const desc = get("description");
    const title = get("title");
    const value = get("value");
    yield flat.map(([path, role], i) => ({
      path,
      role: str(role),
      desc: desc[i],
      title: title[i],
      value: value[i],
    }));
  }
}

function resolve(win, path) {
  return path.reduce((el, i) => el.uiElements[i], win);
}

// A label ending in "*" matches as a prefix: sidebar rows read "Name, idle, ...".
function matcher(text) {
  return text.endsWith("*") ? (s) => s.startsWith(text.slice(0, -1)) : (s) => s === text;
}

function count(win, text, role) {
  const want = matcher(text);
  let n = 0;
  for (const level of levels(win, MAX_DEPTH))
    n += level.filter((x) => (!role || x.role === role) && (want(x.desc) || want(x.title) || want(x.value))).length;
  return n;
}

function find(win, text, role) {
  const want = matcher(text);
  for (const level of levels(win, MAX_DEPTH)) {
    const hit = level.find(
      (n) => (!role || n.role === role) && (want(n.desc) || want(n.title) || want(n.value)),
    );
    if (hit) return { node: hit, el: resolve(win, hit.path) };
  }
  return null;
}

function windowId(pid) {
  const info = ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, 0)));
  let best = null;
  for (const w of info) {
    if (Number(w.kCGWindowOwnerPID) !== Number(pid) || Number(w.kCGWindowLayer) !== 0) continue;
    const area = Number(w.kCGWindowBounds.Width) * Number(w.kCGWindowBounds.Height);
    if (!best || area > best.area) best = { id: Number(w.kCGWindowNumber), area };
  }
  if (!best) throw new Error(`no on-screen window for ${pid}`);
  return String(best.id);
}

function run(argv) {
  const [cmd, pid, ...rest] = argv;
  if (cmd === "windowid") return windowId(pid);
  const p = proc(pid);
  if (cmd === "front") {
    p.frontmost = true;
    delay(0.3);
    const fronts = se.processes.frontmost();
    const owner = se.processes.unixId()[fronts.indexOf(true)];
    if (owner !== Number(pid)) throw new Error(`frontmost is ${owner}, not ${pid}`);
    return "ok";
  }
  const win = p.windows[0];
  if (cmd === "dump") {
    const out = [];
    for (const level of levels(win, Number(rest[0] || 30))) {
      for (const n of level) {
        if (n.desc || n.title || n.value)
          out.push(`${n.path.join(".")} ${n.role} | ${n.desc} | ${n.title} | ${n.value.slice(0, 60)}`);
      }
    }
    return out.join("\n");
  }
  if (cmd === "count") return String(count(win, rest[0], rest[1] || ""));
  if (cmd === "find" || cmd === "checked") {
    const hit = find(win, rest[0], rest[1] || "");
    if (!hit) throw new Error(`not found: ${rest[0]}`);
    if (cmd === "checked") return hit.node.value;
    // A row below the fold has an off-screen frame; a click there would miss.
    try {
      hit.el.actions.byName("AXScrollToVisible").perform();
      delay(0.3);
    } catch (e) {}
    const [x, y] = hit.el.position();
    const [w, h] = hit.el.size();
    return JSON.stringify({ role: hit.node.role, x, y, w, h, cx: Math.round(x + w / 2), cy: Math.round(y + h / 2) });
  }
  throw new Error(`unknown command ${cmd}`);
}
