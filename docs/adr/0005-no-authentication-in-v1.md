# 0005. No authentication and no per-user limit in V1

- Status: Accepted
- Date: 2026-11-19
- Note: Recorded retroactively: moved from the *Design decisions* section of `ARCHITECTURE.md`.

## Context

A per-user limit requires knowing the user, and MCP clients do not identify them:
`clientInfo` describes the software (and is self-declared), and `Mcp-Session-Id` no longer
exists. The only mechanism provided by the spec is `Authorization: Bearer` (OAuth 2.1), with
the gateway acting as the *resource server*.

## Decision

V1 has no authentication and no per-user limit. The `/mcp` endpoint is public, so free
text is capped (`search_tools.query` 500 characters, `send_feedback.author` 200,
`send_feedback.feedback` 4000).

## Alternatives considered

- **Per-IP-address limit**: does not work with Claude.ai, whose connectors call from
  Anthropic's servers, so all their users would share a few addresses.
- Options for later, from simplest to most complete:

  | Mode | Identity | Note |
  | --- | --- | --- |
  | `api_key` | personal key in `Authorization: Bearer` | simple, no identity provider; keys in a Kubernetes Secret |
  | `oauth` | token from an identity provider (Keycloak, possibly federated with GitHub or Google) | the cleanest; the user signs in through their browser. Limitation: an account on that provider is required |

## Consequences

- Simple deployment, but no per-user accounting or abuse protection beyond the per-server
  limits (ADR-0004).
- When authentication is added: **never forward a user's token to the upstreams**
  (forbidden by the spec, "confused deputy" risk); if an upstream requires authentication,
  the gateway uses its own credentials. No user identifier as a metric label.
