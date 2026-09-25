// Fake @earendil-works/pi-ai for protocol tests: no network, deterministic.
// Scenarios are selected by model.id:
//   "echo-model"  — asserts the mapped context, emits text/thinking deltas +
//                   a done event carrying a toolCall and full usage
//   "error-model" — emits an error event ("boom")
//   "slow-model"  — parks until aborted, then emits an aborted error event

const zeroUsage = () => ({
	input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
});

function assertMappedContext(context, options) {
	// These assertions verify the thunder→pi mapping done by bridge.mjs.
	if (context.systemPrompt !== "You are the test system prompt") {
		return `fake assertion failed: systemPrompt=${JSON.stringify(context.systemPrompt)}`;
	}
	const userMsg = context.messages.find((m) => m.role === "user");
	if (!userMsg || userMsg.content !== "hello bridge") {
		return `fake assertion failed: user message=${JSON.stringify(context.messages)}`;
	}
	const toolResult = context.messages.find((m) => m.role === "toolResult");
	if (!toolResult || toolResult.toolCallId !== "call_9" || toolResult.toolName !== "bash") {
		return `fake assertion failed: toolResult=${JSON.stringify(toolResult)}`;
	}
	const assistant = context.messages.find((m) => m.role === "assistant" && Array.isArray(m.content));
	const priorCall = assistant?.content.find((b) => b.type === "toolCall");
	if (!priorCall || priorCall.id !== "call_9" || priorCall.arguments.command !== "ls") {
		return `fake assertion failed: prior assistant toolCall=${JSON.stringify(priorCall)}`;
	}
	if (!Array.isArray(context.tools) || context.tools[0]?.name !== "bash_tool") {
		return `fake assertion failed: tools=${JSON.stringify(context.tools)}`;
	}
	if (options?.reasoning !== "high") {
		return `fake assertion failed: reasoning=${JSON.stringify(options?.reasoning)}`;
	}
	return null;
}

export async function* stream(model, context, options = {}) {
	if (model.id === "error-model") {
		yield { type: "error", reason: "error", error: { errorMessage: "boom" } };
		return;
	}

	if (model.id === "slow-model") {
		yield { type: "start", partial: {} };
		await new Promise((resolve) => {
			const timer = setTimeout(resolve, 30_000);
			options.signal?.addEventListener("abort", () => {
				clearTimeout(timer);
				resolve();
			}, { once: true });
		});
		if (options.signal?.aborted) {
			yield { type: "error", reason: "aborted", error: { errorMessage: "aborted by user" } };
			return;
		}
		yield {
			type: "done", reason: "stop",
			message: { role: "assistant", content: [{ type: "text", text: "late" }], usage: zeroUsage(), stopReason: "stop", timestamp: Date.now() },
		};
		return;
	}

	// echo-model (default)
	const failure = assertMappedContext(context, options);
	if (failure) {
		yield { type: "error", reason: "error", error: { errorMessage: failure } };
		return;
	}

	yield { type: "text_delta", contentIndex: 0, delta: "Hello ", partial: {} };
	yield { type: "text_delta", contentIndex: 0, delta: "world", partial: {} };
	yield { type: "thinking_delta", contentIndex: 1, delta: "pondering", partial: {} };
	yield {
		type: "done",
		reason: "toolUse",
		message: {
			role: "assistant",
			content: [
				{ type: "text", text: "Hello world" },
				{ type: "toolCall", id: "call_1", name: "bash", arguments: { command: "ls" } },
			],
			api: model.api,
			provider: model.provider,
			model: model.id,
			usage: { input: 11, output: 22, cacheRead: 4, cacheWrite: 1, reasoning: 6, totalTokens: 33, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
			stopReason: "toolUse",
			timestamp: Date.now(),
		},
	};
}
