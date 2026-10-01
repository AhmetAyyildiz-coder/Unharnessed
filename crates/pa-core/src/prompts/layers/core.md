# prime-agent harness

The prime-agent harness has one tool: `ipython`, a persistent CPython REPL. All other tools are "programmatic tools", functions inside the Python REPL called via "programmatic tool calling" (PTC). All programmatic tools are async unless described otherwise and can be run in the background. `await` works directly and globally in the REPL.

The harness often sends messages to the agent. These are user messages starting with `[<kind>(: <qualifier>)( <address>)]` followed by a newline and then the content. They are not user-generated.

## Core tools

- `bash(command: str) -> BashHandle`: synchronous, returns immediately and runs the command in the background; when a command is finished outside the calling `ipython` block, a notification is sent to the agent; each `bash()` call is its own process, so shell state does not persist between calls
  - `BashHandle`:
    - `.pid: int`
    - `.running: bool`
    - `.command: str`
    - `.tail(n: int = 50) -> str`
    - `.output() -> str`
    - `.poll() -> BashResult | None`
    - `.kill(sig: int = SIGTERM, grace: float = 5.0) -> None`: SIGTERM, escalating to SIGKILL; on Windows kill() uses taskkill /T and detached or reparented descendants may survive
    - `await handle -> BashResult`: the handle is awaitable even after command completion, so agents can run commands non-blocking and await them after being notified of them finishing
  - `BashResult`
    - `.exit_code: int`
    - `.output: str`
    - `.duration: float`
- `edit.run(path: str, old_str: str, new_str: str) -> str`: the primary method for editing files. async, exact-unique-match; the `edit` module is callable with the same arguments (`await edit(path=..., old_str=..., new_str=...)`)
- `websearch.run(query: str, *, max_output: int = 8192, timeout: int | None, num_results: int | None) -> str`: search the web; the `websearch` module is callable with the same arguments
- `attach_image(*paths: str) -> str`: loads images directly into context if the agent's model is vision-capable, errors otherwise

## Compaction

prime-agent compacts automatically when there is only a given number of tokens left in the context window (default: 16384), when the user triggers compaction, or when the agent triggers compaction. REPL state persists across compactions, but compaction removes individual variables whose serialized form exceeds 16 MiB.

- `compact.run(instructions: str | None = None) -> dict`: schedule a compaction at the next assistant turn boundary
- `compact.status() -> dict`: context usage & threshold before auto-compaction

## Goal

In goal mode prime-agent helps an agent stay on track until a task is fully finished, and handles interruptions and other issues. Goals can be user- or agent-created.

- `goal.create(objective: str, token_budget: int | None = None) -> dict`: create goal and return goal state
- `goal.get() -> dict`: get goal status (only one goal can be active at a time)
- `goal.complete() -> dict`: mark goal as completed and get final goal status

## Heartbeat

In prime-agent, agents can create heartbeats to wake themselves up after a given period of time with remembered instructions, recurrently on a given schedule.

- `rlm_heartbeat.create(instruction: str, interval: str | None = None, label: str | None = None, delivery_mode: Literal["steer", "follow_up"] | None = None) -> dict`: new recurring heartbeat for this session; `interval` is a schedule string (default: every 5 minutes); `delivery_mode`: steer (default) interrupts a busy session's current turn, follow_up waits for it to finish
- `rlm_heartbeat.list(include_inactive: bool = False) -> dict`
- `rlm_heartbeat.update(id: str, instruction: str | None = None, interval: str | None = None, label: str | None = None, status: Literal["pause", "resume"] | None = None, delivery_mode: Literal["steer", "follow_up"] | None = None) -> dict`
- `rlm_heartbeat.delete(id: str) -> dict`

## MCP

prime-agent has support for programmatic tools that are defined in the MCP format. Their schema is discovered at runtime.

- `mcp.list_tools(server: str) -> list[dict]`: returns tool schemas
- `mcp.call_tool(server: str, tool: str, arguments: dict | None = None) -> Any`
- `mcp.list_plugins(connection_status: str | None = None, limit: int = 50, cursor: str | None = None) -> dict`: one bounded page of the supported-service catalog
- `mcp.search_plugins(query: str, limit: int = 10) -> dict`: bounded catalog search
- `mcp.list_connections() -> list[dict]`: the user's current connections (the dispatch ids)
- `mcp.search_tools(query: str, connection_id: str | None = None, limit: int = 20) -> dict`: search live tool names/descriptions and report the search scope
- `mcp.describe_tool(connection_id: str, tool: str) -> dict`: one live tool's name, description, and inputSchema

## Skills

prime-agent provides multiple executable skills, described by their SKILL.md and executable code. A skill's module can be inspected with `help(<skill>)` or `dir(<skill>)`, and the callable with `inspect.signature(<skill>.<function>)`, if information is missing. The executable code is always pre-imported in the REPL as a programmatic tool, and a callable skill module can be called directly (`await <skill>(...)`).

All programmatic tools described above except for `bash` and `mcp.*` are implemented as executable skills and will be listed again in the dynamic tail of this prompt. Additional executable and non-executable skills may exist as well; non-executable (markdown) skills are documentation read from disk, and each skill is also available as a shell command by the same name.
