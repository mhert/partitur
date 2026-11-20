// The smoke scripts' check of a packaged adapter: `<node> adapter-initialize.mjs <adapter-dir>`.
// It starts the adapter's entry under the same Node.js that runs this file, the way Partitur
// starts it, sends the ACP `initialize` request and passes only when the answer announces the
// widgets (`result._meta.partitur.widgets` is 1), which only the patched adapter does. The
// request needs neither Claude Code nor a login, so the check runs on any machine. It gives the
// adapter 60 s, prints the answer and the outcome, ends the adapter, and exits with status 1 on
// a failure.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { createInterface } from "node:readline";

const dir = process.argv[2];
if (!dir) {
  console.error("usage: node adapter-initialize.mjs <adapter-dir>");
  process.exit(2);
}
const entry = join(dir, "node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js");
if (!existsSync(entry)) {
  console.error(`adapter check: no adapter entry at ${entry}`);
  process.exit(1);
}

const adapter = spawn(process.execPath, [entry], { stdio: ["pipe", "pipe", "inherit"] });
let done = false;
function finish(code, message) {
  if (done) return;
  done = true;
  (code === 0 ? console.log : console.error)(message);
  adapter.kill();
  process.exit(code);
}

const timer = setTimeout(() => finish(1, "adapter check: no answer to initialize within 60 s"), 60_000);
timer.unref();
adapter.on("error", (error) => finish(1, `adapter check: the adapter did not start: ${error.message}`));
adapter.on("exit", (code, signal) =>
  finish(1, `adapter check: the adapter exited before it answered (${signal ?? code})`),
);

createInterface({ input: adapter.stdout }).once("line", (line) => {
  console.log(line);
  let widgets;
  try {
    widgets = JSON.parse(line)?.result?._meta?.partitur?.widgets;
  } catch (error) {
    finish(1, `adapter check: the answer is no JSON: ${error.message}`);
    return;
  }
  if (widgets === 1) {
    finish(0, `adapter check ok: ${entry} announces the widgets (Node.js ${process.version})`);
  } else {
    finish(1, `adapter check: the answer announces no widgets (_meta.partitur.widgets is ${widgets})`);
  }
});

adapter.stdin.write(
  `${JSON.stringify({
    jsonrpc: "2.0",
    id: 1,
    method: "initialize",
    params: { protocolVersion: 2, capabilities: {}, info: { name: "partitur-smoke", version: "0" } },
  })}\n`,
);
