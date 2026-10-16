# RustFabric Prototype (0.1.0)

An early Rust prototype for a future Rust-native Minecraft server with compatibility for existing Fabric mods.

## What it does now

- Scans `.jar` files in a mods directory.
- Reads `fabric.mod.json` from the root of each JAR.
- Checks schema version and reports malformed or non-Fabric JARs.
- Lists mod IDs, versions, environments, entrypoint declarations, Mixin config paths, access wideners, and nested-JAR declarations.
- Skips metadata marked `environment: "client"` when `--server` is passed.
- Detects duplicate mod IDs, missing required dependencies, present dependency IDs, recommendations, and declared conflicts/breakages.

## What it does NOT do

This is not a working Minecraft server or a replacement for Fabric Loader yet. It does **not** execute mod entrypoints, evaluate semantic version ranges, recursively discover nested JARs, apply access wideners, run Mixins, implement Fabric APIs, or launch Minecraft. Those are later milestones. In particular, Java mods cannot run merely because their metadata can be parsed.

## Build

Requires Rust and Cargo.

```sh
cargo build --release
```

## Run

```sh
cargo run --release -- ./mods --server
```

On Windows PowerShell, for example:

```powershell
cargo run --release -- "C:\path\to\server\mods" --server
```

The directory should contain the `.jar` files you want to inspect. This command only reads metadata; it does not modify your mods or world.

## Suggested next milestones

1. Add tests using synthetic JARs and validate metadata edge cases.
2. Parse and evaluate Fabric version predicates correctly.
3. Recursively inspect declared nested JARs.
4. Implement mod resolution/order and a detailed compatibility report.
5. Prototype JVM integration and Mixin support separately before claiming any existing mods run.
6. Build the Rust Minecraft server runtime and define the Java-to-Rust boundary.

## Technical references

- Fabric Loader metadata: https://docs.fabricmc.net/develop/loader/fabric-mod-json
- Fabric Loader overview: https://docs.fabricmc.net/develop/loader/
- Pumpkin Rust server: https://github.com/Pumpkin-MC/Pumpkin

## Included fixtures

`examples/mods/` contains three tiny **synthetic metadata-only JARs** for exercising discovery and reporting. They are not actual mods and contain no Java classes; they are safe test fixtures. After building, run:

```sh
cargo run --release -- ./examples/mods --server
```

Expected checks include discovering `demo-core` and `demo-addon`, finding `demo-addon`'s dependency on `demo-core`, reporting its recommended `fabric-api`, and skipping `demo-client-only` in server mode.
