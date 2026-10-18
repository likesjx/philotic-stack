# Official Muninn observe boundary verification

Dedicated branch-only verification workflow. No deployment triggers, VPS access,
production credentials, personal records, binary upload, packages or runtime installs.
The job downloads the public upstream v0.11.0 Linux AMD64 executable and requires
SHA256 `6a862d2fa39d48fe20b849b6ced66f2673822d1677e8bfe52300f8c16dc032d4`
and101319128 bytes before launch. This matches the measured production executable.
Its dirty build marker is also present in the official artifact and does not
establish local modifications.

The launcher imports an empty local rootfs into Docker (no registry image), binds
only standard runner runtimes, the verified binary and selected public fixture
files, and runs UID994 with no capabilities, no new privileges, read-only root,
no network, 640MiB memory/no swap, 0.5 CPU and128 PIDs. Fresh data is a256MiB tmpfs.
Backend environment is replaced. A public dummy password and short-lived observe
key exist only within the disposable synthetic backend. Container/image cleanup
runs in finally; the outer timeout is170s, the inner alarm140s, and job limit6min.

Required cases: own-vault MCP recall; body/query/direct-ID isolation on native
MCP/REST; create/delete denial; no extra records from denied writes; real gateway
adapter recall and exact vault-override/write-tool rejection; matching bounded
key-expiry metadata and denial after the SAME30-second key expires. Negative
checks require successful same-key own-vault controls so expiration is not
mistaken for scope enforcement. A safely ignored MCP query parameter is reported
separately from explicit rejection.

Visible record-response equality is advisory. Complete cognitive/background
side-effect suppression is unsupported and not a new acceptance requirement.
This does not verify deployed OAuth settings, public routing or client admission.

Local tests exercise classification and cleanup decisions, not live acceptance.
The workflow's terminal receipt records each real case, container exit/OOM state
and confirmed container removal. No ambiguous mutation is retried.
