# Product model

This document exists because of a specific grievance: **vMix's licence cost, and
what it locks away.** That is the reason Rhevia is being built, so it belongs in
the design record rather than in someone's head.

## The thing being reacted to

From the vMix User Guide, not from impressions:

| Feature | Availability |
|---|---|
| MultiCorder (ISO recording of multiple inputs) | 4K and Pro editions only (p8) |
| Instant Replay | 4K and Pro editions only (p8) |
| vMix Call guests | 1 on HD, 4 on 4K, 8 on Pro (p177) |

The pattern is the problem, not any single line. A user buys a licence, builds a
workflow, and then discovers the feature they now need sits one or two tiers up.
The software on their disk is already capable of it — it is switched off. And
because the tiers are tied to *resolution* as well as features, wanting 4K
output drags along a price step that has nothing to do with resolution.

> Prices change and are not recorded here. The **structure** is what matters and
> the structure is what we are rejecting.

## The commitment

**Rhevia ships one binary with every feature enabled.**

Not as a marketing position — as an architectural constraint, because that is
the only form of the promise that survives commercial pressure:

- **No licence-tier checks anywhere in the codebase.** Not disabled, not behind
  a flag, not `if (edition >= Pro)`. Absent. Once those checks exist they are
  trivial to switch on, and every future cash-flow problem becomes an argument
  for switching them on.
- **No resolution gating.** 4K is a number in a config struct. Charging for it
  is charging for a constant.
- **No input-count gating.** The limit is the machine's, and the machine belongs
  to the user. See the [tiered decode scheduler](03-engine.md#decode-tiers) —
  our job is to raise that ceiling, not to install one below it.
- **No guest-count gating on RheviaLink.** The cap is bandwidth, which is a real
  cost, not a price-list entry.

Writing the checks in and promising not to use them is the same as not making
the promise. Their absence is the promise.

## So where does revenue come from

Honest answer: the parts that genuinely cost money to run, plus the software
itself at a single price.

| Source | Why it is defensible |
|---|---|
| **One licence, one price, all features** | Simple, and the entire point |
| **TURN relay bandwidth** | A relayed RheviaLink session consumes real egress — roughly 2.25 GB per hour per 5 Mbps stream. Direct P2P connections cost us nothing and must stay free. |
| **Hosted shorts rendering** | Optional. Renders locally for free; pay only to offload it. |
| **Template marketplace** | Revenue share, and it costs us nothing to supply since titles are [HTML/CSS](04-beyond-vmix.md#6-titles-designers-already-know-how-to-make). |

The distinction that keeps this honest: **charge for marginal cost, not for
unlocking code the user already has.** Bandwidth and compute are marginal costs.
A boolean in a config file is not.

## Upgrade policy

The second half of the grievance is paid version upgrades — buying vMix, then
buying it again for the next major version.

- Perpetual licence for the version bought, which keeps working forever.
- Updates free within a major version.
- If a major-version upgrade is ever charged for, it is because a year of work
  went into it, and the previous version keeps working untouched — no forced
  migration, no remote deactivation, no phone-home requirement to keep running.
- **Offline operation is permanent.** A live production must never fail because
  a licence server was unreachable. No activation check runs during a show.

## Why this is written down

Three years from now there will be a quarter where adding a Pro tier looks like
the obvious fix. This document is here to make that a deliberate reversal of a
founding decision rather than a quiet product tweak — and the absence of the
licence-check plumbing is there to make it expensive enough to think twice.
