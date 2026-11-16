# Contributing

## Before you start

- Behaviour is defined by the PRDs in [docs/prds/](docs/prds/) (`<module>/<feature>.md`). A change no PRD covers needs a PRD decision first: `/blubird:create-prd`.
- Read [docs/README.md](docs/README.md), then the doc for the area you touch ([tool](docs/tool.md), [extension](docs/extension.md) or [desktop](docs/desktop.md)). Conventions and gotchas live there.
- Set up a working install with the [installation guide](docs/guides/installation.md). Rust tests also need `ffmpeg` on `PATH`.

## Workflow

This repo follows the `blubird` plugin's engineering workflow (PRD → epic → refined issues → one PR per issue). `main` is the only base branch: new features ship dark behind a `FEATURE_*` flag ([FLAGS.md](FLAGS.md)) and a release turns them on.

| Stage | Skill |
|---|---|
| PRD with its technical design | `/blubird:create-prd` |
| Epic and task issues | `/blubird:create-tasks` |
| Refined issue (label `refined`, board Ready) | `/blubird:refine-issues` |
| Implementation, local checks, draft PR | `/blubird:implement-issue` |
| Review | `/blubird:pr-review` |
| Docs sync | `/blubird:docs-updater` |

1. Work from a refined issue in its own worktree under `.worktrees/`. Branch from the latest `origin/main` as `issue-<n>-<short-slug>`.
2. One issue per PR. Tests come with the change: Rust tests in the module's `#[cfg(test)] mod tests`, TypeScript tests in a colocated `*.test.ts(x)`.
3. Docs in `docs/` that the change makes wrong are updated in the same PR.
4. Run the gates below, review the branch with `/blubird:pr-review`, push, and open a draft PR into `main` whose body links the issue (`Closes #27`).
5. Board status: Backlog → Ready → In progress → In review (CI green, comments answered) → Done. Only the owner merges; agents never merge.

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
