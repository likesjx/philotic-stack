# Synthetic authenticated Memgraph qualification

Status: prepared locally, not hosted, not a production installer. No real graph has yet run this candidate.

## Concrete behavior under test

Uses the same graph image as the existing passing fixture:
`memgraph/memgraph-mage@sha256:e8e03640071c57de6a13f2f4c5f9ed1789797dc5e8e771bbe62203b20e1b8c27`.

The candidate creates one empty graph with the server binary's MEMGRAPH_PASSFILE mechanism. It validates the file before executing the server, then verifies:
1. The only initial account is the expected synthetic account; its password works.
2. Wrong password, empty password, fully empty basic credentials, Bolt scheme=none and nonexistent username are rejected with the driver's explicit Unauthorized authentication error, not an arbitrary failure.
3. Positive queries work before and after the negative checks.
4. Authentication survives a controlled stop/start with a verified zero exit status and retained data.
5. Changing the bootstrap passfile does not overwrite the already-created user's password on restart.

The first account is an administrator. This candidate DOES NOT prove least-privilege runner roles or enterprise licensing. The initial admin password must not be mislabeled a scoped runner credential.

## Why the pre-exec validation is required

Inspected upstream tag v3.10.1 resolves to commit 0e77b786fc03fe4f92a18b9730940e11c62a0c32:
- [server entrypoint](https://github.com/memgraph/memgraph/blob/0e77b786fc03fe4f92a18b9730940e11c62a0c32/src/memgraph.cpp): MEMGRAPH_USER/PASSWORD take precedence over MEMGRAPH_PASSFILE; user creation occurs before Bolt server start.
- [passfile parser](https://github.com/memgraph/memgraph/blob/0e77b786fc03fe4f92a18b9730940e11c62a0c32/src/helpers.hpp): unreadable or malformed passfile warns and returns empty, allowing startup without a user.
- [first-user initialization](https://github.com/memgraph/memgraph/blob/0e77b786fc03fe4f92a18b9730940e11c62a0c32/src/glue/auth_handler.cpp): first user receives initial administrator setup; existing-user creation is not a password-reset operation.

These source observations are not proof that a Docker digest contains exactly those compiled bytes. The hosted test is the empirical check against the selected digest and explicit direct binary entrypoint. It avoids depending on an image entrypoint shell script.

The synthetic validator allows a single known username and generated 64-character hex password, owner-only regular file, no symlink/hardlink, no conflicting credential/config environment. It is deliberately not a general-purpose credential-file parser. Do not copy a production credential into this fixture.

## Hosted candidate

Publication layout in likesjx/philotic-stack:
- This directory's Python sources and requirements.txt → ops/graph-auth-qualification/
- graph-auth-workflow.yml.proposed → .github/workflows/synthetic-memgraph-auth.yml

Suggested branch: codex/synthetic-memgraph-auth. Publication/dispatch still needs the parent's approved exact candidate and ordinary review. The workflow runs only for pushes to that exact branch touching these paths, with read-only GitHub permission, a 15-minute disposable Ubuntu job, no artifacts, no repository credentials retained. The pure-Python Neo4j 5.28.2 and pytz 2025.2 wheels are SHA-256 locked from their public PyPI release metadata.

Run unit tests first:
`python3 -B -m unittest discover -s ops/graph-auth-qualification -p 'test_*.py' -v`

The hosted runner requires GITHUB_ACTIONS/CI, Linux, and the exact repository, a nonroot runner user and initially empty Docker containers. This is a trusted exclusive-host assumption, not a security boundary against the host administrator.

The graph has no network outside its private network namespace and no published port. Tests run through docker exec inside that namespace. Container root capabilities are dropped; the process runs under the runner's nonroot UID, with read-only image and only fresh scratch/data mounts. One container ID and unpredictable label are checked before stop/wait/remove. No broad prune or old data access. Logs expose fixed phase/failure categories, not passwords, query values or raw dependency messages. Startup and execution failures are not passes; cleanup uncertainty fails the job.

## Integration handoff

If this real-image test passes, apply the same validated private-file startup to the staged synthetic graph, change its runner graph config from empty credentials to the matching synthetic pair, and use the real bootstrap initialize-graph command before enabling admission. Then test the actual runner's authentication and memory operations. This standalone auth test does not run that bootstrap schema initializer, hotel, OAuth or remember/recall.

The existing service renderer remains unchanged. The actual production-shaped service needs a reviewed launcher/private passfile mount, separate decision for database-admin versus application credential, and verified authenticated startup. The renderer's listener healthcheck is insufficient.

No Docker/listener attempt was made locally. No VPS changes, credentials, grants, outside-agent communication or production access.

## Local review result

Independent review found a cleanup false-pass (success recorded before scratch removal), missing unauthenticated scheme=none coverage, insufficient positive-failure categories, and unchecked restart exit status. These are corrected and regression-tested. No Docker compatibility or actual authentication result is claimed from those checks.
