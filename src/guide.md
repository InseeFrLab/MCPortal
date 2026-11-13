# Using the MCP gateway

This gateway aggregates several MCP servers behind a single connection. This guide
describes how it works; the mechanics are the same whatever the deployment.

## Tool naming

Each tool of an upstream server is exposed under the name `{server}_{tool}`: the server
name is used as a prefix. Always use this full name to call a tool, never the upstream
name alone.

## Finding then calling a tool

When tool search is enabled, the tool list (`tools/list`) does not show the upstream
servers' tools, only three gateway tools:

1. `search_tools(query)`: full-text search (keywords, preferably in the language of the
   tool descriptions) across the names, descriptions and parameters of every tool. Each
   result gives the name to call and its full `inputSchema`.
2. `call_tool(name, arguments)`: calls the tool `name` (full name `{server}_{tool}`).
   `arguments` is a **JSON object** matching the `inputSchema` returned by the search,
   not a string.
3. `send_feedback(server, author, feedback)`: reports a problem or a remark about a
   server. The message is recorded in the gateway log.

Typical workflow: `search_tools` → pick a result → `call_tool` with its arguments.

When tool search is disabled, the upstream tools appear directly in `tools/list`, under
their full name, and are called normally.

## The catalog (resources)

The catalog is exposed as resources: reading it costs nothing until it is requested.

| Resource | Content |
| --- | --- |
| `catalog://servers` | all servers: domain, description, tags, status, limits |
| `catalog://domains` | servers grouped by functional domain |
| `catalog://servers/{name}` | details of a server and its metrics (tools, probes) |
| `catalog://servers/{name}/tools` | inventory: upstream name, name to call, description, parameters |
| `catalog://guide` | this guide, followed by the deployment-specific workflows |

An unreachable server does not break the catalog: its resource responds with
`"status": "unavailable"` and the error.

## Common pitfalls

- **Do not guess a tool name**: search for it with `search_tools` or read the inventory.
  An unknown name returns an error that suggests close names.
- **Invalid arguments**: the gateway validates the arguments against the tool's schema
  before calling it and returns the detailed errors. Fix them and try again.
- **Error result** (`isError: true`): the call went through but the tool failed; the
  message explains why. This is not a gateway outage.
- **Gateway or server saturated**: the number of concurrent calls is limited. Try again a
  few seconds later rather than multiplying parallel calls.
- **Server unavailable**: check `catalog://servers` for its status; use another server or
  try again later.
- **Empty search**: rephrase with other keywords or synonyms; the search covers the text
  of the tools, not the data they return.
