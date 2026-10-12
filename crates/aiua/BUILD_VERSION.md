# aiua build version

Release builds set `PHILOTIC_RELEASE_TAG` to the canonical `vX.Y.Z` tag
(optionally `-alpha.N`, `-beta.N`, or `-rc.N`) and `PHILOTIC_BUILD_SHA` to
the full lowercase 40-character source commit SHA. `aiua --version` prints
`aiua X.Y.Z (SHA)`, including the prerelease suffix when present.
Invalid or incomplete release metadata fails the build.

Untagged builds print the Cargo package version with `-dev`, followed by
the supplied SHA or `unknown`. They do not claim an unpublished release.
Cargo tracks changes to both build metadata variables.

The release workflow verifies the native binary banner against its generated
manifest after checking binary hashes. Installed-release verification binds
the downloaded manifest to the requested tag and platform before probing
the host, then requires the observed banner's exact version and full SHA.

Validation: `python3 -m unittest discover -s scripts/tests -p
test_release_manifest.py`; metadata unit tests are included in aiua's normal
test module. Local synthetic builds demonstrate the contract without creating
a release tag or claiming that the synthetic metadata identifies those builds.

This implements the CLI/manifest portion of release-train R2. Cargo package
versions and other runtime capability version fields remain separate work.
CI on the reviewed commit and native release builds on both supported
platforms are required before publication; local smoke is not live rollout proof.
