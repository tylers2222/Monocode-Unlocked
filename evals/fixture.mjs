// Seeds one GigaEval instance for a whole run, while the app is stopped.
//
//   node fixture.mjs <k> <smoke|full>
//
// Every trial (case x repeat) gets its own Mono, chat session and projects,
// so trials share no row or session and can run at the same time. A sender
// Mono ("Beta") holds a habit run whose app token sends every message.
// A `lead` trial gets only projects; they join Beta's, where Beta starts the lead.
// The trial plan goes to $EVAL_OUT_DIR/trials.json for provider.mjs.
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdirSync, readdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import yaml from "js-yaml";

export const SENDER = "sender";
const HOLD_SECONDS = 3600;

export function env() {
  const home = process.env.GIGA_EVAL_HOME;
  const out = process.env.EVAL_OUT_DIR;
  if (!home || !out) throw new Error("GIGA_EVAL_HOME and EVAL_OUT_DIR must be set (evals/run.sh sets them)");
  const data = join(home, "Library/Application Support/com.monocode.desktop.gigaeval");
  return { home, out, data, db: join(data, "monocode.db"), manifest: join(out, "trials.json") };
}

const CASES_DIR = process.env.EVAL_CASES_DIR ?? join(dirname(fileURLToPath(import.meta.url)), "cases");

/** An event as the provider runs it, or undefined when it has none of the four shapes. */
function eventOf(e) {
  if (typeof e === "string") return { send: e };
  const keys = Object.keys(e ?? {}).sort().join();
  if (keys === "ask,due_minutes" && typeof e.ask === "string" && Number.isInteger(e.due_minutes) && e.due_minutes > 0) return e;
  if (keys === "answer" && typeof e.answer === "string") return e;
  if (keys === "advance_minutes" && Number.isInteger(e.advance_minutes) && e.advance_minutes > 0) return e;
}

function problemOf({ pending, fixture, events }) {
  if (pending !== undefined && !(Array.isArray(pending) && pending.every((t) => /^ADE-\d+$/.test(t)))) return "pending must be a list of ADE-<n>";
  if (!["mono", "lead"].includes(fixture.agent)) return `fixture.agent must be mono or lead, got ${fixture.agent}`;
  if (fixture.agent === "lead" && !fixture.projects?.A) return "a lead agent needs project role A";
  if (events.includes(undefined)) return "each event must be a string, {ask, due_minutes}, {answer} or {advance_minutes}";
  if (fixture.agent === "mono" && events.some((e) => !("send" in e))) return "a mono agent takes only string events";
  if (events.filter((e) => "ask" in e).length > 1) return "a case has at most one ask event";
}

function loadCase(path) {
  const c = yaml.load(readFileSync(path, "utf8"));
  for (const key of ["id", "version", "grader", "fixture", "events"]) {
    if (c[key] == null) throw new Error(`${path}: missing ${key}`);
  }
  if (!/^[\w-]+:\w+$/.test(c.grader)) throw new Error(`${path}: grader must be <file>:<export>, got ${c.grader}`);
  const events = Array.isArray(c.events) ? c.events.map(eventOf) : [undefined];
  const loaded = { ...c, tier: c.tier ?? "core", fixture: { agent: "mono", ...c.fixture }, events };
  const problem = problemOf(loaded);
  if (problem) throw new Error(`${path}: ${problem}`);
  return loaded;
}

/** Every case, sorted by file name; smoke keeps the core tier. */
function casesIn(tier) {
  return readdirSync(CASES_DIR)
    .filter((f) => f.endsWith(".yaml"))
    .sort()
    .map((f) => loadCase(join(CASES_DIR, f)))
    .filter((c) => tier === "full" || c.tier === "core");
}

/** The cases that run: a case with `pending` waits on unmerged code. */
export const loadCases = (tier = "full") => casesIn(tier).filter((c) => !c.pending);

/** The message text with {{A}}-style project roles replaced by the trial's project names. */
function render(text, names) {
  return text.replace(/\{\{(\w+)\}\}/g, (_, role) => {
    if (!(role in names)) throw new Error(`unknown project role ${role} in "${text}"`);
    return names[role];
  });
}

const sql = (value) => (value == null ? "NULL" : `'${String(value).replaceAll("'", "''")}'`);

function gitProject(dir, files = {}) {
  mkdirSync(dir, { recursive: true });
  const git = (...args) => execFileSync("git", ["-C", dir, ...args], { stdio: "ignore" });
  git("init", "-q", "-b", "main");
  writeFileSync(join(dir, "README.md"), `# ${dir.split("/").pop()}\n`);
  for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text);
  git("add", "-A");
  git("-c", "user.name=evals", "-c", "user.email=evals@local", "commit", "-qm", "init");
}

function seed(k, tier) {
  const e = env();
  const harness = process.env.EVAL_HARNESS ?? "omp";
  const model = process.env.EVAL_MODEL ?? `${harness}:default`;
  const now = Date.now();
  const statements = [];
  const mono = (id, name, projects, sessionId, order) =>
    statements.push(
      `INSERT INTO monos (id,name,role,data_json,sort_order,created_at,updated_at) VALUES (${sql(id)},${sql(name)},'mono',` +
        `${sql(JSON.stringify({ name, mascot: "crab", color: "#3b82f6", projects, sessionId }))},${order},${now},${now});`,
    );
  const chat = (id, title, h, m) =>
    statements.push(
      `INSERT INTO sessions (id,cwd,harness,model,model_settings,runtime_mode,title,blocks_json,created_at,updated_at) ` +
        `VALUES (${sql(id)},${sql(e.home)},${sql(h)},${sql(m)},'{}','auto',${sql(title)},'[]',${now},${now});`,
    );

  const scratch = join(e.home, "giga-eval-scratch");
  const senderSession = randomUUID();
  chat(senderSession, "Beta", "omp", "omp:default");
  const senderProjects = [scratch];
  for (const c of casesIn(tier)) if (c.pending) console.log(`pending ${c.id}: waits on ${c.pending.join(", ")}`);

  const trials = {};
  let order = 1;
  for (const c of loadCases(tier)) {
    const short = c.id.split("-")[0];
    trials[c.id] = [];
    for (let i = 0; i < k; i++) {
      const trial = `${short}-${i}`;
      const projects = {};
      for (const [role, name] of Object.entries(c.fixture.projects)) {
        projects[role] = join(e.home, "trials", trial, name);
        gitProject(projects[role], c.fixture.files?.[role]);
      }
      const events = c.events.map((ev) => {
        const text = ["send", "ask", "answer"].find((key) => key in ev);
        return text ? { ...ev, [text]: render(ev[text], c.fixture.projects) } : ev;
      });
      if (c.fixture.agent === "lead") {
        senderProjects.push(...Object.values(projects));
        trials[c.id].push({ trial, agent: "lead", projects, events });
        continue;
      }
      const session = randomUUID();
      const name = `Alpha ${trial}`;
      chat(session, name, harness, model);
      mono(trial, name, c.fixture.mono_projects.map((role) => projects[role]), session, order++);
      (c.fixture.milestones ?? []).forEach((m, n) =>
        statements.push(
          `INSERT INTO milestones (id,project_path,title,note,status,target,sort_order,created_by,created_at,updated_at) ` +
            `VALUES (${sql(randomUUID())},${sql(projects[m.project])},${sql(m.title)},'',${sql(m.status ?? "idea")},NULL,${n},'user',${now},${now});`,
        ),
      );
      trials[c.id].push({ trial, agent: "mono", mono: trial, session, projects, events });
    }
  }
  mono(SENDER, "Beta", senderProjects, senderSession, 0);
  execFileSync("sqlite3", [e.db], { input: `BEGIN;\n${statements.join("\n")}\nCOMMIT;\n` });

  const habits = join(e.data, "monos", SENDER);
  mkdirSync(habits, { recursive: true });
  writeFileSync(
    join(habits, "habits.json"),
    JSON.stringify([
      {
        id: `hold-${now}`,
        name: "Hold",
        instructions: `Run exactly this shell command and nothing else: sleep ${HOLD_SECONDS}. When it finishes, reply with the single word held.`,
        schedule: { scheduleKind: "daily", time: "09:00", minute: 0, dayOfWeek: 1 },
        enabled: false,
        createdAt: 0,
        nextRunAt: 0,
        runRequested: true,
      },
    ]),
  );
  mkdirSync(e.out, { recursive: true });
  writeFileSync(e.manifest, JSON.stringify({ harness, model, db: e.db, trials }, null, 2));
  console.log(`seeded ${Object.values(trials).flat().length} trials (${harness} ${model}) -> ${e.manifest}`);
}

// Real paths: argv[1] may run through a symlink (/tmp) or hold a space.
if (process.argv[1] && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [k, tier] = process.argv.slice(2);
  if (!Number(k) || !["smoke", "full"].includes(tier)) {
    console.error("usage: node fixture.mjs <k> <smoke|full>");
    process.exit(2);
  }
  seed(Number(k), tier);
}
