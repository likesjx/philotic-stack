# A2UI specification files

`v0_9/` holds verbatim copies of JSON Schema files from the A2UI project
(<https://github.com/a2ui-project/a2ui>, `specification/v0_9/`, branch `main`,
fetched 2026-10-02), licensed under the Apache License, Version 2.0
(<https://www.apache.org/licenses/LICENSE-2.0>). They are unmodified and pin the
wire contract Philotic surfaces are validated against.

| File | Upstream path |
| --- | --- |
| `v0_9/server_to_client.json` | `specification/v0_9/json/server_to_client.json` |
| `v0_9/client_to_server.json` | `specification/v0_9/json/client_to_server.json` |
| `v0_9/common_types.json` | `specification/v0_9/json/common_types.json` |
| `v0_9/client_data_model.json` | `specification/v0_9/json/client_data_model.json` |
| `v0_9/basic_catalog.json` | `specification/v0_9/catalogs/basic/catalog.json` |

`philotic_desktop_v1.json` is Philotic's own catalog: a strict subset of the
basic catalog plus the `Table` extension. It is the single allowlist read by the
hotel validator (`ansible-mesh-core::surface`) and the web renderer.
