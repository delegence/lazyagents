SHELL := /bin/sh

.PHONY: help test release-input-check release-infra-check release-version-check bump-version release

help:
	@echo "Targets:"
	@echo "  make test                 Run the test suite"
	@echo "  make release VERSION=x.y.z"
	@echo "                            Bump version, test, commit Cargo version files, tag, and push"

test:
	cargo test

release-input-check:
	@test -n "$(VERSION)" || { echo "Usage: make release VERSION=x.y.z"; exit 1; }
	@case "$(VERSION)" in v*) echo "Use VERSION=$(VERSION), without the leading v"; exit 1 ;; esac

release-infra-check: release-input-check
	@test -f .github/workflows/release.yml || { echo "Missing .github/workflows/release.yml"; exit 1; }
	@git diff --quiet -- .github/workflows/release.yml || { echo "Commit .github/workflows/release.yml before releasing"; exit 1; }
	@git diff --cached --quiet -- .github/workflows/release.yml || { echo "Commit .github/workflows/release.yml before releasing"; exit 1; }
	@git ls-files --error-unmatch .github/workflows/release.yml >/dev/null 2>&1 || { echo "Commit .github/workflows/release.yml before releasing"; exit 1; }

release-version-check: release-input-check
	@cargo_version="$$(cargo metadata --no-deps --format-version 1 | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')"; \
	if [ "$$cargo_version" != "$(VERSION)" ]; then \
		echo "Cargo.toml version is $$cargo_version, expected $(VERSION)"; \
		exit 1; \
	fi
	@git rev-parse --verify "v$(VERSION)" >/dev/null 2>&1 && { echo "Tag v$(VERSION) already exists"; exit 1; } || true

bump-version: release-input-check
	perl -0pi -e 's/^version = "[^"]+"/version = "$(VERSION)"/m' Cargo.toml

release: release-infra-check bump-version release-version-check test
	git add Cargo.toml Cargo.lock
	git diff --quiet HEAD -- Cargo.toml Cargo.lock || git commit -m "Release v$(VERSION)" -- Cargo.toml Cargo.lock
	git tag -a "v$(VERSION)" -m "v$(VERSION)"
	git push origin HEAD
	git push origin "v$(VERSION)"
