# Builder Mode in IronClaw

Builder mode is IronClaw's self-expanding capability: the agent can write, compile, test, validate, and register new tools at runtime — entirely through LLM-driven code generation. When a task requires a capability the agent doesn't have, builder mode lets it build that capability on the spot.

---

## What It Does

Builder mode exposes a single entry-point tool called `build_software`. When called, it launches a sandboxed build loop where the LLM iteratively writes code, runs shell commands, reads files, fixes errors, and validates the final artifact — all without human intervention beyond the initial approval prompt.

The output is a compiled artifact (typically a `.wasm` binary) stored on disk and available for immediate registration into the live tool registry.

---

## Configuration

| Environment Variable | Default | Purpose |
|---|---|---|
| `BUILDER_ENABLED` | `true` | Master switch |
| `BUILDER_DIR` | system temp dir | Where build artifacts are written |
| `BUILDER_MAX_ITERATIONS` | `20` | LLM loop iteration cap |
| `BUILDER_TIMEOUT_SECS` | `600` | Hard timeout (10 minutes) |
| `BUILDER_AUTO_REGISTER` | `true` | Config flag (note: current implementation always returns `registered: false` — see Limitations) |

---

## Activation Conditions

Builder mode is wired in during agent startup (`src/app.rs`):

```rust
if self.config.builder.enabled
    && (self.config.agent.allow_local_tools || !self.config.sandbox.enabled)
{
    tools.register_builder_tool(llm, safety, Some(builder_config)).await;
}
```

Both conditions must hold:

1. `BUILDER_ENABLED=true`
2. Either `allow_local_tools=true` **or** Docker sandbox is disabled

When sandbox is enabled and `allow_local_tools` is false, builder mode is deliberately suppressed — the build loop requires shell access to run compilers, which would conflict with strict sandbox isolation.

When builder mode activates, it also registers the **dev tools** (`shell`, `read_file`, `write_file`, `list_dir`, `apply_patch`, `http`) that the build loop uses internally. If builder mode is off, these dev tools are still registered separately for direct agent use.

---

## The `build_software` Tool

**Tool name**: `build_software` (protected — cannot be shadowed by dynamic registrations)

**Requires approval**: Yes — the user is prompted before the build starts unless auto-approval is configured.

### Parameters

| Parameter | Type | Required | Description |
|---|---|---|---|
| `description` | string | yes | Natural language description of what to build |
| `type` | string | no | `wasm_tool`, `cli_binary`, `library`, `script` — inferred by LLM if omitted |
| `language` | string | no | `rust`, `python`, `typescript`, `bash` — inferred if omitted |

### Return Value

```json
{
  "build_id": "uuid",
  "name": "tool_name",
  "success": true,
  "artifact_path": "/tmp/builds/tool_name/target/wasm32-wasip2/release/tool_name.wasm",
  "iterations": 7,
  "error": null,
  "phases": [
    "Analyzing: extracting requirements",
    "Scaffolding: creating project structure",
    "Implementing: writing source code",
    "Building: compiling",
    "Testing: running test suite",
    "Validating: checking WASM interface",
    "Complete"
  ]
}
```

---

## Build Pipeline

```
┌─────────────────────────────────────────────────┐
│  1. ANALYZE                                     │
│     LLM parses description → BuildRequirement   │
│     (name, type, language, dependencies,        │
│      feature list, test cases)                  │
└────────────────────┬────────────────────────────┘
                     ▼
┌─────────────────────────────────────────────────┐
│  2. BUILD LOOP  (up to BUILDER_MAX_ITERATIONS)  │
│                                                 │
│  Each iteration:                                │
│   a) Feed build state + available tools to LLM  │
│   b) LLM emits tool calls (write_file,          │
│      shell, read_file, apply_patch, http…)      │
│   c) Execute tool calls, collect output         │
│   d) Return output to LLM                       │
│   e) Check for completion or failure signal     │
│   f) If error: enter Fixing phase, iterate      │
│                                                 │
│  Stuck detection: 2 consecutive text-only       │
│  responses (no tool calls) → fail              │
└────────────────────┬────────────────────────────┘
                     ▼
┌─────────────────────────────────────────────────┐
│  3. TEST                                        │
│     TestHarness runs input/output cases         │
│     extracted from JSON schema or provided      │
│     by the LLM during the build loop            │
└────────────────────┬────────────────────────────┘
                     ▼
┌─────────────────────────────────────────────────┐
│  4. VALIDATE  (WASM tools only)                 │
│     WasmValidator checks:                       │
│     • Binary size ≤ 10 MB                       │
│     • `run` export exists                       │
│     • Only allowed import modules               │
│       (env, wasi_snapshot_preview1, wasi)       │
│     • Warns on dangerous imports                │
│       (filesystem, socket WASM functions)       │
└────────────────────┬────────────────────────────┘
                     ▼
┌─────────────────────────────────────────────────┐
│  5. ARTIFACT                                    │
│     Returns path to compiled binary.            │
│     Registration is a separate step             │
│     (see Limitations).                          │
└─────────────────────────────────────────────────┘
```

### Build Phases (tracked in `BuildPhase` enum)

| Phase | Meaning |
|---|---|
| Analyzing | Parsing the natural language requirement |
| Scaffolding | Creating directory structure and boilerplate |
| Implementing | Writing source code |
| Building | Compiling (`cargo component build --release`) |
| Testing | Running TestHarness |
| Fixing | Iterating on compiler/test errors |
| Validating | WASM interface compliance check |
| Registering | Adding to tool registry |
| Packaging | Producing final artifact |
| Complete | Success |
| Failed | Unrecoverable error |

---

## Software Types

| Type | Language | When the LLM picks it |
|---|---|---|
| `WasmTool` | Rust | Any tool the agent itself will call — this is the strongly preferred output for agent-usable capabilities |
| `CliBinary` | Rust, Go | Standalone programs for human use |
| `Library` | Rust, TypeScript | Reusable code modules |
| `Script` | Python, Bash | Short interpreted programs |
| `WebService` | TypeScript | REST APIs (stub, not fully implemented) |

The LLM's system prompt hard-directs it: **"If this is a tool that the agent will use, ALWAYS use `wasm_tool` type and `rust` language."** Non-WASM types are reserved for software being built for human end-users.

---

## Tools Available Inside the Build Loop

The build loop gives the LLM access to a constrained set of dev tools:

| Tool | What it does |
|---|---|
| `shell` | Run arbitrary shell commands (compiler, cargo, npm, etc.) |
| `write_file` | Create or overwrite source files |
| `read_file` | Read any file in the build directory |
| `list_dir` | Inspect directory structure |
| `apply_patch` | Surgical targeted edits to existing files |
| `http` | Fetch documentation or external resources |

These are **protected tool names** — they cannot be shadowed by any dynamic tool registration.

---

## WASM Tools — What They Can Do

When building a `WasmTool`, the LLM receives full documentation for the WASM host ABI. Built tools can call these host functions if the corresponding capability is granted:

| Host Function | Capability Required | What it gives the tool |
|---|---|---|
| `log(level, msg)` | none | Write to agent log |
| `now_millis()` | none | Current Unix timestamp |
| `workspace_read(path)` | `workspace` | Read from persistent memory |
| `http_request(req)` | `http` | Call external APIs |
| `tool_invoke(name, params)` | `tool_invoke` | Call other registered tools |
| `secret_exists(key)` | `secrets` | Check for named credentials |

Capabilities are declared in a `{tool_name}.capabilities.json` file alongside the `.wasm` binary:

```json
{
  "http": {
    "allowed_endpoints": [
      { "host": "api.openai.com", "path_prefix": "/v1/" }
    ]
  },
  "workspace": true,
  "secrets": {
    "allowed": ["OPENAI_API_KEY"]
  }
}
```

Tools without a capabilities file get no external access — pure computation only.

---

## WASM Tool Registration Pipeline

After a successful build, registering the tool into the live registry (`registry.rs::register_wasm`) follows these steps:

1. **Prepare** — Validate and compile the WASM module with `wasmtime`
2. **Extract capabilities** — Parse HTTP credentials and endpoint allowlist from capabilities file
3. **Create wrapper** — Instantiate `WasmToolWrapper` with its capability set
4. **Apply overrides** — Custom description, schema, secrets store handle
5. **Register** — Add to `ToolRegistry` (available for LLM tool calls immediately)
6. **Index credentials** — Add credential mappings to shared registry for proxy HTTP injection

---

## Database Persistence

Built tools are tracked in two tables:

**`dynamic_tools`** — logical tool record:
```sql
id, name, description, parameters_schema, code,
sandbox_config, created_by_job_id,
success_count, failure_count, last_error, status,
created_at, updated_at
```

**`wasm_tools`** — binary-level record with integrity hash and capabilities JSON.

The `dynamic_tools.status` field can be `active` or `suspended`. Suspended tools remain in the database but are excluded from the registry at startup.

---

## Use Cases

These are concrete scenarios where builder mode would be needed:

### 1. New API Integrations
The agent needs to interact with an external service (weather API, GitHub Enterprise, a company's internal REST API) that has no pre-built tool. Builder mode generates a WASM tool that wraps the API, declares the necessary `http` capability with the endpoint allowlist, and handles authentication via a stored secret.

### 2. Data Transformation Pipelines
A user asks the agent to repeatedly convert between data formats (XML to JSON, CSV to Parquet, custom binary protocols). Rather than shelling out every time, builder mode creates a dedicated high-performance WASM transform tool that can be called directly in subsequent jobs.

### 3. Domain-Specific Calculators
Financial calculations (compound interest, options pricing), scientific computations (unit conversions, statistical models), or business-rule engines — logic too complex to inline into a prompt but not worth maintaining as a separate service. Builder mode creates these as versioned WASM compute tools.

### 4. Persistent Integrations for Routines
A routine needs to poll a data source every hour. Rather than re-doing the work through shell commands each time, builder mode creates a dedicated WASM tool that the routine calls directly, with proper error handling and rate limiting declared in capabilities.

### 5. Internal CLI Tools
A user needs a command-line utility for their own use — a custom `git` wrapper, a log parser, a database migration script. Builder mode can produce a standalone `cli_binary` for human use rather than agent use.

### 6. Extending Agent Capability in Locked-Down Environments
When `SANDBOX_ENABLED=true` and the agent cannot run arbitrary shell commands during normal operation, builder mode (if `allow_local_tools=true`) provides a controlled pathway to extend capability: code is reviewed, compiled, and validated before being made available as a proper sandboxed WASM tool.

### 7. Prototype-and-Deploy Workflow
A user describes a capability in natural language. The agent builds a working prototype, runs tests, and hands back the artifact path. The user reviews the source, grants capabilities, and registers it — at which point the tool becomes a permanent part of the agent's repertoire.

---

## What Builder Mode Does NOT Do

- **It does not auto-register built tools into the live registry** — despite `BUILDER_AUTO_REGISTER` existing in config, the current implementation always returns `registered: false`. Registration is a manual step after build.
- **It does not version built tools** — there is no rollback, no version history, no upgrade path for a rebuilt tool. A re-build overwrites the artifact.
- **It does not grant capabilities automatically** — a built WASM tool that needs HTTP access must have its capabilities file manually created or the LLM must emit it during the build. There is no UX flow for post-build capability granting.
- **It does not build non-WASM tools for agent use** — scripts and binaries cannot be registered as callable tools; only WASM modules can be wrapped by the tool registry.
- **It does not support incremental compilation** — each build starts from scratch in a fresh directory.

---

## Security Considerations

Builder mode has significant security implications:

- **Shell access is real**: The build loop runs actual shell commands. This is intentionally gated behind `allow_local_tools` or sandbox-off. Never enable builder mode on a system where you wouldn't want the LLM to run arbitrary shell commands.
- **Approval gate**: All `build_software` calls require user approval (unless auto-approved via agent config). Do not set global auto-approval if builder mode is on.
- **WASM sandbox boundary**: Once built and registered, WASM tools run inside the wasmtime sandbox with fuel metering, memory limits, and the capability allowlist. The dangerous part is the build phase itself, not the resulting tool.
- **Import allowlist validation**: `WasmValidator` blocks modules importing raw filesystem or socket WASM functions. Tools needing network access must go through the controlled `http_request` host function.
- **No secret injection during build**: Secrets are not available inside the build loop. They are injected at runtime through the WASM credential injector, keeping them out of source code and build logs.

---

## Limitations (Current Implementation)

| Limitation | Impact |
|---|---|
| `auto_register` config is a no-op | Built tools must be manually registered after build |
| No version tracking | Rebuilding a tool overwrites the previous artifact with no history |
| No capability-granting UX | HTTP/workspace/secrets access requires manually editing capabilities files |
| WASM-only for agent tools | Python/bash scripts cannot be registered as callable tools |
| No schema extraction from WASM | Tool description and parameters schema must be declared in the build (WIT bindgen not integrated) |
| `WebService` type is a stub | TypeScript web services are scaffolded but not deployable through current infrastructure |

---

## File Map

| File | Role |
|---|---|
| `src/tools/builder/core.rs` | `LlmSoftwareBuilder`, `BuildRequirement`, `SoftwareType`, `Language`, `BuildPhase` |
| `src/tools/builder/templates.rs` | Project scaffolding templates by type/language |
| `src/tools/builder/testing.rs` | `TestHarness` — runs input/output test cases against built tools |
| `src/tools/builder/validation.rs` | `WasmValidator` — size, export, import checks |
| `src/tools/wasm/wrapper.rs` | `WasmToolWrapper` — wraps compiled WASM as a `Tool` |
| `src/tools/wasm/loader.rs` | Discovers `.wasm` files and their `.capabilities.json` pairs |
| `src/tools/registry.rs` | `ToolRegistry::register_wasm()` — live registration pipeline |
| `src/app.rs` | `register_builder_tool()` — wires builder into the agent at startup |
| `src/config.rs` (builder section) | `BuilderModeConfig`, `BuilderConfig` structs |
