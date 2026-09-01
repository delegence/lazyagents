# Agent Guidelines

study README.md to learn about project.
study docs/* for product contract, specifications and etc.

## How to code
- Respect KISS, YAGNI, SOLID principles.
- Rust 2021 edition; prefer standard library + well-known crates.
- Run `cargo fmt` on all changed Rust files.
- When making any changes proceed with the simplest change possible. DO NOT care about migration and backward compatibility.
- Code readability matters most, and we're happy to make bigger changes to achieve it.
- Favor clear, user-facing error messages for CLI output.
