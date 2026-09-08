<!--
Title must follow Conventional Commits (release-please builds the changelog
and version from it): `feat: …`, `fix: …`, `docs: …`, `chore: …`, `ci: …`,
`refactor: …`, `test: …`. Breaking change: `feat!: …`.
-->

## What

## Why

## Checklist

- [ ] `npm run typecheck && npm run lint && npm run format:check` pass
- [ ] `cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` pass
- [ ] UI changes checked in both light and dark themes
- [ ] Dependency changes: `bash scripts/gen-third-party-notices.sh` re-run and `THIRD_PARTY_NOTICES.md` committed
- [ ] No audio, transcripts or personal data in the diff or the PR description
