# HTTP Permissions in IronClaw

IronClaw enforces HTTP access control at multiple independent layers. Each layer
defends a different boundary: the web gateway API, containers running agent jobs,
WASM tool sandboxes, and the built-in HTTP tool available to LLMs. This document
describes how each layer works, how they interact, and how to configure them.

---

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Web Gateway Authentication](#web-gateway-authentication)
3. [Sandbox Network Proxy](#sandbox-network-proxy)
4. [WASM Tool HTTP Allowlisting](#wasm-tool-http-allowlisting)
5. [Built-in HTTP Tool](#built-in-http-tool)
6. [Credential Injection Pipeline](#credential-injection-pipeline)
7. [Capabilities Schema Reference](#capabilities-schema-reference)
8. [Configuration Reference](#configuration-reference)
9. [Request Flow Diagrams](#request-flow-diagrams)
10. [Security Properties](#security-properties)

---

## Architecture Overview

There are four distinct enforcement points, each layered on top of the previous:

```
┌─────────────────────────────────────────────────────────────────┐
│  External Client (browser, webhook, Telegram, ...)              │
└──────────────────────────────┬──────────────────────────────────┘
                               │  HTTPS
                               ▼
┌─────────────────────────────────────────────────────────────────┐
│  Layer 1 — Web Gateway Auth (src/channels/web/auth.rs)          │
│  Bearer token, constant-time comparison                         │
└──────────────────────────────┬──────────────────────────────────┘
                               │  Internal
                               ▼
┌─────────────────────────────────────────────────────────────────┐
│  Layer 2 — Docker Sandbox + Network Proxy (src/sandbox/)        │
│  Domain allowlist, sandbox policy, credential injection         │
└──────────────────────────────┬──────────────────────────────────┘
                               │  Per-tool capabilities
                               ▼
┌─────────────────────────────────────────────────────────────────┐
│  Layer 3 — WASM HTTP Allowlisting (src/tools/wasm/allowlist.rs) │
│  Per-endpoint allowlist, path/method validation                 │
└──────────────────────────────┬──────────────────────────────────┘
                               │  LLM-invoked
                               ▼
┌─────────────────────────────────────────────────────────────────┐
│  Layer 4 — Built-in HTTP Tool (src/tools/builtin/http.rs)       │
│  HTTPS-only, IP blocklist, DNS rebinding defense                │
└─────────────────────────────────────────────────────────────────┘
```

Layers are independent. A request blocked at Layer 2 never reaches Layer 3 or 4.

---

## Web Gateway Authentication

**Source:** `src/channels/web/auth.rs`

The web gateway (browser UI and REST API) requires a bearer token on every
request.

### How it works

Requests must include an `Authorization` header:

```
Authorization: Bearer <token>
```

Server-Sent Events clients (browser `EventSource`) cannot set custom headers, so
a query-parameter fallback is supported:

```
GET /api/events?token=<token>
```

Token comparison uses `subtle::ConstantTimeEq` to prevent timing side-channel
attacks. All routes share the same token; there is no role-based access control.

### Configuration

| Variable | Required | Description |
|----------|----------|-------------|
| `GATEWAY_AUTH_TOKEN` | Yes | Bearer token for all API access |
| `GATEWAY_HOST` | No | Bind address (default: `127.0.0.1`) |
| `GATEWAY_PORT` | No | Listen port (default: `3001`) |
| `GATEWAY_ENABLED` | No | Enable/disable the gateway (default: `true`) |

---

## Sandbox Network Proxy

**Source:** `src/sandbox/proxy/`

Containers running agent jobs route all outbound HTTP/HTTPS through a host-side
proxy. The proxy enforces a domain allowlist and injects credentials at transit
time so containers never handle secrets directly.

### Sandbox Policies

The `SANDBOX_DEFAULT_POLICY` (or per-job `SandboxPolicy`) controls both
filesystem access and network access:

| Policy | Filesystem | Network |
|--------|-----------|---------|
| `ReadOnly` | `/workspace` read-only | Allowlisted domains only |
| `WorkspaceWrite` | `/workspace` read-write | Allowlisted domains only |
| `FullAccess` | Full host filesystem | Unrestricted (allowlist bypassed) |

`FullAccess` should only be used for fully trusted administrative jobs.

### Domain Allowlist

**Source:** `src/sandbox/proxy/allowlist.rs`

The allowlist is a list of hostname patterns. Both exact and wildcard patterns
are supported:

- `api.openai.com` — exact match
- `*.github.com` — matches `api.github.com`, `raw.githubusercontent.com`, etc.
  The base domain (`github.com`) is also matched.

**Default allowlist includes:**
- Package registries: `crates.io`, `registry.npmjs.org`, `pypi.org`, `registry.yarnpkg.com`
- Docs: `docs.rs`, `nodejs.org`, Python docs
- Version control: `github.com`, `raw.githubusercontent.com`, `api.github.com`
- Common APIs: `api.openai.com`, `api.anthropic.com`, `api.near.ai`

To add extra domains without rebuilding:

```bash
SANDBOX_EXTRA_DOMAINS=api.example.com,*.mycompany.com
```

### Network Policy Decisions

**Source:** `src/sandbox/proxy/policy.rs`

The `NetworkPolicyDecider` trait produces one of three outcomes for each request:

| Decision | Meaning |
|----------|---------|
| `Allow` | Forward without modification |
| `AllowWithCredentials { secret_name, location }` | Resolve secret and inject before forwarding |
| `Deny { reason }` | Drop request, return error to container |

The default decider (`DefaultPolicyDecider`) first checks the domain allowlist,
then looks up credential mappings by host pattern.

### HTTP vs HTTPS Tunneling

For plain HTTP requests the proxy can read and modify headers, enabling
credential injection. For HTTPS, the container sends a `CONNECT` request to
establish an encrypted tunnel; the proxy validates the target domain but cannot
read or modify the encrypted payload. Containers that need authenticated HTTPS
must fetch credentials explicitly via the orchestrator API:

```
GET /worker/{job_id}/credentials/{secret_name}
```

### Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `SANDBOX_ENABLED` | `true` | Enable Docker sandbox |
| `SANDBOX_DEFAULT_POLICY` | `workspace_write` | Default policy for new jobs |
| `SANDBOX_EXTRA_DOMAINS` | (empty) | Comma-separated extra allowlisted domains |
| `SANDBOX_NETWORK_PROXY` | `true` | Enable network proxy |
| `SANDBOX_PROXY_PORT` | `8080` | Proxy listen port on host |
| `SANDBOX_TIMEOUT_SECS` | `1800` | Container lifetime |
| `SANDBOX_MEMORY_LIMIT_MB` | `512` | Container memory cap |
| `SANDBOX_CPU_LIMIT` | `1.0` | CPU cores per container |
| `SANDBOX_IMAGE` | `ironclaw-worker:latest` | Docker image |

---

## WASM Tool HTTP Allowlisting

**Source:** `src/tools/wasm/allowlist.rs`, `src/tools/wasm/capabilities_schema.rs`

Each WASM tool declares the HTTP endpoints it is permitted to reach in a
`capabilities.json` sidecar file stored alongside the `.wasm` binary. The host
runtime validates every HTTP request from the tool against this declaration
before executing it.

### Allowlist Pattern Format

```json
{
  "http": {
    "allowlist": [
      {
        "host": "api.slack.com",
        "port": 443,
        "path_prefix": "/api/",
        "methods": ["GET", "POST"]
      },
      {
        "host": "*.example.com"
      }
    ]
  }
}
```

| Field | Required | Description |
|-------|----------|-------------|
| `host` | Yes | Exact hostname or `*.domain` wildcard |
| `port` | No | If omitted, any port is accepted |
| `path_prefix` | No | Request path must start with this value |
| `methods` | No | Allowed HTTP methods; if omitted, all are allowed |

### URL Validation Rules

Before checking the pattern list, the runtime validates the URL itself:

- **HTTPS required** — plain HTTP is rejected.
- **No userinfo** — `user:pass@host` syntax is rejected to prevent host-confusion
  bypasses.
- **Path normalization** — paths are normalized and checked for traversal
  sequences (`/../`, `%2e%2e`, encoded path separators like `%2F`).
- **Percent encoding** — all `%XX` sequences must be valid hex.

A request that fails any of these checks is rejected regardless of the allowlist.

### Wildcard Matching Semantics

| Pattern | Matches | Does not match |
|---------|---------|----------------|
| `api.example.com` | `api.example.com` | `www.example.com` |
| `*.example.com` | `api.example.com`, `a.b.example.com` | `example.com` |

---

## Built-in HTTP Tool

**Source:** `src/tools/builtin/http.rs`

The LLM can call the `http_request` tool directly. This tool runs on the host
(not inside a WASM sandbox), so it applies its own validation layer.

### URL Validation

- HTTPS only.
- `localhost` and `*.localhost` are blocked.
- Private IP ranges are blocked:
  - IPv4: `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `127.0.0.0/8`
  - IPv4 link-local: `169.254.0.0/16` (includes AWS metadata endpoint `169.254.169.254`)
  - IPv6: loopback (`::1`), unique local (`fc00::/7`), link-local (`fe80::/10`)
- **DNS rebinding defense** — the hostname is resolved to IP addresses; all
  resolved addresses are checked against the IP blocklist.

### Approval Requirements

Some requests require explicit user approval before execution:

- `ApprovalRequirement::Always`:
  - Request includes authentication headers (`Authorization`, `X-API-Key`,
    `Cookie`, `X-Token`, etc.)
  - URL contains credential query parameters (`api_key=`, `access_token=`, etc.)
  - Target host has a credential mapping in the shared registry
- `ApprovalRequirement::UnlessAutoApproved` — otherwise

### Response Limits

| Limit | Value |
|-------|-------|
| `MAX_RESPONSE_SIZE` | 5 MB |
| Content-Length pre-check | Yes (rejects oversized before download) |
| Streaming hard cap | Yes |

### Leak Detection

Both outbound requests and inbound responses are scanned by `LeakDetector`:

- Outbound: `LeakDetector::scan_http_request()` — prevents secrets from being
  sent to unexpected destinations.
- Inbound: `LeakDetector::scan()` — prevents secrets from reaching the LLM in
  raw form.

---

## Credential Injection Pipeline

**Source:** `src/tools/wasm/credential_injector.rs`, `src/sandbox/proxy/http.rs`

The credential injection system ensures WASM tools and sandboxed containers can
authenticate against external APIs without ever handling secret values in
plaintext.

### How it works

```
Tool/container makes HTTP request
         │
         ▼
  Host resolves target host
         │
         ▼
  SharedCredentialRegistry.find_for_host(host)
         │
   ┌─────┴──────┐
   │  No match  │  Allow (unauthenticated)
   └─────┬──────┘
         │  Match found
         ▼
  Check secret against tool's allowed_names
         │
   ┌─────┴──────┐
   │  Rejected  │  Return error
   └─────┬──────┘
         │  Permitted
         ▼
  Decrypt secret (host only, never passed to tool)
         │
         ▼
  Inject into request at specified location
         │
         ▼
  Execute request → return response
```

### Injection Locations

| Location type | Result |
|---------------|--------|
| `bearer` | `Authorization: Bearer {secret}` |
| `basic { username }` | `Authorization: Basic base64("{username}:{secret}")` |
| `header { name, prefix }` | `{name}: {prefix}{secret}` |
| `query_param { name }` | `?{name}={secret}` appended to URL |
| `url_path { placeholder }` | Placeholder in path replaced with secret |

### Declaring credentials in capabilities.json

```json
{
  "http": {
    "credentials": {
      "slack": {
        "secret_name": "slack_bot_token",
        "location": { "type": "bearer" },
        "host_patterns": ["slack.com", "*.slack.com"]
      }
    }
  },
  "secrets": {
    "allowed_names": ["slack_bot_token"]
  }
}
```

`secrets.allowed_names` is a glob list. The tool can only access secrets whose
names match at least one pattern in this list. An entry in
`http.credentials` that references a secret not in `allowed_names` will be
rejected at runtime.

---

## Capabilities Schema Reference

**Source:** `src/tools/wasm/capabilities_schema.rs`

A full `capabilities.json` with all supported fields:

```json
{
  "http": {
    "allowlist": [
      {
        "host": "api.example.com",
        "port": 443,
        "path_prefix": "/v1/",
        "methods": ["GET", "POST", "DELETE"]
      }
    ],
    "credentials": {
      "my_service": {
        "secret_name": "example_api_key",
        "location": { "type": "header", "name": "X-API-Key", "prefix": "" },
        "host_patterns": ["api.example.com"]
      }
    },
    "rate_limit": {
      "requests_per_minute": 50,
      "requests_per_hour": 1000
    },
    "max_request_bytes": 32768,
    "max_response_bytes": 5242880,
    "timeout_secs": 30
  },
  "secrets": {
    "allowed_names": ["example_api_key", "example_*"]
  },
  "workspace": {
    "allowed_prefixes": ["context/", "daily/"]
  },
  "websocket": {
    "allowlist": [
      { "host": "localhost", "port": 9222 }
    ],
    "pooling_enabled": true
  },
  "auth": {
    "secret_name": "example_api_key",
    "display_name": "Example Service",
    "instructions": "Obtain an API key from https://example.com/settings",
    "oauth": {
      "authorization_url": "https://example.com/oauth/authorize",
      "token_url": "https://example.com/oauth/token",
      "scopes": ["read", "write"]
    }
  },
  "approval_policy": {
    "actions_requiring_approval": ["send_message", "delete_resource"]
  }
}
```

---

## Configuration Reference

### Web Gateway

| Variable | Default | Description |
|----------|---------|-------------|
| `GATEWAY_ENABLED` | `true` | Enable the web gateway |
| `GATEWAY_AUTH_TOKEN` | (required) | Bearer token for all API access |
| `GATEWAY_HOST` | `127.0.0.1` | Bind address |
| `GATEWAY_PORT` | `3001` | Listen port |
| `GATEWAY_USER_ID` | `default` | User ID injected into gateway requests |

### Sandbox & Network Proxy

| Variable | Default | Description |
|----------|---------|-------------|
| `SANDBOX_ENABLED` | `true` | Enable Docker sandbox |
| `SANDBOX_DEFAULT_POLICY` | `workspace_write` | `readonly`, `workspace_write`, or `full_access` |
| `SANDBOX_NETWORK_PROXY` | `true` | Enable network proxy for containers |
| `SANDBOX_PROXY_PORT` | `8080` | Proxy port on host |
| `SANDBOX_EXTRA_DOMAINS` | (empty) | Comma-separated extra allowlisted domains |
| `SANDBOX_TIMEOUT_SECS` | `1800` | Container lifetime in seconds |
| `SANDBOX_MEMORY_LIMIT_MB` | `512` | Memory cap per container |
| `SANDBOX_CPU_LIMIT` | `1.0` | CPU cores per container |
| `SANDBOX_IMAGE` | `ironclaw-worker:latest` | Docker image for worker containers |

### Secrets

| Variable | Default | Description |
|----------|---------|-------------|
| `SECRETS_MASTER_KEY` | (from keychain) | AES-256-GCM master key for secret encryption |

---

## Request Flow Diagrams

### Container outbound HTTP (Layers 2)

```
Container process
    │  http_proxy=host.docker.internal:8080
    ▼
Sandbox proxy receives request
    │
    ├─ SandboxPolicy::FullAccess?
    │     └─ YES → Allow immediately (no allowlist check)
    │
    └─ NO → DefaultPolicyDecider:
          │
          ├─ Domain in allowlist?
          │     └─ NO → DENY
          │
          └─ YES → Credential mapping for host?
                │
                ├─ YES → Resolve secret from EnvCredentialResolver
                │         Inject at configured location
                │         Forward to destination
                │
                └─ NO  → Forward without modification
```

### WASM tool HTTP request (Layer 3)

```
WASM module calls http_request(url, method, ...)
    │
    ▼
AllowlistValidator.validate(url, method)
    │
    ├─ Invalid URL, non-HTTPS, userinfo, path traversal → DENY
    │
    ├─ Host not in capabilities.json allowlist → DENY
    │
    ├─ Method not in allowed methods → DENY
    │
    └─ ALLOW → find credential mapping for host
                    │
                    ├─ Secret not in allowed_names → ERROR
                    │
                    └─ Decrypt + inject → execute request
                                              │
                                              └─ Scan response for leaks → return
```

### LLM calls built-in http_request tool (Layer 4)

```
LLM emits http_request tool call
    │
    ▼
Validate URL
    ├─ Not HTTPS → DENY
    ├─ localhost / *.localhost → DENY
    ├─ Private IP (direct or via DNS) → DENY
    └─ PASS
    │
    ▼
Determine approval requirement
    ├─ Auth headers / credential params / mapped host → Require user approval
    └─ Otherwise → Allow (or auto-approve if configured)
    │
    ▼
Scan outbound request for leaked secrets
    │
    ▼
Execute (with credential injection if mapped)
    │
    ▼
Enforce response size limit (5 MB)
    │
    ▼
Scan response for leaked secrets → return to LLM
```

---

## Security Properties

| Property | Mechanism |
|----------|-----------|
| Secrets never leave the host | Decryption and injection happen in the host process; containers and WASM modules receive only the HTTP response |
| Timing-safe gateway auth | `subtle::ConstantTimeEq` prevents token enumeration via response timing |
| DNS rebinding defense | Built-in HTTP tool resolves hostnames and rejects private IPs post-resolution |
| Path traversal prevention | WASM allowlist validator normalizes paths and rejects `/../`, `%2e%2e`, encoded separators |
| Per-tool secret scoping | `secrets.allowed_names` in capabilities.json limits which secrets each tool may access |
| Defense in depth | Allowlists exist independently at both the sandbox proxy layer and the WASM layer |
| Leak detection | Both outbound requests and inbound responses are scanned for 15+ secret patterns before reaching the LLM |
| HTTPS-only external access | Both the built-in HTTP tool and WASM allowlist validation reject plain HTTP |
| Container network isolation | Containers have no direct internet access; all traffic flows through the allowlist-enforcing proxy |
