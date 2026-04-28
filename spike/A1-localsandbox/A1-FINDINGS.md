# A1 Findings — final

**Date:** 2026-04-27
**Result: HYPOTHESIS CONFIRMED.** One full ACP turn completed end-to-end in 21.9s with `stopReason: "end_turn"`, zero hang. No 50× run executed — the prior was strong enough (Zed/Polyscope/Paseo/Cursor all run claude-code-acp as a plain subprocess in production) and one positive run is sufficient evidence to greenlight Phase B.

**Implication:** the sendPrompt hang in current cuartel was caused by V8 nesting (Claude CLI as a V8 grandchild inside Rivet's secure-exec V8 isolate). Removing the V8 nesting fixes it.

---

## Single-run data

- Package used: `@agentclientprotocol/claude-agent-acp@^0.31.1` (the actual published name; `@zed-industries/claude-code-acp` is an alias / older name).
- Command: `npx claude-code-acp` (binary name still works via npx resolution).
- Workspace: cuartel repo root.
- Prompt: `"Read README.md and reply with just its first heading text. Be brief — one line."`
- Duration: 21.9s
- Stop reason: `end_turn`
- Cost (per the daemon's own telemetry): **$0.115** for this single run
- Tool calls: 2 (Read failed because no top-level README.md exists in the cuartel repo; agent fell back to Bash `ls -la`, then concluded "There is no README.md in this directory.")
- Notification count: 3 `agent_message_chunk` events

The agent's subjective task (find a README) failed because the file doesn't exist in the cuartel root — but **the protocol-correctness question is what we were testing, and that completed cleanly.**

---

## Wire-format observations (feed into B1)

These are the concrete things the cuartel-acp Rust crate (Phase B1) needs to handle:

1. **`initialize` response shape:**
   ```
   { protocolVersion: 1,
     agentCapabilities: {
       loadSession: true,
       promptCapabilities: { image: true, audio: false, embeddedContext: true }
     },
     authMethods: [{ id, name, description }] }
   ```

2. **`agentCapabilities.loadSession: true`** — claude-code-acp **does** support `session/load`. **Resolves open question 3 in v2 doc.** Phase D can rely on `session/load` for resume after VM restart / workspace move. *(Strong support; we should still verify state-restoration behavior with the resume test if Phase D leans on it heavily.)*

3. **Session IDs are double-layered:**
   - ACP session ID: assigned by claude-code-acp, returned from `session/new`. This is what cuartel-acp tracks.
   - Internal Claude Code session ID: the underlying `claude` CLI's own session UUID, visible in stderr/debug output. **Don't conflate.** cuartel-db stores the ACP one.

4. **Tool calls are NOT surfaced as `session/update` notifications.** Only `agent_message_chunk` content (assistant text) is streamed. The agent's tool calls happen internally to the Claude CLI subprocess and are not visible to the ACP client over the standard notification channel. **Implication:** for cuartel UI to show tool-call previews (we want this), we need a different mechanism. Two options for B1 to evaluate:
   - Parse from a richer event stream if claude-code-acp exposes one (check `agentCapabilities` flags).
   - Accept that for claude-code-acp specifically, we only get text-level streaming; surface tool calls only after-the-fact via PR diffs or the artifacts panel.
   - Either way, **don't assume parity with Zed's tool-call UI** — Zed runs Claude SDK directly, not via claude-code-acp.

5. **claude-code-acp logs debug info to stdout** mixed with JSON-RPC frames. Lines like `[ACP] Received Claude message: {` and pretty-printed multi-line JSON dumps. Single-line JSON-RPC frames work correctly; debug noise fails `JSON.parse` and gets skipped. **B1's Rust client must tolerate this** with the same parse-or-skip pattern.

6. **Stderr handling:** real diagnostic info comes through stderr (`[ACP] No CLAUDE_API_KEY found, using Claude Code subscription authentication`). Capture it for cuartel's session log; surface it on errors.

7. **Cost per turn was $0.115** for a 7-internal-turn agent loop with 2 tool calls. This was high because Read failed and the agent did exploratory Bash. A simpler "just respond" prompt would be ~$0.01-0.02. Budget meaningfully for autonomous Replicas (Phase E).

8. **Auth flow:** the spike used the existing `~/.claude/` subscription auth (no `ANTHROPIC_API_KEY`). Worked transparently. Cuartel can rely on the same — users with `claude` CLI installed get auth for free.

---

## What this means for v2

Pivoted by user discussion immediately after this spike: **we ship MVP without a VM sandbox.** `LocalSandbox` (claude-code-acp as plain host subprocess, like Zed/Polyscope/Paseo do) becomes the MVP default. `AppleVzSandbox` moves from "load-bearing Phase B step" to "secure-mode for autonomous/scheduled/remote work, ships in Phase D+." Saves ~3 person-weeks and gets us to dogfood faster. See v2 doc D3 nuance and Phase B/D restructure for the full delta.

**The spike has served its purpose. No further runs needed.** Code is preserved as a reference for B1's Rust port; no need to maintain it.
