# Rubber (0.1.0)

An experimental Rust-native Minecraft server project exploring compatibility with existing Fabric mods. It currently has a metadata inspector and an early Java Edition 26.2 server-list status/ping listener. Players cannot log in or enter a world yet.

## What it does now

- Scans `.jar` files in a mods directory.
- Reads `fabric.mod.json` from the root of each JAR.
- Checks the metadata schema, mod IDs, version strings, and declared environments; malformed files are diagnosed without stopping other scans.
- Lists mod IDs, versions, environments, entrypoint declarations, Mixin config paths, access wideners, and nested-JAR declarations.
- Skips metadata marked `environment: "client"` when `--server` is passed.
- Detects duplicate mod IDs, missing required dependencies, and required dependency version mismatches.
- Reports missing recommendations/suggestions and applicable conflicts/breakages.
- Rejects JARs over 128 MiB and bounds decompressed metadata reads to 1 MiB per JAR.
- Serves the Java Edition 26.2 (protocol 776) server-list status and ping exchange on a local TCP socket.

## What it does NOT do

This is not a playable Minecraft server or a replacement for Fabric Loader yet. The listener supports only server-list status and ping; login, authentication, configuration/registry synchronization, world/chunk delivery, movement, block interaction, ticking, and persistence are not implemented. It does **not** execute mod entrypoints, recursively discover nested JARs, apply access wideners, run Mixins, or implement Fabric APIs. Dependency predicates support Fabric-style exact versions, comparison operators, `~`, `^`, trailing `.x`/`.X` wildcards, conjunctions separated by spaces, and arrays of alternative predicates; the scanner does not claim complete Fabric version compatibility. In particular, Java mods cannot run merely because their metadata can be parsed.

The intended Minecraft target is Java Edition 26.2. Protocol support is currently limited to server-list status and ping; no client login or gameplay compatibility has been verified. The included JARs are synthetic fixtures; no real mods have been validated.

## Build

Requires Rust and Cargo.

```sh
cargo build --release
cargo test
cargo fmt --check
```

## Run

```sh
cargo run --release -- ./examples/mods --server
```

On Windows PowerShell, for example:

```powershell
cargo run --release -- "C:\path\to\server\mods" --server
```

The directory should contain the `.jar` files you want to inspect. This command only reads metadata; it does not modify your mods or world.

## Experimental server listener

Start the status-only endpoint (bound to loopback by default):

```powershell
cargo run -- serve
```

Then add `localhost:25565` in a Java Edition 26.2 client's multiplayer server list. The server can answer the list ping, but selecting it will not join a game yet. To bind a different address, pass `--bind`, for example `cargo run -- serve --bind 0.0.0.0:25565`; exposing this development listener does not enable login or gameplay.

## Optional JNI proof of concept

The `java-poc` Cargo feature starts a JVM using the installed JDK, loads only the project-owned test class, calls a Java method from Rust, calls a Rust native method from Java, handles a deliberate Java exception, and shuts the JVM down. This is an interoperability experiment only; it does not load Fabric Loader, Minecraft, or third-party mod code.

Requires a JDK with `java` and `javac` on `PATH` (or discoverable by the JNI crate):

```powershell
cargo check --features java-poc
javac -d target\java-poc examples\jvm-poc\RubberBridge.java
cargo run --features java-poc -- --java-poc target\java-poc
```

If JVM discovery fails or selects an unexpected installation on Windows, set `$env:JAVA_HOME` to the intended JDK before running Rubber and make sure its `bin` directory is on `PATH`. No system-wide settings are changed by Rubber. This embedded-JVM experiment keeps Rust and Java in one process (so Java faults are not isolated); classloader isolation, Fabric lifecycle integration, and failure containment remain unimplemented.

## Suggested next milestones

1. Implement the 26.2 login and configuration states, including the required registry exchange.
2. Send a minimal valid dimension and chunk so a client can enter the world.
3. Add movement, block interactions, ticking, and persistence before calling it playable.
4. Extend the JVM experiment to evaluate classloader isolation and JVM lifecycle on supported platforms.
5. Investigate Fabric Loader and Mixin requirements before loading real mods.
6. Expand metadata checks against real Fabric mods and inspect nested JARs safely.

## Technical references

- Fabric Loader source: https://github.com/FabricMC/fabric-loader (including `VersionPredicateParser` and `VersionComparisonOperator`)
- Fabric Loader overview: https://docs.fabricmc.net/develop/loader/
- Minecraft Java protocol version numbers: https://github.com/PrismarineJS/minecraft-data/blob/master/data/pc/common/protocolVersions.json
- Pumpkin Rust server: https://github.com/Pumpkin-MC/Pumpkin

## Included fixtures

`examples/mods/` contains three tiny **synthetic metadata-only JARs** for exercising discovery and reporting. They are not actual mods and contain no Java classes; they are safe test fixtures. After building, run:

```sh
cargo run --release -- ./examples/mods --server
```

Expected checks include discovering `demo-core` and `demo-addon`, verifying `demo-addon`'s `>=1.0.0` dependency on `demo-core`, reporting its recommended `fabric-api`, and skipping `demo-client-only` in server mode. The executable is named `rubber` (`rubber.exe` on Windows).
