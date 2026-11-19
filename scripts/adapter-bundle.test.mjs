// Behaviour tests of the widget patch, run by scripts/adapter-bundle.test.sh against a bundled
// adapter: `node adapter-bundle.test.mjs <package-dir>`. They import the patched modules' helpers
// and drive them with a stand-in for the session's Claude Code query, so no login is needed.
// Each check prints an `ok` or `FAIL` line; the exit status is 1 when any check failed.
import { pathToFileURL } from "node:url";
import { isDeepStrictEqual } from "node:util";
import { join } from "node:path";

const pkg = process.argv[2];
const agent = await import(pathToFileURL(join(pkg, "dist/v2/agent.js")).href);
const renderer = await import(pathToFileURL(join(pkg, "dist/tool-calls/renderer.js")).href);

let failed = false;
function check(desc, got, want) {
  if (isDeepStrictEqual(got, want)) {
    console.log(`ok   - ${desc}`);
  } else {
    console.log(`FAIL - ${desc} (got ${JSON.stringify(got)}, want ${JSON.stringify(want)})`);
    failed = true;
  }
}
async function rejection(promise) {
  try {
    await promise;
    return undefined;
  } catch (error) {
    return { code: error.code, message: error.message };
  }
}
function thrown(run) {
  try {
    run();
    return undefined;
  } catch (error) {
    return { code: error.code };
  }
}

/** An agent whose one session `s1` runs `query`; any other session is not found. */
function agentWith(query) {
  return {
    session(sessionId) {
      if (sessionId !== "s1") throw Object.assign(new Error("Resource not found"), { code: -32002 });
      return { query };
    },
  };
}

// The `ui://` resource of a tool's `_meta`.
check("the ui resource is read", agent.partiturResourceUri({ ui: { resourceUri: "ui://a/b" } }), "ui://a/b");
check("the flat key is read", agent.partiturResourceUri({ "ui/resourceUri": "ui://a/c" }), "ui://a/c");
check("the ui resource wins over the flat key",
  agent.partiturResourceUri({ ui: { resourceUri: "ui://a/b" }, "ui/resourceUri": "ui://a/c" }), "ui://a/b");
check("another scheme is no widget", agent.partiturResourceUri({ ui: { resourceUri: "https://a/b" } }), undefined);
check("no _meta is no widget", agent.partiturResourceUri(undefined), undefined);

// `_partitur/widget_tools`.
const statuses = [
  {
    name: "atlassian",
    tools: [
      { name: "getJiraIssue", _meta: { ui: { resourceUri: "ui://jira/issue", visibility: ["model", 7, "app"] } } },
      { name: "legacy", _meta: { "ui/resourceUri": "ui://jira/legacy" } },
      { name: "plain" },
      { name: "web", _meta: { ui: { resourceUri: "https://example.com" } } },
    ],
  },
  { name: "offline" },
];
check("the tools with a widget are listed",
  await agent.partiturWidgetTools(agentWith({ mcpServerStatus: async () => statuses }), { sessionId: "s1" }),
  {
    tools: [
      { server: "atlassian", tool: "getJiraIssue", resourceUri: "ui://jira/issue", visibility: ["model", "app"] },
      { server: "atlassian", tool: "legacy", resourceUri: "ui://jira/legacy", visibility: [] },
    ],
  });
check("a failed server status is an error with the SDK's message",
  await rejection(agent.partiturWidgetTools(
    agentWith({ mcpServerStatus: async () => { throw new Error("status failed"); } }), { sessionId: "s1" })),
  { code: -32603, message: "status failed" });
check("an unknown session is not found",
  await rejection(agent.partiturWidgetTools(agentWith({}), { sessionId: "s2" })),
  { code: -32002, message: "Resource not found" });
check("a session without a query fails",
  (await rejection(agent.partiturWidgetTools(agentWith(undefined), { sessionId: "s1" })))?.code, -32603);

// `_partitur/read_widget`.
const read = (contents) => agent.partiturReadWidget(
  agentWith({ readMcpResource: async (server, uri) => (server === "w" && uri === "ui://w/x" ? { contents } : { contents: [] }) }),
  { sessionId: "s1", server: "w", uri: "ui://w/x" });
check("a text resource is read with its _meta",
  await read([{ uri: "ui://w/x", mimeType: "text/html;profile=mcp-app", text: "<p>a</p>", _meta: { ui: { prefersBorder: true } } },
    { uri: "ui://w/y", text: "second" }]),
  { mimeType: "text/html;profile=mcp-app", html: "<p>a</p>", meta: { ui: { prefersBorder: true } } });
check("a blob resource is decoded from base64",
  await read([{ uri: "ui://w/x", mimeType: "text/html", blob: Buffer.from("<p>ü</p>").toString("base64") }]),
  { mimeType: "text/html", html: "<p>ü</p>", meta: {} });
check("a resource without content fails",
  (await rejection(read([])))?.code, -32603);
check("a failed read is an error with the SDK's message",
  await rejection(agent.partiturReadWidget(
    agentWith({ readMcpResource: async () => { throw new Error("Server not found: w"); } }),
    { sessionId: "s1", server: "w", uri: "ui://w/x" })),
  { code: -32603, message: "Server not found: w" });

// The params of both requests.
check("widget_tools takes a session", agent.parsePartiturWidgetTools({ sessionId: "s1", other: 1 }), { sessionId: "s1" });
check("widget_tools needs a session", thrown(() => agent.parsePartiturWidgetTools({})), { code: -32602 });
check("read_widget takes a session, a server and a uri",
  agent.parsePartiturReadWidget({ sessionId: "s1", server: "w", uri: "ui://w/x" }),
  { sessionId: "s1", server: "w", uri: "ui://w/x" });
check("read_widget needs a uri", thrown(() => agent.parsePartiturReadWidget({ sessionId: "s1", server: "w", uri: "" })),
  { code: -32602 });

// `_meta.partitur.callToolResult` of an MCP tool's result.
const mcp = { id: "t1", name: "mcp__w__show", input: {} };
const content = [{ type: "text", text: "{\"temp\":21}" }];
check("a spread result keeps its structured content and _meta",
  renderer.partiturCallToolResult(mcp, { is_error: false },
    { content, structuredContent: { temp: 21 }, _meta: { m: 1 }, resourceLinks: [] }),
  { content, structuredContent: { temp: 21 }, _meta: { m: 1 }, isError: false });
check("a bare content array is the content",
  renderer.partiturCallToolResult(mcp, { is_error: true }, content),
  { content, isError: true });
check("a string result is one text block",
  renderer.partiturCallToolResult(mcp, {}, "sunny"),
  { content: [{ type: "text", text: "sunny" }], isError: false });
check("a result without tool_use_result has none",
  renderer.partiturCallToolResult(mcp, {}, undefined), undefined);
check("a tool that is not MCP has none",
  renderer.partiturCallToolResult({ id: "t2", name: "Read", input: {} }, {}, content), undefined);

process.exit(failed ? 1 : 0);
