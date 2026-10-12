# CarPlay conversation foundation

Source foundation on `codex/carplay-foundation`, rebased onto verified remote
`develop` `3144e42930d575532249e0133dc15e10b45afb58`. Original commit
`3f8ce9e6b410b7236384fde22df1d694fcc4f725` is preserved on local backup branch
`codex/carplay-foundation-before-update`. This is transitional and dormant by
default. No capability,
entitlement, provisioning profile, account, or signing configuration was changed.

## Apple requirements verified October 10, 2026

The [June 8, 2026 CarPlay Developer Guide](https://developer.apple.com/download/files/CarPlay-Developer-Guide.pdf)
lists voice-based conversational apps starting at iOS 26.4 and the entitlement
`com.apple.developer.carplay-voice-based-conversation` (page 11). Apple must
approve the category and add the entitlement to the developer account; neither
source code nor this document establishes eligibility (pages 9–11).

The category supports action sheet, alert, grid, list, tab bar, information,
point-of-interest and voice-control templates (page 12). This implementation uses
only `CPVoiceControlTemplate`, required while recording/voice services are active
(pages 26 and 28). Its states support at most two action buttons from iOS 26.4.
The driving interaction is status plus Talk/End, with no transcript, query answer,
image response, conversation history, or provider selection on the car display.
Audio activates only after Talk, using play-and-record/default with no mixing,
consistent with [Apple's WWDC26 guidance](https://developer.apple.com/videos/play/wwdc2026/212/).
Both category availability and template APIs are gated; macOS excludes the scene.

## Implemented behavior

- The phone owns `ChatSessionManager`; CarPlay borrows its existing selected
  conversation, agent routing, continuous PCM/VAD capture and voice replies.
  Select Beacon on the phone before preparing. CarPlay does not create a second
  session manager or hard-code a new route/provider.
- The installed voice template must complete successfully before any future
  preparation. Launch never opens the microphone. Current development's
  `externalVoiceEligible == false` is authoritative for this client; both the
  bridge and `startConversation` deny before capture. The car shows Voice
  unavailable, and phone preparation cannot request a microphone permission or
  grant eligibility. The adapter can acquire no active voice session today.
- Future eligible preparation is ephemeral and bound to transport epoch,
  hotel/agent/conversation and a car-scene preparation generation. A pending phone
  permission result cannot restore preparation after disconnect/reconnect.
  No permission prompt is requested from the car.
- End, disconnect, audio interruption, route change, audio-services reset,
  connection loss, and conversation change revoke preparation. No automatic
  resume. Lease cleanup releases local capture before waiting on transport and
  uses existing `VoiceTurnGate` invalidation to cancel queued/fallback speech and
  reject late acceptance, Final and audio. No duplicate reply-suppression
  authority was introduced.
- PCM permission waits now check cancellation, startup generation and competing
  active capture. Stops revoke pending starts before an engine exists. Session
  startup allocates an exact lease before awaiting; stale cleanup cannot revoke
  a replacement lease. CarPlay frame forwarding also checks transport scope and
  current selection. These mechanics are verified with synthetic permission
  races; successful microphone acquisition and CarPlay audio remain unverified.
- Latest on-device dictation, local Apple reply selection, correlated turn
  interruption and private voice denial stay intact. The upstream raw recording
  path is still denied by source policy; its full permission-race lifecycle is
  outside this bounded PCM/CarPlay change.

## Privacy limits

Disclosure and microphone permission never authorize egress. The Apple client's
existing `externalVoiceEligible` remains hard false, and the external speaking
provider remains unavailable through `VoiceProviderPreference`. No CarPlay
audio, transcript, context or provider request is dispatched. A trusted route
and complete-source privacy contract is still missing for production voice
dispatch; adding a UI flag, preference, userJSON or a consent dialog cannot
replace that authority. The previous stale-base disclosure-only approach was
removed during this integration. Voice/context/provider protection must stay
fail-closed until the corresponding server and client contracts are installed
and verified.

## Capability work is deferred

Compile source with `SWIFT_ACTIVE_COMPILATION_CONDITIONS="DEBUG PHILOTIC_CARPLAY"`
to verify the dormant iOS adapter. This alone does not expose a CarPlay app.
Do not request entitlement activation now. First resolve and verify trusted
voice/privacy routing and the correlated hands-free transport contract. Only
after a separately approved capability step and Apple's actual category approval
could the granted entitlement/profile and CarPlay scene registration be added.
The future scene-manifest entry is:

```xml
<key>UIApplicationSceneManifest</key>
<dict>
  <key>UIApplicationSupportsMultipleScenes</key><true/>
  <key>UISceneConfigurations</key>
  <dict>
    <key>CPTemplateApplicationSceneSessionRoleApplication</key>
    <array><dict>
      <key>UISceneClassName</key><string>CPTemplateApplicationScene</string>
      <key>UISceneConfigurationName</key><string>Philotic CarPlay</string>
      <key>UISceneDelegateClassName</key><string>$(PRODUCT_MODULE_NAME).CarPlaySceneDelegate</string>
    </dict></array>
  </dict>
</dict>
```

Preserve the SwiftUI phone scene configuration during that step. Do not enable
an unrelated category or fabricate eligibility to make the icon appear.

## Verification and remaining work

Lifecycle tests cover no capture at launch, exact-context preparation, repeated
starts, denial/failure, suspended-start disconnect and reconnect, connection
loss, context change, stale phone preparation and no automatic restart. Template tests check the eight
status states, two controls and the SDK's actual Objective-C scene callbacks.
Full test/build results are recorded in `docs/task.md`.

Historical first-commit results, before integration onto current develop:

| Check | Result |
| --- | --- |
| PhiloticKit, `swift test --build-system native --scratch-path /tmp/philotic-carplay-kit` | 127 executed, one existing skip, zero failures |
| Four initial lifecycle tests, same command with `--filter CarPlayConversationLifecycleTests` | Four passed |
| Hosted macOS app suite, `xcodebuild ... -scheme PhiloticApp-macOS test` | 31 passed, zero failures; XCTest app startup excludes real settings/Keychain/hotel access |
| Gated iOS simulator `build-for-testing`, both arm64 and x86_64 | Passed for the previous source commit; latest-base rerun results are recorded below at closeout |
| Normal iOS simulator and macOS builds, feature flag absent | Passed |
| iOS 27 simulator execution of `CarPlayTemplateTests` | Blocked: Xcode could not connect/bless its device test service (`com.apple.instruments.deviceservice.lockdown`). Simulator booted; stalled test command interrupted, no tests executed |

Read-only simulator diagnosis on the updated slice: macOS 27.2 (26B5101f),
Xcode 27.0 (27A5228h), installed iOS 27.0 runtime (24A5390f), no booted devices.
The retained test log reports `DTServiceHubClient failed to bless service hub`
and `localDeviceConnectionWithError:`. This identifies a runner/device-service
failure, not its root cause. No simulator services were restarted and no
security, account or simulator settings changed. The failed simulator attempt
is not evidence of successful CarPlay presentation or test execution.

Generate the app project with `xcodegen generate` in `PhiloticApp` first. The
gated compile command from that directory was:

```sh
xcodebuild -project PhiloticApp.xcodeproj -scheme PhiloticApp-iOS \
  -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath /Users/jaredlikes/Documents/Codex/2026-10-10/task/build-carplay \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO \
  SWIFT_ACTIVE_COMPILATION_CONDITIONS='DEBUG PHILOTIC_CARPLAY' build-for-testing
```

Hosted macOS tests used `/tmp/philotic-carplay-mac-tests` as DerivedData and
`CODE_SIGNING_ALLOWED=YES CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=-
CODE_SIGN_STYLE=Manual` on the command line for ad-hoc **test-output** signing.
No project signing settings or developer accounts were changed. Default Swift
27's `swiftbuild` test engine first hit resource-fork signing detritus in the
Documents build output; native SwiftPM with `/tmp` scratch passed. This is a
build-location/toolchain issue, not evidence of a failed lifecycle test.

Storage was checked before builds (49 GiB free). Task-specific outputs used
the workspace and `/tmp`; no drive was mounted, migrated, or reconfigured.
Memory/bootstrap telemetry was skipped under the explicit no-personal-memory
instruction. Rust-wide baseline checks were omitted for this bounded native
Swift-only change and limited-storage request; the affected Swift test/build
gates above were run instead. Local commit only; remote push/publication is
left for approval under the source-work authorization boundary.

Simulator compilation is distinct from simulator execution and a live CarPlay
scene. A true CarPlay presentation/capture test still needs capability activation.
Real-car verification needs separate device-install approval, then locked-phone
access, vehicle mic/speaker routing, Siri/phone-call interruption, reconnect,
revoked microphone permission, prolonged sessions and residual echo tests. The
existing input-only engine does not provide verified acoustic echo cancellation;
VAD barge-in resistance is not proof of vehicle echo handling. No real hotel
traffic, microphone audio or credentials were used during this source work.

## Updated-base verification (2026-10-10)

Validated against fetched `develop` at
`3144e42930d575532249e0133dc15e10b45afb58` after preserving the original
commit on `codex/carplay-foundation-before-update`.

- PhiloticKit: `swift test --build-system native --scratch-path /tmp/philotic-carplay-updated-kit`: 140 executed, one existing skip, zero failures; includes five lifecycle tests.
- Hosted macOS: `xcodebuild -project PhiloticApp.xcodeproj -scheme PhiloticApp-macOS -destination 'platform=macOS' -derivedDataPath /tmp/philotic-carplay-updated-mac CODE_SIGNING_ALLOWED=YES CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual test`: 55 passed, zero failures on final source. Four PCM startup/lease tests and two session policy/ownership regressions exercise denial, canceled permission, repeated starts and unowned cleanup without hardware or hotel traffic.
- Gated iOS: the above simulator `build-for-testing` command with `/tmp/philotic-carplay-updated-ios` DerivedData: **TEST BUILD SUCCEEDED**, arm64 and x86_64. This compiles template tests but does not execute them.
- Default macOS source is covered by the hosted suite. Default iOS simulator `build` with the feature flag absent also passed (same DerivedData and signing-disabled command).
- Independent source reviewer: no blocking findings for the dormant foundation. Review examined fail-closed eligibility, transport/context selection, exact capture ownership, cancellation and delayed stream/transcript completions. The source fixes recheck leases after suspension and before each VAD event. Review did not verify simulator execution, audio routing, entitlement eligibility or real-car behavior.

Logs are retained in the task directory as `carplay-updated-kit.log`,
`carplay-updated-mac-final.log`, `carplay-updated-ios.log`, and
`carplay-updated-ios-default.log`. Earlier sandbox-only build attempts failed
on toolchain macro/service access; authorized toolchain retries passed.
Untracked duplicate filenames ending in ` 2`/` 3` were preserved in
`/tmp/philotic-carplay-duplicate-quarantine` to prevent duplicate source
compilation; regenerated project duplicates remain untracked. No other
checkout or agent work was changed.

Activation is blocked on authoritative voice privacy/dispatch eligibility and
correlated hands-free transport support. No entitlement request is needed for
this source-only slice. Subsequent capability/account, device installation or
live-car work requires separate authorization after those prerequisites exist.
