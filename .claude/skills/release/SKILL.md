---
name: release
description: Release 2fcast - bump the version, commit all pending work, tag vX.Y.Z and push to GitHub (the tag triggers the CI that builds and publishes the .deb). Use when the user asks to release, bump the version and push, or publish a new version.
argument-hint: "[X.Y.Z | major | minor | patch]"
disable-model-invocation: true
---

# Release 2fcast

Pushing a `vX.Y.Z` tag runs `.github/workflows/release.yml`. It checks that the tag
matches `crates/app/Cargo.toml`, runs lint, builds the `.deb`, runs the
Docker self-tests and publishes the GitHub release. Installed apps then offer
the update (`crates/core/src/update.rs`), so a broken release reaches users:
stop at the first failed step and report it. Don't work around a failure.

Argument: `$ARGUMENTS`

## 1. Preflight

```sh
git branch --show-current          # must be main
git fetch origin --tags
git status --short
git log --oneline origin/main..HEAD   # local commits not pushed yet
git log --oneline HEAD..origin/main   # must be empty (otherwise stop: pull/rebase first, ask the user)
```

- Current version: `sed -n 's/^version = "\(.*\)"/\1/p' crates/app/Cargo.toml | head -1`.
- Last tag: `git describe --tags --abbrev=0`.

## 2. Choose the version

- `X.Y.Z` given: use it. It must be greater than the current version.
- `major` / `minor` / `patch`: bump the current version accordingly.
- No argument: look at the pending changes and the commits since the last tag.
  New features → `minor`; only fixes/docs → `patch`. Say which one you chose and why.
- If the current version has **no tag yet** (`git tag -l vCURRENT` is empty)
  and no argument was given, ask whether to release the current version as it is or to bump it.
- The tag must not exist yet: `git tag -l vNEW` and `git ls-remote --tags origin vNEW` are both empty.

## 3. Check before changing anything

```sh
df -h /        # builds need a few GB
make lint
make test
```

Both must pass. If they don't, stop and show the failure.

## 4. Review what "everything" contains

Look at `git status --short` (including untracked files) and `git diff`.
Commit everything that belongs to the project. Stop and ask the user if you see:
secrets or credentials (`.env`, tokens, keys), large binaries or recordings
(`*.mkv`, `*.deb`, files over ~1 MB), build output, scratch or log files. Don't
commit those, and suggest `.gitignore` entries for them.

## 5. Commit the pending work

If there are changes besides the version bump, commit them first with
`git add -A` and a commit message in the repo's style (`feat: …`, `fix: …`,
`CI: …`; see `git log --oneline -10`). The message summarizes what changed.
Split the work into several commits only if it falls into clearly unrelated parts.

## 6. Bump, commit, tag

```sh
make version V=X.Y.Z     # crates, Cargo.lock, README .deb names
git diff --stat           # only Cargo.toml x2, Cargo.lock, README.md
cargo build -q            # Cargo.lock stays consistent
git add -A && git commit -m "Release X.Y.Z"
git tag -a vX.Y.Z -m "2fcast X.Y.Z"
```

Use an annotated tag: `--follow-tags` pushes annotated tags only.
End every commit message with the attribution lines the session asks for.

## 7. Push

```sh
git push origin main --follow-tags
git ls-remote --tags origin vX.Y.Z    # the tag is on GitHub
```

Never force-push, and never move or delete a tag that is already pushed. If the
push is rejected, stop and report it.

## 8. Report

```sh
gh run list --workflow release.yml --limit 1
```

Tell the user the new version, the commits that were pushed, and the CI run's
link. The release appears at https://github.com/ostretsov/2fcast/releases/tag/vX.Y.Z
once the run passes (about 15 minutes). Don't wait for the run unless the user asks.
