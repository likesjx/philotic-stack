---
title: "Discord Watched UAT Runbook"
doc_type: workflow
domain: workflow-docs
status: active
last_updated: 2026-04-02
tags:
  - discord
  - uat
  - watched-live
  - membrane
  - operator
related_docs:
  - ../architecture/DISCORD_MEMBRANE_PROPOSAL.md
  - ../architecture/ARCHITECTURE_STATUS.md
  - ../process/WORKFLOW.md
task_refs:
  - ../task.md
---

# Discord Watched UAT Runbook

This runbook is for an operator running watched-live Discord validation from the main `develop` checkout.

Current truth as of April 2, 2026:

- `develop` contains both `membrane-discord` and `membrane-telegram`
- Discord watched UAT is currently a side-by-side launch:
  - `aiua` hotel in one terminal
  - `membrane-discord` in another
- text ingress/egress and gateway lifecycle are the honest primary acceptance path today
- voice is only partially landed:
  - `/join` currently acknowledges and primes pending voice state
  - it does **not yet** send the outbound Discord voice-state join request by itself
  - so voice-channel join is not yet a must-pass acceptance criterion

## What This Run Validates

Must-pass for this slice:

1. `membrane-discord` starts cleanly against the local hotel
2. Discord gateway connects and stays up
3. lease acquisition succeeds
4. slash commands register or degrade with a clear non-fatal warning
5. Discord text ingress reaches the agent path
6. text replies return to the Discord channel
7. runtime failures degrade into bounded retry/backoff rather than panic/exit

Nice-to-observe but not yet required:

1. `/join` ack path
2. voice gateway preparation logs
3. future voice-channel roundtrip

## Preconditions

Before starting:

1. You are in [/Users/jaredlikes/code/philotic-stack](/Users/jaredlikes/code/philotic-stack)
2. `git status` is clean
3. `git branch --show-current` is `develop`
4. `DISCORD_BOT_TOKEN` is available
5. `DISCORD_APPLICATION_ID` is available
6. the bot is invited to a test Discord server with:
   - `View Channels`
   - `Send Messages`
   - `Use Application Commands`
   - `Read Message History`
   - `Connect`
   - `Speak`
7. gateway intents are enabled in the Discord app as needed for:
   - guilds
   - guild messages
   - message content
   - guild voice states
8. your local agent/model path is configured well enough to answer at least a text turn

## Terminal 1: Start The Hotel

Run:

```bash
cd /Users/jaredlikes/code/philotic-stack
RUST_LOG=info cargo run -p aiua -- --hotel discord-uat 2>&1 | tee /tmp/discord-uat-aiua.log
```

Expected:

- hotel starts cleanly
- socket appears at `/tmp/philotic-discord-uat.sock`
- no immediate crash loop

## Terminal 2: Start The Discord Provider

Run:

```bash
cd /Users/jaredlikes/code/philotic-stack
PHILOTIC_HOTEL_SOCKET=/tmp/philotic-discord-uat.sock \
DISCORD_BOT_TOKEN='YOUR_BOT_TOKEN' \
DISCORD_APPLICATION_ID='YOUR_APPLICATION_ID' \
RUST_LOG=info \
cargo run -p membrane-discord -- \
  --agent-id agent-01 \
  --guest-id membrane-discord-01 \
  --hotel-socket /tmp/philotic-discord-uat.sock \
  --node-id local-aiua-01 2>&1 | tee /tmp/discord-uat-membrane.log
```

Expected log markers:

- `membrane-discord starting for agent [agent-01]`
- `Connected to hotel IPC`
- `Discord bot token resolved`
- `Discord gateway lease acquired`
- `membrane-discord seat loop started`
- `Gateway READY`

Acceptable warning:

- slash command registration may fail with a clear warning if the application config is incomplete

Not acceptable:

- panic
- immediate exit
- repeated crash without backoff

## Terminal 3: Watch The Runtime

Run:

```bash
ps aux | rg 'aiua|membrane-discord|philote|model-router'
tail -f /tmp/discord-uat-aiua.log
```

Optional second watcher:

```bash
tail -f /tmp/discord-uat-membrane.log
```

## Discord Client Steps

### Step 1: Confirm Bot Presence

In Discord:

1. open the test server
2. confirm the bot appears online

Pass:

- bot is visible and online after provider startup

### Step 2: Confirm Slash Commands Exist

In a test text channel:

1. type `/`
2. confirm you can see:
   - `/status`
   - `/join`
   - `/leave`
   - `/tts`
   - `/new`

Pass:

- command registration is visible in Discord

### Step 3: Text E2E

In a test text channel:

1. send a simple plain-text message to the bot or in the bound channel
2. watch Terminal 2 for inbound gateway/message handling
3. wait for the agent reply

Pass:

- inbound Discord text reaches the membrane
- a task is emitted toward the hotel/agent path
- a text reply returns to the same Discord channel

If text does not return:

- note whether ingress happened anyway
- note whether failure is in gateway, IPC, agent/model, or egress

### Step 4: Slash Command Sanity

Run:

1. `/new`
2. `/status`

Pass:

- `/new` gets an immediate response
- `/status` routes through the agent path and produces a response if the agent/model path is healthy

### Step 5: `/join` Reality Check

Run:

1. join a voice channel yourself
2. invoke `/join` in a text channel

Expected current behavior:

- Discord acknowledges the command
- membrane logs pending voice setup behavior

Current limitation:

- this slice does **not yet** issue the outbound Discord voice-state join request automatically
- so lack of actual bot voice-channel join is a known current gap, not a surprise failure

## Optional Failure/Recovery Watch

If you want to validate the new retry behavior:

1. stop the Discord membrane process with `Ctrl-C` and restart it
2. or induce a temporary network interruption
3. watch for bounded retry logs

Expected:

- runtime failure logs mention retry delay
- delay grows from `1s` upward and caps at `600s`
- process does not panic-loop

## Pass / Fail Summary

### Watched-live-green for the current slice

You can call this `watched-live-green` for the current Discord slice if all of these are true:

1. hotel starts cleanly
2. `membrane-discord` starts cleanly
3. gateway lease is acquired
4. gateway reaches ready state
5. slash commands are visible
6. Discord text ingress produces a returned text reply
7. no panic or immediate crash occurs

### Not required yet

These do **not** block `watched-live-green` for the current slice:

1. actual voice-channel join
2. Discord voice roundtrip audio

Those remain follow-on acceptance once outbound join initiation is implemented.

## What To Report Back

When you finish, report:

1. whether text E2E passed
2. whether slash commands appeared
3. whether `/join` only acknowledged or did anything more
4. any panic, retry, or lease-related log lines
5. exact timestamps from `/tmp/discord-uat-membrane.log` if something failed
