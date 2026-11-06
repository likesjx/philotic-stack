# Disposable native fixture inventory — source review only

The previously qualified issuer-owned workflow and its saved
`native-package-evidence.json` describe the following six containers. Image
IDs below are historical Docker configuration IDs, not current live state.

| Role | Historical image configuration ID | Network and explicit mounts |
|---|---|---|
| runtime | `sha256:b11e492bdf90ed00432bee24ac7f2019de9c94ed3a5b5e99b2f7b1fa1c9d6ea7` | anchor network namespace; checkout read-only at `/fixture`, runner scratch writable at `/tmp` |
| anchor | `sha256:b11e492bdf90ed00432bee24ac7f2019de9c94ed3a5b5e99b2f7b1fa1c9d6ea7` | internal bridge; no explicit bind |
| mongo | `sha256:3342b5975a68ccf08b047b4e13d6297a5888b83fe145797bda94d270af9dc298` | anchor network namespace; `/data/db` tmpfs |
| graph | `sha256:064aa6d4f8b2bcb90880def1752a746a15c42839fb190fed42ab7f8fb696219a` | anchor network namespace; no explicit mount |
| website | `sha256:6b5bc8a2039ad550d0e7c2c53303dfdc6ba2db059b8ad7afbea8359963650b57` | anchor network namespace; website custody bind read-only |
| proxy | `sha256:ec0c09f2aea697114e10ed73f0add5c646a7595e883ab0a7508966de98430d42` | internal bridge; proxy configuration bind read-only |

Workflow image references pin Mongo `d0d926f94df099bff534b7ee5b5986458131a22489dfff8664509af0c1e2ca9c`,
Memgraph `e8e03640071c57de6a13f2f4c5f9ed1789797dc5e8e771bbe62203b20e1b8c27`,
and Traefik `74fe8a2e317dbfbdca113347633b44a00a11d57170bc01147c150469418a4bc3`.
The runtime/anchor userland was built from a mutable `ubuntu:24.04` base and
package repositories; its historical configuration ID is evidence for that
run, not a reproducible pin for the proposed run. The website configuration
ID and source revision were checked in the workflow.

The historical runtime contained the test driver and packaged hotel, issuer
worker, test HTTP adapter, membrane and runner. The still-aborted new candidate
would instead use an in-process hotel. Website and proxy are separate clients.
The workflow also uses host-side `docker exec` readiness and database setup; an
earlier sequential privilege test creates a separate temporary Mongo container.
The historical pinned version attempted removal but did not produce an
independently verified cleanup receipt; the later modified local script is not
evidence of what that historical run did. The saved
evidence reports image IDs, resource limits, network mode and port bindings;
it does **not** include actual mount lists, image-declared volumes, PID-mode,
restart-policy or complete host-client inventory.

## Why the candidate remains blocked

The disabled preflight compares six created, never-started containers with a
separately supplied contract for images, commands, synthetic environment,
mounts, tmpfs, network and artifact hashes. It has no standalone entry point.
The required artifact set and contract must be independently pinned from the
reviewed fixture; deriving expected values from inspection would be circular.
It rejects every Docker volume until volume teardown is qualified. Checks assume
a trusted exclusive runner; they do not enumerate host clients or atomically
bind inspected state to a later start. Real Docker behavior remains untested.

The old workflow uses fixed `native-package-*` names and no fixture labels.
The local split-workflow draft uses unique names/labels, but has not been run.
Both mount `$PWD` and a separate runner scratch path; the proposed checker
requires all binds beneath one unique scratch directory. Actual anonymous or
named Docker volumes for the pinned images are not available in the saved
evidence, and the checker currently rejects them. A full candidate must capture
and verify exact source/destination/read-only mounts and any image-declared
volume identities, enumerate all clients, and provide independently pinned image
and instrumented-bundle provenance before using the checker.

Read and persist the synthetic Mongo admission/version and journal state while
the database is still alive. If the response or those observations are
uncertain, the fixture must fail; terminating the database is not evidence of
whether the change committed. The test entry point still aborts before any
listener. Container-stop observations are not a production authority fence,
host-native systemd qualification, independently verified cgroup emptiness, or
proof that volume/network removal occurred.
