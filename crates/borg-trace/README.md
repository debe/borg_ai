# borg-trace

Parses Claude Code and omp (oh-my-pi) session transcripts into one canonical stream of touches: files
read and edited, searches, and shell commands with a fingerprint of their failure.
[borg](https://github.com/debe/borg_ai) uses it in its hooks and to read past sessions (`borg assimilate`).

**Internal API, no semver.** This crate exists for borg's own crates, which pin it `=` to the same
version. Any release may change or remove any item.

License: MIT OR Apache-2.0.
