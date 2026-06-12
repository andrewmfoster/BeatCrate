# Security Policy

BeatCrate is a local-only, single-user macOS desktop app. It ships unsigned
(aarch64), runs entirely on your machine, and makes no network calls — there is
no server, account, or remote data. Its data lives in a local SQLite database at
`~/Library/Application Support/BeatCrate/beatcrate.db`.

## Reporting a vulnerability

If you find a security issue, please report it privately rather than opening a
public issue:

- Use GitHub's [private vulnerability reporting](https://github.com/andrewmfoster/BeatCrate/security/advisories/new), or
- Open a regular issue for low-severity / non-sensitive findings.

Because this is a personal project maintained by one person, there is no formal
SLA — but reports are read and appreciated.

## Scope notes

- The app reads audio files and Ableton `.als` projects from a user-chosen
  folder. Parsing untrusted media/project files is the main local attack
  surface; the renderer↔backend bridge uses Tauri `invoke()` with
  least-privilege capabilities (dialog open + reveal-in-finder only) and a
  scoped asset protocol.
- The bundled CSP, capability grants, and IPC surface have been through several
  internal code audits.
- The companion VST3/AU plugin reads the same local SQLite DB read-mostly from
  inside a DAW host.
