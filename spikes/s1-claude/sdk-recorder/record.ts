// Spike only (D8): records reference frames of the TypeScript Agent SDK.
// It never ships. Usage (live, owner or recorder only):
//
//   cd spikes/s1-claude/sdk-recorder && bun install && bun record.ts [scenario|all]
//
// Each scenario runs in a temporary directory with haiku and `--safe-mode`.
// `tee-claude.mjs` sits between the SDK and the real `claude` CLI and logs every
// frame. The scrubbed log becomes `../fixtures/sdk/<scenario>.jsonl`.
import { query, type Query, type SDKUserMessage } from "@anthropic-ai/claude-agent-sdk";
import { appendFileSync, mkdtempSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir, homedir, userInfo } from "node:os";
import { join, resolve } from "node:path";

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

/** Parses one JSON text; a parse error names the source. */
function parseJson(text: string, source: string): Json {
  try {
    return JSON.parse(text) as Json;
  } catch (e) {
    throw new Error(`${source}: invalid JSON: ${e}`);
  }
}

const SDK_PACKAGE = parseJson(
  readFileSync(join(import.meta.dir, "node_modules/@anthropic-ai/claude-agent-sdk/package.json"), "utf8"),
  "claude-agent-sdk package.json",
);
const SDK_VERSION =
  SDK_PACKAGE && typeof SDK_PACKAGE === "object" && !Array.isArray(SDK_PACKAGE) ? String(SDK_PACKAGE.version) : "unknown";
const TEE = resolve(import.meta.dir, "tee-claude.mjs");
const OUT_DIR = resolve(import.meta.dir, "../fixtures/sdk");
const TURN_LIMIT_MS = 120_000;
const BASH_TOUCH =
  "Use the Bash tool to run exactly this command: touch fd-approve.txt && echo approve-ok\nRun it in the foreground. Then reply with the command output only.";

/** A prompt stream that the scenario feeds while the query runs. */
class Prompts implements AsyncIterable<SDKUserMessage> {
  private items: SDKUserMessage[] = [];
  private wake: (() => void) | null = null;
  private closed = false;
  push(text: string) {
    this.items.push({ type: "user", message: { role: "user", content: text }, parent_tool_use_id: null });
    this.wake?.();
  }
  close() {
    this.closed = true;
    this.wake?.();
  }
  async *[Symbol.asyncIterator]() {
    while (true) {
      const next = this.items.shift();
      if (next) {
        yield next;
        continue;
      }
      if (this.closed) return;
      await new Promise<void>((r) => (this.wake = r));
      this.wake = null;
    }
  }
}

type Decide = (tool: string, input: Record<string, unknown>) => Promise<
  | { behavior: "allow"; updatedInput: Record<string, unknown> }
  | { behavior: "deny"; message: string }
>;

/** One SDK session: a query, its prompt stream, and a message pump. */
class Session {
  readonly prompts = new Prompts();
  readonly q: Query;
  private results = 0;
  private waiters: { n: number; resolve: () => void }[] = [];
  private onMsg: ((m: any) => void) | null = null;
  sessionId: string | undefined;
  done: Promise<void>;

  constructor(ctx: Ctx, opts: { mode?: string; resume?: string; decide?: Decide }) {
    const decide: Decide = opts.decide ?? (async (_t, input) => ({ behavior: "allow", updatedInput: input }));
    this.q = query({
      prompt: this.prompts,
      options: {
        cwd: ctx.workdir,
        model: "haiku",
        permissionMode: (opts.mode ?? "default") as any,
        includePartialMessages: true,
        pathToClaudeCodeExecutable: TEE,
        executable: "node",
        extraArgs: { "safe-mode": null },
        resume: opts.resume,
        env: { ...process.env, FD_TEE_LOG: ctx.log },
        canUseTool: async (tool, input) => {
          ctx.note(`canUseTool ${tool}`);
          return await decide(tool, input);
        },
      },
    });
    this.done = (async () => {
      for await (const m of this.q) {
        if (m.type === "system" && (m as any).subtype === "init") this.sessionId = (m as any).session_id;
        this.onMsg?.(m);
        if (m.type === "result") {
          this.results += 1;
          for (const w of this.waiters.filter((w) => w.n <= this.results)) w.resolve();
          this.waiters = this.waiters.filter((w) => w.n > this.results);
        }
      }
    })();
  }

  /** Resolves when `n` results arrived in total. */
  results_(n: number): Promise<void> {
    if (this.results >= n) return Promise.resolve();
    return withTimeout(new Promise((resolve) => this.waiters.push({ n, resolve })), `result #${n}`);
  }

  /** Resolves on the first message that matches `pred`. */
  until(pred: (m: any) => boolean): Promise<void> {
    return withTimeout(
      new Promise<void>((resolve) => {
        this.onMsg = (m) => {
          if (pred(m)) {
            this.onMsg = null;
            resolve();
          }
        };
      }),
      "message",
    );
  }

  async close() {
    this.prompts.close();
    await withTimeout(this.done, "session end");
  }
}

function withTimeout<T>(p: Promise<T>, what: string): Promise<T> {
  return Promise.race([
    p,
    new Promise<T>((_, reject) => setTimeout(() => reject(new Error(`timeout waiting for ${what}`)), TURN_LIMIT_MS)),
  ]);
}

class Ctx {
  workdir = realpathSync(mkdtempSync(join(tmpdir(), "fd-s1-sdk-")));
  log = join(this.workdir, "..", `${this.workdir.split("/").pop()}.tee.jsonl`);
  note(text: string) {
    appendFileSync(this.log, JSON.stringify({ dir: "note", text }) + "\n");
  }
}

const allow: Decide = async (_t, input) => ({ behavior: "allow", updatedInput: input });
const deny: Decide = async () => ({ behavior: "deny", message: "The owner denied this command." });

const SCENARIOS: Record<string, { notes: string; run: (c: Ctx) => Promise<void> }> = {
  simple: {
    notes: "One prompt, one text reply.",
    run: async (c) => {
      const s = new Session(c, {});
      s.prompts.push("Reply with exactly the word: pong");
      await s.results_(1);
      await s.close();
    },
  },
  multi_turn: {
    notes: "Two prompts; the second waits for the first result.",
    run: async (c) => {
      const s = new Session(c, {});
      s.prompts.push("Remember the number 42. Reply with exactly: ok");
      await s.results_(1);
      s.prompts.push("What number did I ask you to remember? Reply with the number only.");
      await s.results_(2);
      await s.close();
    },
  },
  queued_turn: {
    notes: "Two prompts written at once; the second waits in the CLI queue.",
    run: async (c) => {
      const s = new Session(c, {});
      s.prompts.push("Reply with exactly the word: first");
      s.prompts.push("Reply with exactly the word: second");
      await s.results_(2);
      await s.close();
    },
  },
  turn_interrupt: {
    notes: "Interrupt after the first text delta, then one more prompt.",
    run: async (c) => {
      const s = new Session(c, {});
      const delta = s.until((m) => m.type === "stream_event" && m.event?.delta?.type === "text_delta");
      s.prompts.push(
        "Do not use any tools. Write the numbers from 1 to 400 as plain text, one number per line, and nothing else.",
      );
      await delta;
      c.note("host interrupt");
      await s.q.interrupt();
      await s.results_(1);
      s.prompts.push("Reply with exactly the word: after");
      await s.results_(2);
      await s.close();
    },
  },
  message_steering: {
    notes: "A second prompt while an approved foreground command runs.",
    run: async (c) => {
      let steer = () => {};
      const s = new Session(c, {
        decide: async (_t, input) => {
          setTimeout(() => steer(), 1500);
          return { behavior: "allow", updatedInput: input };
        },
      });
      steer = () => {
        c.note("host steer message");
        s.prompts.push("Also end your reply with the word: banana");
      };
      s.prompts.push(
        "Use the Bash tool to run exactly this command in the foreground (not in the background): touch fd-steer.txt && python3 -c \"import time; time.sleep(6); print('step-one')\"\nThen reply with the command output.",
      );
      await s.results_(1);
      // A steer message that the CLI queued instead of joining runs as a second turn.
      await new Promise((r) => setTimeout(r, 3000));
      await s.close();
    },
  },
  provider_thread_resume: {
    notes: "Close the session, resume it in a new process, ask about the first turn.",
    run: async (c) => {
      const s = new Session(c, {});
      s.prompts.push("Remember the code word: walnut. Reply with exactly: ok");
      await s.results_(1);
      await s.close();
      if (!s.sessionId) throw new Error("no session id");
      const r = new Session(c, { resume: s.sessionId });
      r.prompts.push("What was the code word? Reply with the word only.");
      await r.results_(1);
      await r.close();
    },
  },
  tool_call_approve: {
    notes: "A Bash command that writes a file; the host allows it.",
    run: async (c) => {
      const s = new Session(c, { decide: allow });
      s.prompts.push(BASH_TOUCH);
      await s.results_(1);
      await s.close();
    },
  },
  tool_call_deny: {
    notes: "A Bash command that writes a file; the host denies it.",
    run: async (c) => {
      const s = new Session(c, { decide: deny });
      s.prompts.push(BASH_TOUCH);
      await s.results_(1);
      await s.close();
    },
  },
  tool_call_edit_input: {
    notes: "The host allows the command with an edited input.",
    run: async (c) => {
      const s = new Session(c, {
        decide: async (_t, input) => ({
          behavior: "allow",
          updatedInput: { ...input, command: "touch fd-edited.txt && echo edited-ok" },
        }),
      });
      s.prompts.push(BASH_TOUCH);
      await s.results_(1);
      await s.close();
    },
  },
  set_permission_mode: {
    notes: "Switch to plan mode before the first prompt.",
    run: async (c) => {
      const s = new Session(c, {});
      await s.q.setPermissionMode("plan");
      c.note("host set_permission_mode plan done");
      s.prompts.push("Reply with exactly the word: planned");
      await s.results_(1);
      await s.close();
    },
  },
};

// --- scrubbing (same rules as `spike-canon::fixture::Scrubber` in the Rust recorder) ---
const REDACT = new Set(["account", "email", "emailAddress", "organization", "messaging_socket_path"]);

function scrubber(workdir: string) {
  const pairs: [string, string][] = [
    [workdir, "<workdir>"],
    [homedir(), "<home>"],
    [userInfo().username, "<user>"],
  ];
  pairs.sort((a, b) => b[0].length - a[0].length);
  const str = (s: string) => pairs.reduce((acc, [from, to]) => acc.split(from).join(to), s);
  const val = (v: Json): Json => {
    if (typeof v === "string") return str(v);
    if (Array.isArray(v)) return v.map(val);
    if (v && typeof v === "object") {
      return Object.fromEntries(
        Object.entries(v).map(([k, x]): [string, Json] => [str(k), REDACT.has(k) ? "<redacted>" : val(x)]),
      );
    }
    return v;
  };
  return val;
}

async function record(name: string) {
  const sc = SCENARIOS[name];
  if (!sc) throw new Error(`unknown scenario ${name}`);
  const c = new Ctx();
  writeFileSync(c.log, "");
  let failed: unknown = null;
  try {
    await withTimeout(sc.run(c), "scenario");
  } catch (e) {
    failed = e;
    c.note(`FAILED: ${e}`);
  }
  const scrub = scrubber(c.workdir);
  const records = readFileSync(c.log, "utf8")
    .split("\n")
    .filter((l) => l.trim() !== "")
    .map((l, i) => scrub(parseJson(l, `tee log line ${i + 1}`)));
  const header = {
    fixture: {
      provider: "claude",
      scenario: name,
      cli_version: process.env.FD_CLI_VERSION ?? "2.1.289",
      recorder: `sdk-recorder (@anthropic-ai/claude-agent-sdk ${SDK_VERSION})`,
      model: "haiku",
      notes: sc.notes,
    },
  };
  const file = join(OUT_DIR, failed ? `${name}.failed.jsonl` : `${name}.jsonl`);
  writeFileSync(file, [header, ...records].map((r) => JSON.stringify(r)).join("\n") + "\n");
  console.log(`${failed ? "FAILED" : "ok    "} ${name.padEnd(24)} ${file}`);
}

const which = process.argv[2] ?? "all";
for (const name of which === "all" ? Object.keys(SCENARIOS) : [which]) {
  await record(name);
}
process.exit(0);
