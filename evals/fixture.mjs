// Seeds one GigaEval instance for a whole run, while the app is stopped.
//
//   node fixture.mjs <k> <smoke|full>
//
// Every trial (case x repeat) gets its own Mono, chat session and projects,
// so trials share no row or session and can run at the same time. A sender
// Mono ("Beta") holds a habit run whose app token sends every message.
// The trial plan goes to $EVAL_OUT_DIR/trials.json for provider.mjs.
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
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

function loadCase(path) {
  const c = yaml.load(readFileSync(path, "utf8"));
  for (const key of ["id", "version", "grader", "fixture", "events"]) {
    if (c[key] == null) throw new Error(`${path}: missing ${key}`);
  }
  return { ...c, tier: c.tier ?? "core" };
}

/** Every case, sorted by file name; smoke keeps the core tier. */
export function loadCases(tier = "full") {
  return readdirSync(CASES_DIR)
    .filter((f) => f.endsWith(".yaml"))
    .sort()
    .map((f) => loadCase(join(CASES_DIR, f)))
    .filter((c) => tier === "full" || c.tier === "core");
}

/** The message text with {{A}}-style project roles replaced by the trial's project names. */
function render(text, names) {
  return text.replace(/\{\{(\w+)\}\}/g, (_, role) => {
    if (!(role in names)) throw new Error(`unknown project role ${role} in "${text}"`);
    return names[role];
  });
}

const sql = (value) => (value == null ? "NULL" : `'${String(value).replaceAll("'", "''")}'`);

function gitProject(dir) {
  mkdirSync(dir, { recursive: true });
  const git = (...args) => execFileSync("git", ["-C", dir, ...args], { stdio: "ignore" });
  git("init", "-q", "-b", "main");
  writeFileSync(join(dir, "README.md"), `# ${dir.split("/").pop()}\n`);
  git("add", "README.md");
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
  mono(SENDER, "Beta", [scratch], senderSession, 0);

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
        gitProject(projects[role]);
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
      trials[c.id].push({
        trial,
        mono: trial,
        session,
        projects,
        messages: c.events.map((text) => render(text, c.fixture.projects)),
      });
    }
  }
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

if (import.meta.url === `file://${process.argv[1]}`) {
  const [k, tier] = process.argv.slice(2);
  if (!Number(k) || !["smoke", "full"].includes(tier)) {
    console.error("usage: node fixture.mjs <k> <smoke|full>");
    process.exit(2);
  }
  seed(Number(k), tier);
}
