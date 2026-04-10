---
title: "Desktop Component Authoring Watched UAT"
doc_type: guide
domain: docs
status: active
last_updated: 2026-04-02
tags:
  - desktop
  - components
  - uat
  - philotic-web
---

# Desktop Component Authoring Watched UAT

Use this runbook to validate the desktop component create/edit flow served by `philotic-web`.

## Preconditions

- Work from [`/Users/jaredlikes/code/philotic-stack`](/Users/jaredlikes/code/philotic-stack).
- `philotic-web` has been rebuilt after the latest desktop bundle update.
- A local hotel is available for the desktop surface to query.

## Start The Surface

Terminal 1:

```bash
cd /Users/jaredlikes/code/philotic-stack
cargo run -p philotic-web -- serve
```

Wait for the local desktop URL to print, then open it in the browser if it does not auto-open.

## What To Test

1. Open the `Aiua` app and switch to the `Components` tab.
2. Confirm the panel shows a `New Component` button and each existing row has an `Edit` button.
3. Click `New Component`.
4. Create a disposable component using a safe manifest, for example:

```text
Guest ID: uat-component-01
Role: tool.echo
Hotel: default
Command: tool-runner
Args JSON: ["--help"]
Env JSON: {}
Component Config JSON: {}
Auto Start: off
```

5. Save the component and confirm:
   - the new row appears without a full page reload
   - the row shows hotel and command detail
   - the component remains inactive when `Auto Start` is off
6. Click `Edit` on that new component.
7. Change one or more fields, for example:
   - `Role` to `tool.echo.uat`
   - `Args JSON` to `["--version"]`
   - `Auto Start` to on
8. Save changes and confirm the row updates in place.
9. Use `Disable`, `Enable`, and `Restart` on an existing safe component and confirm the row state refreshes after each action.
10. Try one invalid form case, such as malformed `Env JSON`, and confirm the form shows a validation error instead of silently submitting bad data.

## What I Will Watch

In a second terminal, I can watch:

```bash
cd /Users/jaredlikes/code/philotic-stack
tail -f /tmp/philotic-web.log
```

If you are running `cargo run -p philotic-web -- serve` directly in the foreground, I can also watch that console output with you instead of a separate log file.

## Expected Outcome

- Desktop create/edit uses the same manifest contract as `phil component add`
- create and edit both round-trip `hotel`, `command`, `args`, `env`, `component_config`, and `auto_start`
- enable/disable/restart behavior still works after the authoring changes
- malformed JSON is rejected at the form boundary

## Honest Validation Level

Passing this runbook is `watched-live-green` for the desktop component authoring slice.
