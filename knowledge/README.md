# SecureMesh Demo Operational Knowledge

Field-response guidance shipped with SecureMesh so a node has something to
answer from before any incident has been recorded.

## What this is

**Demonstration content, written for this project.** It is a plausible,
internally consistent set of field procedures at the level of detail a briefing
card would carry. It is *not* sourced from NDMA, NDRF, FEMA, the IFRC, or any
other authority, and it is not a substitute for the procedures an operating
agency issues.

Every document carries that label in its own text, so it survives being chunked
and quoted back by the retrieval pipeline — a passage that reaches an operator
out of context still says what it is.

## What it is not

- Not official. Do not present it as government or agency guidance.
- Not medical, structural, or legal advice.
- Not operational doctrine for any real deployment.

For a real device, this pack is the slot a genuine, licensed procedure set drops
into at provisioning time. The mechanism is the deliverable; this content is a
stand-in that makes the mechanism demonstrable.

## How it reaches the index

The files are compiled into the binary with `include_str!` and installed on an
explicit operator action — never on startup, and never over the network. See
`src-tauri/src/ai/knowledge_pack.rs`.

Installation is idempotent: each document is keyed by a SHA-256 of its
normalised text, so installing twice adds nothing.
