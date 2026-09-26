#!/usr/bin/env node
// thunder-pi-bridge sidecar: NDJSON protocol over stdio.
//
// Rust → Node: {cmd:"health"|"stream"|"cancel"|"list_models"|"shutdown", …}
// Node → Rust: {id, type:"ready"|"text_delta"|"reasoning_delta"|"done"|"error"|"models"}
//              | {type:"fatal", message}
//
// pi-ai resolution order:
//   1. $THUNDER_PI_AI_PATH          — explicit package dir (tests / offline setups)
//   2. ./node_modules (bridge dir)  — normal case after bootstrap `npm install`
//   3. global pi installations      — fnm / nvm / npm-global / homebrew layouts
//   4. auto `npm install`           — unless THUNDER_BRIDGE_AUTOINSTALL=0

import { execSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import readline from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";
import * as bundledPiAi from "@earendil-works/pi-ai/compat";

const BRIDGE_DIR = path.dirname(fileURLToPath(import.meta.url));
const MIN_NODE_MAJOR = 22;
const PI_PKG = "@earendil-works/pi-ai";

function fatal(message) {
	process.stdout.write(JSON.stringify({ type: "fatal", message }) + "\n");
	// Give the parent a chance to read the line before exit.
	setTimeout(() => process.exit(1), 50).unref();
}

function send(obj) {
	process.stdout.write(JSON.stringify(obj) + "\n");
}

// ---------------------------------------------------------------------------
// pi-ai loading
// ---------------------------------------------------------------------------

function importCandidatesFromDir(dir) {
	// Support both a published layout (dist/compat.js) and minimal fakes.
	return ["dist/compat.js", "compat.js", "index.js"].map((rel) => path.join(dir, rel));
}

async function importFromDir(dir) {
	for (const entry of importCandidatesFromDir(dir)) {
		if (fs.existsSync(entry)) {
			const mod = await import(pathToFileURL(entry).href);
			return { mod, dir };
		}
	}
	throw new Error(`no pi-ai entrypoint found under ${dir}`);
}

function globalCandidateDirs() {
	const home = os.homedir();
	const out = [];
	const addTree = (root) => {
		try {
			for (const ent of fs.readdirSync(root, { withFileTypes: true })) {
				if (!ent.isDirectory()) continue;
				const prefix = path.join(root, ent.name);
				// Standalone global install of pi-ai
				out.push(path.join(prefix, "lib", "node_modules", PI_PKG));
				out.push(path.join(prefix, "node_modules", PI_PKG));
				// pi-coding-agent bundles pi-ai in its own node_modules
				out.push(
					path.join(prefix, "lib", "node_modules", "@earendil-works", "pi-coding-agent", "node_modules", PI_PKG),
				);
				out.push(
					path.join(prefix, "node_modules", "@earendil-works", "pi-coding-agent", "node_modules", PI_PKG),
				);
			}
		} catch {
			// missing dir — fine
		}
	};
	addTree(path.join(home, ".local", "share", "fnm", "node-versions"));
	addTree(path.join(home, ".nvm", "versions", "node"));
	addTree(path.join(home, ".npm-global", "lib", "node_modules"));
	addTree("/usr/local/lib/node_modules");
	addTree("/opt/homebrew/lib/node_modules");
	return out;
}

function readVersion(dir) {
	try {
		const pkg = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8"));
		return typeof pkg.version === "string" ? pkg.version : "unknown";
	} catch {
		return "unknown";
	}
}

async function loadPiAi() {
	// 1. explicit override
	const envPath = process.env.THUNDER_PI_AI_PATH;
	if (envPath) {
		const loaded = await importFromDir(envPath);
		return { ...loaded, version: readVersion(envPath) };
	}

	// 2. Pre-bundled pi-ai module (zero runtime dependencies, instant boot)
	if (bundledPiAi && typeof bundledPiAi.stream === "function") {
		return { mod: bundledPiAi, dir: BRIDGE_DIR, version: "0.87.1" };
	}

	// 3. bridge-local node_modules
	try {
		const mod = await import(`${PI_PKG}/compat`);
		// exports map hides package.json from `resolve`; read it directly.
		const localDir = path.join(BRIDGE_DIR, "node_modules", PI_PKG);
		const version = fs.existsSync(path.join(localDir, "package.json"))
			? readVersion(localDir)
			: "unknown";
		return { mod, dir: localDir, version };
	} catch {
		// fall through
	}

	// 3. global installations
	for (const dir of globalCandidateDirs()) {
		try {
			const loaded = await importFromDir(dir);
			return { ...loaded, version: readVersion(dir) };
		} catch {
			// try next
		}
	}

	// 4. auto-install into the bridge dir
	if (process.env.THUNDER_BRIDGE_AUTOINSTALL !== "0") {
		if (!fs.existsSync(path.join(BRIDGE_DIR, "package.json"))) {
			throw new Error(`cannot auto-install: ${BRIDGE_DIR} has no package.json`);
		}
		execSync("npm install --no-audit --no-fund --loglevel=error", {
			cwd: BRIDGE_DIR,
			stdio: "inherit",
			timeout: 180_000,
		});
		const mod = await import(`${PI_PKG}/compat`);
		return { mod, dir: null, version: "unknown" };
	}

	throw new Error(
		`pi-ai not found. Set $THUNDER_PI_AI_PATH, run \`npm install\` in ${BRIDGE_DIR}, ` +
			"or unset THUNDER_BRIDGE_AUTOINSTALL=0 to allow automatic installation.",
	);
}

// ---------------------------------------------------------------------------
// thunder → pi mapping
// ---------------------------------------------------------------------------

function zeroUsage() {
	return { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };
}

/** thunder ChatMessage[] (OpenAI-shaped, serde tag="role") → pi Context */
function thunderToPiContext(messages, model) {
	const piMessages = [];
	const toolNames = new Map(); // tool_call_id → name, for Tool results
	let systemPrompt;

	for (const m of messages ?? []) {
		const ts = Date.now();
		switch (m.role) {
			case "system": {
				if (systemPrompt === undefined && piMessages.length === 0) {
					systemPrompt = m.content ?? "";
				} else {
					piMessages.push({ role: "system", content: m.content ?? "", timestamp: ts });
				}
				break;
			}
			case "user": {
				piMessages.push({ role: "user", content: m.content ?? "", timestamp: ts });
				break;
			}
			case "assistant": {
				const blocks = [];
				if (typeof m.content === "string" && m.content.length > 0) {
					blocks.push({ type: "text", text: m.content });
				}
				for (const tc of m.tool_calls ?? []) {
					const name = tc.function?.name ?? "unknown";
					let argumentsObject = {};
					try {
						argumentsObject = JSON.parse(tc.function?.arguments ?? "{}");
					} catch {
						argumentsObject = {};
					}
					blocks.push({ type: "toolCall", id: tc.id, name, arguments: argumentsObject });
					toolNames.set(tc.id, name);
				}
				piMessages.push({
					role: "assistant",
					content: blocks,
					api: model.api,
					provider: model.provider,
					model: model.id,
					usage: zeroUsage(),
					stopReason: "stop",
					timestamp: ts,
				});
				break;
			}
			case "tool": {
				const toolName = m.name ?? toolNames.get(m.tool_call_id) ?? "unknown";
				piMessages.push({
					role: "toolResult",
					toolCallId: m.tool_call_id,
					toolName,
					content: [{ type: "text", text: m.content ?? "" }],
					isError: false,
					timestamp: ts,
				});
				break;
			}
			default:
				// unknown role — drop rather than fail the whole stream
				break;
		}
	}

	const context = { messages: piMessages };
	if (systemPrompt !== undefined) context.systemPrompt = systemPrompt;
	return context;
}

/** thunder ToolDefinition[] (OpenAI-shaped) → pi Tool[] */
function thunderToPiTools(tools) {
	return (tools ?? [])
		.map((t) => ({
			name: t.function?.name ?? t.name,
			description: t.function?.description ?? "",
			parameters: t.function?.parameters ?? { type: "object", properties: {} },
		}))
		.filter((t) => typeof t.name === "string" && t.name.length > 0);
}

// ---------------------------------------------------------------------------
// pi events → thunder protocol
// ---------------------------------------------------------------------------

function finishReasonFromPi(reason) {
	if (reason === "toolUse" || reason === "deferred") return "tool_calls";
	if (reason === "length") return "length";
	return "stop";
}

async function handleStream(req, piAi) {
	const { stream } = piAi.mod;
	if (typeof stream !== "function") {
		send({ id: req.id, type: "error", message: "pi-ai module has no stream() export" });
		return;
	}

	const model = req.model;
	// pi-ai requires Model.input (modality array) and Model.cost; normalize defaults.
	if (!Array.isArray(model.input) || model.input.length === 0) {
		model.input = ["text"];
	}
	if (!model.cost) {
		model.cost = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, tiers: [] };
	}
	const context = thunderToPiContext(req.messages, model);
	const tools = thunderToPiTools(req.tools);

	const abort = new AbortController();
	activeStreams.set(req.id, abort);

	const options = { signal: abort.signal };
	// pi-ai reads the key from options (ProviderRequestOptions.apiKey); the
	// compat dispatcher only injects ENV keys when this is absent.
	if (typeof model.apiKey === "string" && model.apiKey.length > 0) {
		options.apiKey = model.apiKey;
	}
	if (req.thinkingLevel) options.reasoning = req.thinkingLevel;
	if (typeof req.temperature === "number") options.temperature = req.temperature;
	if (typeof req.maxTokens === "number") options.maxTokens = req.maxTokens;
	// Prompt-cache write hint: "none" disables cache writes for one-off
	// requests (checkpoint summarization) that will never be reused.
	if (typeof req.cacheRetention === "string") options.cacheRetention = req.cacheRetention;
	if (tools.length > 0) context.tools = tools;

	try {
		for await (const ev of stream(model, context, options)) {
			switch (ev.type) {
				case "text_delta":
					send({ id: req.id, type: "text_delta", delta: ev.delta });
					break;
				case "thinking_delta":
					send({ id: req.id, type: "reasoning_delta", delta: ev.delta });
					break;
				case "done": {
					const msg = ev.message ?? {};
					const blocks = Array.isArray(msg.content) ? msg.content : [];
					const text = blocks
						.filter((b) => b.type === "text")
						.map((b) => b.text)
						.join("");
					const toolCalls = blocks
						.filter((b) => b.type === "toolCall")
						.map((b) => ({ id: b.id, name: b.name, arguments: b.arguments ?? {} }));
					const usage = msg.usage ?? {};
					send({
						id: req.id,
						type: "done",
						content: text.length > 0 ? text : null,
						toolCalls,
						finishReason: finishReasonFromPi(ev.reason),
						usage: {
							input: usage.input ?? 0,
							output: usage.output ?? 0,
							cacheRead: usage.cacheRead ?? 0,
							cacheWrite: usage.cacheWrite ?? 0,
							reasoning: typeof usage.reasoning === "number" ? usage.reasoning : undefined,
						},
					});
					break;
				}
				case "error": {
					const message = ev.error?.errorMessage ?? `stream failed (${ev.reason ?? "error"})`;
					send({ id: req.id, type: "error", message });
					break;
				}
				default:
					// start/end markers carry no thunder-relevant payload
					break;
			}
			if (ev.type === "done" || ev.type === "error") break;
		}
	} catch (err) {
		send({ id: req.id, type: "error", message: err?.message ?? String(err) });
	} finally {
		activeStreams.delete(req.id);
	}
}

async function handleListModels(req, piAi) {
	try {
		// getBuiltinModels lives on the providers/all subpath; optional import.
		let models = [];
		try {
			const all = await import(`${PI_PKG}/providers/all`);
			if (typeof all.getBuiltinModels === "function") {
				models = all.getBuiltinModels();
			}
		} catch {
			// Older/custom pi-ai builds may not ship it; report an empty catalog.
		}
		const entries = (models ?? []).map((m) => ({
			provider: m.provider,
			id: m.id,
			name: m.name ?? m.id,
			api: m.api,
			baseUrl: m.baseUrl ?? "",
			reasoning: m.reasoning ?? false,
			contextWindow: m.contextWindow ?? 0,
			maxTokens: m.maxTokens ?? 0,
			thinkingLevelMap: m.thinkingLevelMap ?? null,
		}));
		send({ id: req.id, type: "models", models: entries });
	} catch (err) {
		send({ id: req.id, type: "error", message: err?.message ?? String(err) });
	}
}

// ---------------------------------------------------------------------------
// main loop
// ---------------------------------------------------------------------------

const activeStreams = new Map(); // id → AbortController

const major = Number.parseInt(process.versions.node.split(".")[0], 10);
if (!Number.isFinite(major) || major < MIN_NODE_MAJOR) {
	fatal(`Node >= ${MIN_NODE_MAJOR} required (found ${process.versions.node}); pi-ai needs modern ESM + fetch.`);
	process.exit(1);
}

let piAi;
try {
	piAi = await loadPiAi();
} catch (err) {
	fatal(`failed to load pi-ai: ${err?.message ?? err}`);
	process.exit(1);
}

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

rl.on("line", (line) => {
	const trimmed = line.trim();
	if (!trimmed) return;
	let req;
	try {
		req = JSON.parse(trimmed);
	} catch {
		return; // ignore malformed lines
	}

	switch (req.cmd) {
		case "health":
			send({ id: req.id ?? "health-0", type: "ready", version: piAi.version, node: process.versions.node });
			break;
		case "stream":
			handleStream(req, piAi).catch((err) => {
				send({ id: req.id, type: "error", message: err?.message ?? String(err) });
			});
			break;
		case "cancel": {
			const abort = activeStreams.get(req.id);
			if (abort) abort.abort();
			break;
		}
		case "list_models":
			handleListModels(req, piAi).catch((err) => {
				send({ id: req.id, type: "error", message: err?.message ?? String(err) });
			});
			break;
		case "shutdown":
			process.exit(0);
			break;
		default:
			break;
	}
});

rl.on("close", () => process.exit(0));
