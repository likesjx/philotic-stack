# ZeroClaw: The Architect's Thesis on the Context Graph & Philotic Web

*Compiled by Aria the Architect, 2026-03-05*

This document outlines the architectural synthesis of the ZeroClaw mesh, specifically focusing on the transition from a flat, node-bound execution environment to a deeply relational, dynamically materializing graph system.

## 1. The Core Paradigm Shift: The "Philotic Web"
ZeroClaw represents a shift from static APIs to a connectionless, ambient mesh (UDP). In this model, the underlying hardware becomes transient. 
* `mac-jane` and `vps-jane` cease to be distinct identities with localized state.
* They become **Compute Nodes** connected to the Philotic Web.
* The "Agent" is no longer a localized process; it is a portable state vector that materializes wherever compute is optimal.

## 2. The Context Graph as the Source of Reality
The Context Graph is the absolute center of gravity for the system. It replaces flat files, static configs, and SQLite silos. 
* **State as Graph:** Identities, sessions, active workspaces, and capabilities are all nodes and edges in the graph. 
* **Optimistic Writes:** Agents perform optimistic writes to their local materialized view of the graph and keep executing. The Ansible daemon handles asynchronous UDP broadcasting and conflict resolution in the background.
* **The Ansible is the Transport:** The Ansible layer serves as the replication, gossiping, and reconciliation engine that keeps the Context Graph synchronized across the mesh.

## 3. Tool Calling & The "Hotel" Routing Model
Tools must be abstracted from the physical filesystem to support true location independence.
* **MCP Wrapping:** All legacy tools are wrapped as MCP (Model Context Protocol) servers.
* **Abstract Invocation:** The LLM calls a tool (`mcp.vfs.read`). It has no awareness of *where* that tool lives.
* **Capability Discovery:** Capabilities are stored in the Context Graph (e.g., `Node:vps-jane` -> `HAS_CAPABILITY` -> `Tool:mcp.github`).
* **Just-In-Time Materialization (The Hotel):** If a requested tool's primary node is unreachable, the Ansible routes the request to a "similar hotel." The secondary node spins up a temporary, sandboxed instance of the tool, executes the request, and spins it down. The mesh bends around the agent's logic.

## 4. The Pacemaker (Distributed Cron)
Crontabs are no longer bound to a local OS cron daemon.
* **Cron as a Graph Node:** A cron is simply a `CronConstruct` node in the Context Graph.
* **The TimePulse:** Backbone nodes emit a UDP `TimePulse` to the mesh.
* **Mesh Consensus Execution:** When a pulse triggers a cron node, *any* available node can materialize the target agent and execute the payload, writing the `last_fired` state back to the graph to prevent duplicate execution.

## 5. Security & Isolation (The Apartments)
A unified "Big Sink" graph risks severe context bleed and security contamination.
* **Federated Permissions, Unified Infrastructure:** The graph is a single entity, but access is partitioned into "Apartments" (e.g., `Apartment: Coding`, `Apartment: Personal`).
* **Strict MCP Scoping:** When an agent queries the graph via an MCP server, the server acts as a bouncer, enforcing strict Role-Based Access Control (RBAC). A coding agent physically cannot traverse edges into secure personal domains.
* **Cryptographic Identity:** Every `BeaconMessage` and state mutation over the UDP mesh must be cryptographically signed by the originating node/agent to prevent spoofing.

## 6. The Execution Loop Separation
To prevent race conditions and handle the complexity of live chat streams (like Telegram):
* **The Control Plane (UDP):** The Ansible mesh handles instantaneous routing, capability discovery, and mailbox queuing. 
* **The Data Plane (TCP/IPC):** Heavy, ordered streams (like raw LLM token buffers for UI rendering) establish dedicated, localized pipes after the UDP control plane negotiates the connection.

---
**Next Actions for MVP 3:**
1. Scaffold the LLM-to-`ToolInvoker` bridge in Rust to enable graph queries.
2. Build the first Context Graph primitives (`graph.read`, `graph.write`, `graph.link`).
3. Develop the onboarding JSON seeder (`zeroclaw graph seed`) to migrate existing OpenClaw state into the new architecture.
