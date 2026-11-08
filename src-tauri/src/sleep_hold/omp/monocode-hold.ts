// MonoCode's sleep hold for omp (alarms design 11). MonoCode writes this file
// and passes it with `-e` to each omp it starts; MONOCODE_HOLD_EXE names the
// MonoCode binary. A foreground bash call runs only when `hold` exits 0.
// Plain JavaScript, so MonoCode's tests can load it with Node.
import { spawn } from "node:child_process";

function hold(args, stdin) {
  if (!process.env.MONOCODE_HOLD_EXE) {
    return Promise.resolve({ code: null, stderr: "MonoCode's sleep hold binary is not set." });
  }
  return new Promise(resolve => {
    const child = spawn(process.env.MONOCODE_HOLD_EXE, ["hold", ...args], { stdio: ["pipe", "ignore", "pipe"] });
    let stderr = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", chunk => {
      stderr += chunk;
    });
    child.on("error", error => resolve({ code: null, stderr: String(error) }));
    child.on("close", code => resolve({ code, stderr }));
    child.stdin.on("error", () => {});
    child.stdin.end(stdin);
  });
}

export default function (pi) {
  // The app takes a ping only inside a turn, so one before the first turn is retried.
  let reported = false;
  const ping = async () => {
    if (!reported) {
      reported = (await hold(["ping"], "")).code === 0;
    }
  };
  pi.on("session_start", ping);
  pi.on("agent_start", ping);

  pi.on("tool_call", async event => {
    if (event.toolName !== "bash" || event.input.async === true) return;
    const payload = { toolCallId: event.toolCallId, command: event.input.command, async: false };
    const { code, stderr } = await hold(["--harness", "omp"], JSON.stringify(payload));
    if (code !== 0) {
      return { block: true, reason: stderr.trim() || "MonoCode's sleep hold refused this command." };
    }
  });
}
