# Contributing

## Before you start

- Behaviour is defined by the PRD, [docs/prds/meeting-recorder.md](docs/prds/meeting-recorder.md). A change it doesn't cover needs a PRD decision first: open an issue.
- Read [docs/README.md](docs/README.md), then the doc for the area you touch ([tool](docs/tool.md) or [extension](docs/extension.md)). Conventions and gotchas live there.
- Set up a working install with the [installation guide](docs/guides/installation.md). Rust tests also need `ffmpeg` on `PATH`.

## Workflow

1. Work from an issue. Branch from `main`, e.g. `issue-27-install`.
2. Keep each PR to one issue. Write tests with the change: Rust tests in the module's `#[cfg(test)] mod tests`, extension tests in a colocated `*.test.ts(x)`.
3. If the change alters behaviour a file in `docs/` describes, update that doc in the same PR.
4. Run the gates below, push, and open a PR into `main` that links the issue (`Closes #27`).
5. CI must pass before merging.

## Gates

Same as CI ([.github/workflows](.github/workflows)). No git hooks run them for you.

```sh
# Rust, repo root
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test

# Extension, extension/
npm run build && npm run typecheck && npx biome ci . && npm test
```

`cargo fmt --all` and `npx biome check --write .` fix formatting.

## Commits

`type(#issue): summary`, imperative and lower case, for example:

```
feat(#27): meeting install
fix(#26): process even when origin-private cleanup fails
test(#17): cover setup failure and stop while paused
docs(#59): generate agent docs
```

## Hard rules

- Never log or print the Deepgram key or transcript text.
- In `native-host` mode stdout is Chrome's message channel: the tool never writes anything else to it.
- Never change the `key` in `extension/manifest.config.ts`; it fixes the extension ID the native host trusts.
- No new Rust crate or npm package without a PRD decision.

Agents follow the same rules, summarised in [CLAUDE.md](CLAUDE.md).
