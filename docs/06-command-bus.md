# One command bus, many front-ends

Remote control is not a feature to add later. It is a consequence of one
decision made now, and an expensive retrofit if that decision is missed.

## The decision

**Every action in Rhevia is a command on a single bus. The local UI has no
privileged path.** Clicking Cut in the desktop window and cutting from a phone
on the other side of the world produce the identical command object.

```
     local UI  ─┐
  remote control ┤
       scripting ┼──►  command bus  ──►  engine  ──►  state broadcast  ──► all front-ends
       HTTP API  ┤
     MIDI / X-keys ┘
```

If the local UI is allowed to call the engine directly "just this once", every
other front-end drifts out of sync, and remote control becomes a permanent game
of catching up with features it cannot reach. vMix shows the end state of that:
the Web Controller, the scripting API and the HTTP API each expose a different,
partial slice of what the application can actually do.

The rule that prevents it: **if the UI can do it, it is a command, and therefore
it is already remote-controllable, scriptable and bindable to a MIDI key.** No
extra work per feature.

## Why this is cheap for us and hard for vMix

vMix's Web Controller is a local web server. It works on the LAN, which means
the operator has to be in the building.

Rhevia already has the hard part built: [RheviaLink](01-remote-camera.md) pairs
two devices across any two networks by QR code and gives them an authenticated
WebRTC connection with NAT traversal. Control traffic is a **data channel on
that same connection**.

So remote control costs us a message type, not an infrastructure project:

| | vMix Web Controller | Rhevia |
|---|---|---|
| Range | same LAN | anywhere with internet |
| Setup | find the machine's IP | scan a QR code |
| Transport | HTTP, plaintext on LAN | DTLS-encrypted data channel |
| Coverage | a subset of functions | everything, by construction |

And because it is the same pairing flow, **one phone can be a camera and a
control surface at once** — shooting a shot while cutting the show.

## Command shape

Commands are data, never closures. They must survive a network hop, a log file,
and a replay.

```
{ id, at, source: "local" | "remote" | "script" | "midi", op: "input.cut", args: {...} }
```

This buys three things for free:

- **Undo** — a command log is an undo stack. vMix has a single Undo button;
  a log gives a real history.
- **Show replay** — replaying a log against the same media reproduces the show
  exactly, which is how you debug "it broke during the live stream" after it is
  over.
- **Multi-operator** — several people driving one show, with every action
  attributed. vMix is single-operator by construction. This is the feature that
  is genuinely out of reach for them without a rewrite.

## What this costs today

One rule, applied from the first engine commit: no front-end touches engine
state directly. That is it. Enforcing it later means rewriting every call site,
which is why it is written down before the engine exists.
