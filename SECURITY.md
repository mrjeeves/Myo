# Security Policy

Myo runs locally and **updates itself**, so we take supply-chain and update
integrity seriously. Thanks for helping keep it safe.

## Supported versions

Myo is pre-1.0 and ships from the latest release. Fixes land on `main` and in
the next tagged release; please test against the latest `myo --version` before
reporting.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

- Preferred: open a private report via GitHub →
  **Security** tab → **Report a vulnerability** (private advisory).
- Or email **cpjeeves@gmail.com** with details and, if possible, a
  proof-of-concept.

We aim to acknowledge within **72 hours** and to keep you updated as we
investigate. We're happy to credit you in the release notes once a fix ships
(let us know if you'd prefer to stay anonymous).

## Update integrity — current model & roadmap

The self-updater downloads release binaries over TLS and **verifies each one's
SHA-256** against the checksum sidecar published with the same GitHub release
*before* it can replace the running binary. It refuses downgrades, refuses to
treat a checksum/signature sidecar as the executable, and never swaps a binary
out from under a running process (it stages and applies on next launch). See
[`docs/auto-update.md`](docs/auto-update.md) for the full design.

Today this protects against corrupted or truncated downloads and tampering with
the artifact in transit. It does **not** yet provide cryptographic *authenticity*
independent of GitHub — i.e. it trusts that the checksum published alongside the
release is genuine. **Artifact signing (e.g. minisign / cosign) with a pinned
public key is on the roadmap** to close that gap; if you have thoughts on the
design, we'd love to hear them.
