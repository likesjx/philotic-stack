# Retained runtime state qualification and separate Claude acceptance

The public `muninn-oct22-composition.yml` workflow now runs all eight historical
SDK/state cases using retained executables. Its earlier cross-repository issuer
checkout failed before HTTP tests; that run is not composition evidence.
Claude HTTP/OAuth acceptance belongs in issuer-owned CI. This public workflow
does not check out or export private issuer sources, credentials or artifacts.

Exact retained inputs:

- Candidate source `230d5aaa1ccdf9fe2b18fd14c7d66c91cff95219`, executable SHA256
  `7f789203dc87790740399885f093b940d22f66bff66dd6541c5fdf6f3345b9dd`.
  Producer run `39671418572`, workflow `f7f1d8484df19c9e39bbc50c0435ae89b6bb55bf`,
  artifact `12379818356`, archive
  `0e747ae7c5e0653c782becb1f916de114353fc866d76bd357678ed765309abf3`.
- Historical source `56b0cdc2ed1123aad01b22eb88fa7b43baf02bf6`, executable SHA256
  `0beb8f1c02bb12c34acdedbf5a062950465e0dc681cbf3fdeb0b6a49f6254c82`.
  Producer run `39665912963`, workflow `ab26ebc5200458f5abc07597583b9b372474fc7f`,
  artifact `12377009217`, archive
  `345d486e6418e460a5ef06d5055369e9c85ce5884814596448b7ddbd6d539d84`.

Both producer identities, successful runs, artifact association/expiry/digests,
archive structure and executable hashes are checked before use. The candidate
verifier also checks its producer receipt and prior results. Only the controlled
historical SDK guest is rebuilt, from the exact historical source with locked
dependencies. Neither hotel executable is rebuilt. Preparation downloads public
dependencies; the test container has no network, host mounts or privileged mode.

Test08 now captures both `aiua_context.db` and `whisper_training.db` using
SQLite backup APIs while the synthetic hotel is stopped. Open fixture connections
keep committed WAL records present in both databases, and assertions require those
records in the backups. After candidate startup the test adds a training row and
candidate-only marker, restores both databases and synthetic key/config/selection,
verifies integrity and logical training state, then restarts the historical hotel.
These are disposable fixture paths, not assertions about installed production paths.
All eight original cases and their assertions remain. A ninth case restores prior
training-database absence (including WAL/SHM/journal absence) before historical
restart. Backup and restoration require all fixture hotel processes stopped;
both source connections remain open, their initial data versions are captured
before any backup, and both are rechecked after all copies. A tenth deterministic
negative case commits changes to both source databases between the backups and
requires refusal of the resulting mixed pair. Presence/absence is rechecked too;
source and destination connections close on success or failure.

The producer's three migration unit tests and broker tests remain separate prior
evidence. The historical binary is a rebuild, not the installed b7d0 executable.
Receipts retain `installed_equivalence=false`: controlled SDK tests do not prove
installed guests/provider loops, all persistent stores or a live cutover.

The existing activation operator still admits only b7d0 with its own missing
same-binary qualification. No operator guard changes here. Production schema,
effective profile/state/key paths, all writers, guest protocols, supervisor
drop-ins and an executable-plus-state restoration contract remain required.
Credential migration atomicity does not cover preceding configuration import.
No deployment, real memory disclosure, OAuth flag change, provisioning or grant
is authorized by this workflow or its receipts.
