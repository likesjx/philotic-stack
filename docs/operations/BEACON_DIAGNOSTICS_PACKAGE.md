# Controller-only diagnostics package

The `beacon-controller-diagnostics-x86_64` CI artifact contains one deterministic `beacon-controller-diagnostics-x86_64.tar.gz`, its manifest, a package receipt and the supported `beacon-controller-diagnostics-v1` release/rollback contract. The inner archive has exactly four x86_64 Linux executables: OpenRouter, Gemini, Ollama and the legacy generic router. Packaging validates ELF architecture, the exact baked source SHA, supported system libraries and maximum glibc requirement of 2.39. No hotel, Philote, heal, gateway, database, configuration or credential is included.

CI checks out the exact head, verifies clean source before building and again before assembly, and sets the existing compile-time build SHA. The baseline controller did not use that provenance function despite its comment; this candidate adds its public SHA to the existing startup metadata so all four binaries retain the stamp. A stamp alone is not build proof: the reviewed workflow/source checkout and successful run bind the artifact to its build. Publication and promotion require both source acceptance and x86 package jobs to pass on that exact head.

The package's supported component selector can replace only a reviewed subset of those four executables. Including all four does not authorize installing all four. The generic router is selected only if the operator verifies it is actually registered; the source's defaults do not establish installed registration.

`package_release.py` verifies the reviewed archive digest, manifest digest and source before constructing a release plan. It rejects unknown components, extra files, traversal, symlinks, duplicate JSON keys, mismatched installed/running hashes, role confusion, missing rollback hashes and incomplete private evidence references. The planner never installs, drains guests, opens hotel state, connects to a provider or changes journal policy. The release owner must bind this supported contract to reviewed fenced swap/drain/restart operations; the old retired hotel-trio executor does not support this component set.

## Exact operator metadata needed

1. **Selected guests and executable hashes:** for each used component, the current installed SHA256 and running executable SHA256, fixed role, registered guest count, and digest of a private manifest mapping the exact guest identities to that component and its owning supervisor. Confirm all consumers, including other agents, are accounted for. Keep guest/agent identifiers and process command lines private. The last approved receipt already records OpenRouter hash `2ecb669b801417a3b749893e83eab99f0818aa63a5a81f90f6145835666884d2`; confirm it remains current. Gemini, Ollama and generic-router hashes/registration are unverified; omit components proven unused.
2. **Journal policy:** actual storage mode, configured age and byte limits, and digest of a private policy manifest identifying readers/groups and forwarding destinations. Zero age limit means no configured age expiration; it does not mean immediate deletion. Confirm reader audience and exports have been reviewed. No journal entries or raw logs are needed and no policy change is requested.
3. **Existing binary-release gates:** references to reviewed evidence for trusted install ancestry, the existing shared fence, historical worker terminal state and fencing of workers able to replace the selected binaries. These are existing unresolved gates, not new metadata investigations. This binary-only contract does not request a memory snapshot or the private-memory writer inventory. Existing UID-999 ancestry must not be silently accepted or changed by this package.

Preserve aiua/Philote/heal hashes from an approved installed receipt and confirm no drift when the executor acquires its fence. Exact private guest identities remain in the host-side manifest used by the release owner; the public receipt exports its digest and counts only.

## Review and rollback

The operator receipt schema is exercised by `scripts/beacon-diagnostics-57b/test_package_release.py`. To prepare a plan, using the exact source and digests from the CI package receipt:

Create the host-side receipt template with `python3 package_release.py template --manifest-sha256 REVIEWED_MANIFEST_SHA256`. It deliberately refuses planning until every required fact is supplied. Select only verified registered components; the template starts with OpenRouter, not an assumption that the other guests are installed. A configured journal mode of `auto` requires the operator to identify effective persistent/volatile/disabled storage.

```sh
python3 package_release.py plan \
  --archive beacon-controller-diagnostics-x86_64.tar.gz \
  --archive-sha256 REVIEWED_ARCHIVE_SHA256 \
  --manifest-sha256 REVIEWED_MANIFEST_SHA256 \
  --source REVIEWED_SOURCE_SHA \
  --operator-receipt reviewed-operator-receipt.json
```

Placeholder values are rejected. Evidence-manifest digests are review references; the planner cannot independently certify their truth. The executing release driver must revalidate installed/running identities and fence ownership immediately before mutation.

Under the existing fences, drain only the privately identified selected guests, save/hash all selected binary preimages, atomically replace the selected set, restart those guests and verify new running hashes/registrations. Preserve hotel/configuration/model bindings and all nonselected executables. Acceptance uses only separately approved synthetic inputs and bounded metadata; source CI does not prove production provider behavior.

On any partial failure, retain the fences, stop selected guests, restore **all** selected preimages by their pinned rollback hashes, restart their original guests and verify original running hashes/registrations. An unproven rollback retains the fence for operator recovery. A source/artifact package is not production activation permission; no install, security change or runtime probe occurs in this preparation.
