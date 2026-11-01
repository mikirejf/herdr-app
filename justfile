set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

notes-clip scene version="":
    bun scripts/notes-media.ts clip "{{scene}}" "{{version}}"

notes-upload version:
    bun scripts/notes-media.ts upload "{{version}}"

notes-check version:
    bun scripts/notes-media.ts check "{{version}}"

check: test

notices:
    bun scripts/notices.ts

# Before a release: tests, docs, the next version's notes and clips, then what
# changed since the last release, to review by hand.
[unix]
pre-release: test docs-check
    #!/usr/bin/env bash
    set -euo pipefail
    version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
    last=$(git describe --tags --abbrev=0 --match 'v*')
    if grep -q "^## \[$version\]" CHANGELOG.md; then
      bun scripts/notes-media.ts check "$version"
    else
      echo "note: CHANGELOG.md has no [$version] entry yet: write the release notes before tagging."
    fi
    echo; echo "Since $last:"; git log --oneline "$last"..HEAD
    echo
    if git diff --quiet "$last"..HEAD -- src/SKILL.md; then
      echo "review: the agent manual (herdr-app --skill, src/SKILL.md) is unchanged since $last. Does it still match the changes above?"
    else
      echo "review: the agent manual (herdr-app --skill) changed since $last. Read it once more."
    fi
    echo "review: the mod guide (defaults/AGENTS.md), README.md and docs/ describe the changes above."

build:
    cargo build --locked

test:
    bun test scripts/files.test.ts
    cargo test --locked

docs-generate: build
    bun scripts/docs.ts generate

docs-check: build
    bun test scripts/docs.test.ts
    bun scripts/docs.ts check

# Builds target/browser-dev/Herdr Dev.app (scripts/browser-dev-bundle): the
# `browser` feature, CEF's framework and helpers. CEF_PATH (default
# ~/.cache/herdr-app/cef) keeps CEF's ~600 MB download outside the repo; a
# bare `cargo build --features browser` without it is refused (build.rs).
# Herdr with Chromium browser panes, built but not started (macOS)
[macos]
browser-build *args:
    bash scripts/browser-dev-bundle {{args}}

# Runs the dev bundle on its own home and core (/tmp/hbd.dev, kept between
# runs; FRESH=1 for a new one), never your real ones, and serves the test
# pages on 127.0.0.1:8765. `/tmp/hbd.dev/herdr-app do …` drives it. A new
# home opens a terminal and the test page. Details: scripts/browser-dev-run.
# Herdr with Chromium browser panes, on its own home (macOS)
[macos]
browser-dev *args:
    bash scripts/browser-dev-run {{args}}

# Rebuilds, then quits the running dev app and starts the new build in the background
[macos]
browser-relaunch *args:
    bash scripts/browser-dev-run --relaunch {{args}}

# Quits the dev app and stops its core
[macos]
browser-stop:
    bash scripts/browser-dev-run --stop

# Serves scripts/ on http://127.0.0.1:8765 for browser panes (they open
# http, https and about:blank only): open
# http://127.0.0.1:8765/browser-fixture.html with --profile agent.
# Serves scripts/ over loopback http for browser panes
browser-fixture port="8765":
    python3 -m http.server {{port}} --bind 127.0.0.1 --directory scripts

[linux]
install:
    bash scripts/install-linux

[macos]
install:
    bash scripts/install-macos

[windows]
install:
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/install-windows.ps1
