# RubberMC (0.1.0)

An experimental Rust-native Minecraft server project exploring compatibility with existing Fabric mods. It currently has a metadata inspector, Anvil-format world storage, and an early Java Edition 26.2 listener that synchronizes registries and initializes an offline client into a minimal one-chunk world. This is a join-path prototype, not a playable server.

## What it does now

- Scans `.jar` files in a mods directory.
- Reads `fabric.mod.json` from the root of each JAR.
- Checks the metadata schema, mod IDs, version strings, and declared environments; malformed files are diagnosed without stopping other scans.
- Lists mod IDs, versions, environments, entrypoint declarations, Mixin config paths, access wideners, and nested-JAR declarations.
- Skips metadata marked `environment: "client"` when `--server` is passed.
- Detects duplicate mod IDs, missing required dependencies, and required dependency version mismatches.
- Reports missing recommendations/suggestions and applicable conflicts/breakages.
- Rejects JARs over 128 MiB and bounds decompressed metadata reads to 1 MiB per JAR.
- Serves Java Edition 26.2 (protocol 776) status and ping, validates offline login starts, negotiates known packs, synchronizes generated registries and tags, and enters the play state.
- Sends a single initial chunk with a small stone platform, a spawn position, and teleport/keepalive handling so the client can initialize a world session.
- Creates Anvil-style world directories with gzip-compressed `level.dat`, reads and saves level metadata, and reads, writes, and removes chunk NBT in `.mca` region files.

## What it does NOT do

This is not a playable Minecraft server or a replacement for Fabric Loader yet. The listener accepts offline logins, but does not authenticate accounts. The one-chunk platform is a protocol smoke-world only: movement, block interaction, terrain generation, chunk streaming, ticking, and game-integrated persistence are not implemented. The Anvil storage API is still separate from the network server; new worlds include 26.2 data-version metadata and overworld/nether/end generator settings, but no terrain is generated. No real client has yet been used to validate the join path. The Anvil storage API writes caller-provided NBT compounds to region files, but does not validate version-specific chunk contents. It does **not** execute mod entrypoints, recursively discover nested JARs, apply access wideners, run Mixins, or implement Fabric APIs. Dependency predicates support Fabric-style exact versions, comparison operators, `~`, `^`, trailing `.x`/`.X` wildcards, conjunctions separated by spaces, and arrays of alternative predicates; the scanner does not claim complete Fabric version compatibility. In particular, Java mods cannot run merely because their metadata can be parsed.

The intended Minecraft target is Java Edition 26.2. A raw-TCP regression test exercises the status, offline login, registry configuration, and initial play packets, but no real-client session or gameplay compatibility has been verified. The included JARs are synthetic fixtures; no real mods have been validated.

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

## Anvil world storage

The `rubber::world::World` API and the CLI can create an Anvil-format directory with Java 26.2 version metadata and dimension generator settings, read and save its `level.dat` NBT metadata, and read, write, or remove chunk NBT in standard `.mca` region files. Chunk coordinates are world chunk coordinates (not block coordinates); the CLI expects each chunk file to contain an uncompressed NBT compound.

```powershell
cargo run -- world create "C:\rubber-worlds\test" "Test World"
cargo run -- world info "C:\rubber-worlds\test"
cargo run -- world write-chunk "C:\rubber-worlds\test" 0 0 chunk.nbt
cargo run -- world read-chunk "C:\rubber-worlds\test" 0 0 saved-chunk.nbt
cargo run -- world remove-chunk "C:\rubber-worlds\test" 0 0
```

`world create` will not overwrite an existing directory. `level.dat` and `data/minecraft/world_gen_settings.dat` are gzip-compressed NBT; saving `level.dat` keeps an older copy as `level.dat_old`. This is a storage foundation only: Rubber does not yet generate terrain, validate chunk NBT against 26.2, or connect persisted chunks to the network server. Client compatibility has not been tested; do not treat a created world as playable.

## Experimental server listener

Start the development endpoint (bound to loopback by default):

```powershell
cargo run -- serve
```

Add `localhost:25565` in a Java Edition 26.2 client's multiplayer server list. The server attempts the configuration exchange and initializes a one-chunk stone-platform world. This join path has not been validated with a real client, and the world is not playable yet: movement and block interactions are ignored, and the chunk is not backed by Anvil storage.

This listener is offline-mode only: it does not contact Mojang or verify account ownership, and usernames/UUIDs are not authenticated. Keep it on loopback; do not expose it to untrusted networks. To bind a different address, pass `--bind`, for example `cargo run -- serve --bind 0.0.0.0:25565`. Binding publicly does not enable authentication.

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

1. Validate the join path with a real Java Edition 26.2 client.
2. Generate terrain chunks and connect Anvil storage to world loading and saving.
3. Add movement, block interactions, ticking, and multiplayer state before calling it playable.
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
