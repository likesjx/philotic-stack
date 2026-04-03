name: routing-reflex
description: Inspect and self-correct the agent's media routing and voice response policy at runtime. Use when entering a voice-capable context (Discord, TTS, audio input) or when routing is behaving unexpectedly.

# Routing Reflex Skill

Use this skill when you need to inspect or update your own routing policy — for example, when joining a Discord voice channel, when audio input is arriving but not being transcribed, or when voice output is not behaving as expected.

## When to invoke

- You receive a `voice.dialogue` task and suspect audio is being analyzed rather than transcribed
- You join a voice-capable channel (`/join` in Discord) and need to activate voice routing
- You want to enable or change TTS voice output
- You are diagnosing why media attachments are being handled unexpectedly

## What the routing reflex controls

### `media_routing_policy` — how incoming media is routed

| Path | Type | Default | Meaning |
|------|------|---------|---------|
| `media_routing_policy.voice_action` | string | `None` → `"analyze_media"` | Action for voice/audio input. Set to `"transcribe"` for STT. |
| `media_routing_policy.image_action` | string | `None` → `"analyze_media"` | Action for image attachments. |
| `media_routing_policy.document_action` | string | `None` → `"analyze_media"` | Action for document attachments. |
| `media_routing_policy.forward_media_to_model` | bool | `true` | If false, strips all attachments — text-only mode. |
| `media_routing_policy.strip_tools_on_media` | bool | `true` | Suppress tools on media turns to reduce prompt size. |

### `voice_response_policy` — how outgoing audio is synthesized

| Path | Type | Default | Meaning |
|------|------|---------|---------|
| `voice_response_policy.mode` | `"off"` \| `"auto"` \| `"on"` | `"off"` | `"auto"` = TTS when user spoke voice; `"on"` = always TTS. |
| `voice_response_policy.provider` | string | `None` | TTS provider (e.g. `"elevenlabs"`). |
| `voice_response_policy.voice_id` | string | `None` | Provider voice ID for this persona. |
| `voice_response_policy.send_text_caption` | bool | `true` | Send text alongside audio when mode is `"on"`. |
| `voice_response_policy.fallback_to_text` | bool | `true` | Fall back to text if synthesis fails. |

## Standard reflexes

### Entering a Discord voice channel

When `/join` is called or a `voice.dialogue` task arrives, activate voice routing:

```
agent.configure("media_routing_policy.voice_action", "transcribe", "set")
agent.configure("voice_response_policy.mode", "auto", "set")
```

If your persona has a voice identity:
```
agent.configure("voice_response_policy.provider", "elevenlabs", "set")
agent.configure("voice_response_policy.voice_id", "<your-voice-id>", "set")
```

### Leaving a voice channel or going text-only

```
agent.configure("media_routing_policy.voice_action", "analyze_media", "set")
agent.configure("voice_response_policy.mode", "off", "set")
```

### Diagnosing unexpected media routing

1. Check current policy by reading your session context or asking the operator
2. The default `voice_action: None` resolves to `"analyze_media"` — this is the most common cause of voice not being transcribed
3. Use `agent.configure` to correct the policy without restarting

## Important: `agent.configure` requires operator approval

`agent.configure` is in the `config` class and requires approval unless the class or tool is preapproved. If you are operating in a context where voice routing changes are expected (e.g. a voice-capable hotel), request that `config` class be preapproved:

```
agent.configure("approval_policy.preapproved_classes", "config", "append")
```

This requires one-time operator approval, after which routing reflex calls are frictionless.

## Reflex checklist

Before concluding voice is broken, verify:
- [ ] `media_routing_policy.voice_action` is `"transcribe"` (not `None` / `"analyze_media"`)
- [ ] `voice_response_policy.mode` is `"auto"` or `"on"` if outbound speech is expected
- [ ] `voice_response_policy.voice_id` is set if using ElevenLabs
- [ ] The model-router guest is running and has a valid API key for the TTS provider
