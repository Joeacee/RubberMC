use rubber::{scan_directory, Severity};
use std::{env, path::PathBuf, process::ExitCode};

#[cfg(feature = "java-poc")]
mod java_poc;
mod server;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(directory) = args.next() else {
        print_usage();
        return ExitCode::SUCCESS;
    };

    if directory == "world" {
        return world_command(args.collect());
    }

    if directory == "serve" {
        let mut bind_address = "127.0.0.1:25565".to_string();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--bind" => match args.next() {
                    Some(address) => bind_address = address,
                    None => {
                        eprintln!(
                            "error: --bind requires an address (for example 127.0.0.1:25565)"
                        );
                        return ExitCode::from(2);
                    }
                },
                "--help" | "-h" => {
                    print_server_usage();
                    return ExitCode::SUCCESS;
                }
                _ => {
                    eprintln!("error: unexpected serve argument '{argument}'");
                    print_server_usage();
                    return ExitCode::from(2);
                }
            }
        }
        return match server::serve(&bind_address) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("server error: {error}");
                ExitCode::FAILURE
            }
        };
    }

    if directory == "--java-poc" {
        let Some(class_path) = args.next() else {
            eprintln!("error: --java-poc requires a compiled Java class directory");
            return ExitCode::from(2);
        };
        if args.next().is_some() {
            eprintln!("error: unexpected argument after Java class directory");
            return ExitCode::from(2);
        }
        #[cfg(feature = "java-poc")]
        {
            return match java_poc::run(&PathBuf::from(class_path)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("Java bridge proof of concept failed: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        #[cfg(not(feature = "java-poc"))]
        {
            let _ = class_path;
            eprintln!(
                "error: rebuild Rubber with --features java-poc to run this proof of concept"
            );
            return ExitCode::from(2);
        }
    }

    if directory == "--help" || directory == "-h" {
        print_usage();
        return ExitCode::SUCCESS;
    }

    let mut server_only = false;
    for arg in args {
        match arg.as_str() {
            "--server" => server_only = true,
            "--help" | "-h" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("error: unexpected argument '{arg}'");
                print_usage();
                return ExitCode::from(2);
            }
        }
    }

    let directory = PathBuf::from(directory);
    match scan_directory(&directory, server_only) {
        Ok(report) => {
            println!("Rubber — metadata discovery");
            println!("Directory: {}", directory.display());
            println!(
                "Mode: {}",
                if server_only {
                    "server"
                } else {
                    "all environments"
                }
            );
            println!("JAR files found: {}\n", report.jar_count);

            println!("DISCOVERED MODS ({})", report.mods.len());
            for mod_info in &report.mods {
                let metadata = &mod_info.metadata;
                println!(
                    "• {} {} — {}",
                    metadata.id,
                    metadata.version,
                    metadata.name.as_deref().unwrap_or(&metadata.id)
                );
                println!("  File: {}", mod_info.file_name);
                println!(
                    "  Environment: {}",
                    metadata.environment.as_deref().unwrap_or("* (default)")
                );
                println!(
                    "  Entrypoints declared: {} (not executed)",
                    metadata.entrypoint_count()
                );
                for mixin in metadata.mixins.iter().flatten() {
                    println!(
                        "  Mixin config: {} [environment: {}]",
                        mixin.config(),
                        mixin.environment()
                    );
                }
                if let Some(access_widener) = &metadata.access_widener {
                    println!("  Access widener: {access_widener} (not applied)");
                }
                for nested_jar in metadata.jars.iter().flatten() {
                    println!(
                        "  Nested JAR: {} (recursive discovery not implemented)",
                        nested_jar.file
                    );
                }
                println!();
            }

            println!("\nDEPENDENCY / COMPATIBILITY CHECKS");
            for diagnostic in &report.diagnostics {
                println!(
                    "[{}] {}",
                    match diagnostic.severity {
                        Severity::Error => "ERROR",
                        Severity::Warning => "WARN",
                        Severity::Info => "INFO",
                        Severity::Ok => "OK",
                    },
                    diagnostic.message
                );
            }

            let issue_count = report
                .diagnostics
                .iter()
                .filter(|diagnostic| {
                    matches!(diagnostic.severity, Severity::Error | Severity::Warning)
                })
                .count();
            let errors = report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error);
            println!(
                "\nSUMMARY: {} mod(s), {} reported issue(s).",
                report.mods.len(),
                issue_count
            );
            println!(
                "STATUS: metadata inspection only; use 'rubber serve' for the experimental 26.2 listener."
            );
            if errors {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn world_command(args: Vec<String>) -> ExitCode {
    let invalid_usage = || {
        eprintln!("error: invalid world command");
        print_world_usage();
        ExitCode::from(2)
    };
    match args.as_slice() {
        [action, path, name] if action == "create" => {
            match rubber::world::World::create(path, name) {
                Ok(world) => {
                    println!(
                        "Created Anvil world '{}' at {}",
                        name,
                        world.path().display()
                    );
                    println!("No terrain chunks are generated; this is storage support, not a playable world.");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("world creation failed: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        [action, path] if action == "info" => {
            match rubber::world::World::open(path) {
                Ok(world) => {
                    let name = world
                        .level_data()
                        .get("LevelName")
                        .and_then(fastnbt::Value::as_str)
                        .unwrap_or("(unnamed)");
                    match world.region_file_count() {
                        Ok(regions) => {
                            println!("World: {name}");
                            println!("Directory: {}", world.path().display());
                            println!("Overworld region files: {regions}");
                            println!("Stored chunks are opaque NBT; terrain generation is not implemented.");
                            ExitCode::SUCCESS
                        }
                        Err(error) => {
                            eprintln!("could not inspect world regions: {error}");
                            ExitCode::FAILURE
                        }
                    }
                }
                Err(error) => {
                    eprintln!("could not read world: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        [action, path, x, z, file] if action == "write-chunk" => {
            let Some((chunk_x, chunk_z)) = parse_chunk_coordinates(x, z) else {
                return invalid_usage();
            };
            let world = match rubber::world::World::open(path) {
                Ok(world) => world,
                Err(error) => {
                    eprintln!("could not open world: {error}");
                    return ExitCode::FAILURE;
                }
            };
            let nbt = match std::fs::read(file) {
                Ok(nbt) => nbt,
                Err(error) => {
                    eprintln!("could not read chunk NBT file: {error}");
                    return ExitCode::FAILURE;
                }
            };
            match world.write_chunk(chunk_x, chunk_z, &nbt) {
                Ok(()) => {
                    println!("Saved chunk ({chunk_x}, {chunk_z}) to the Anvil world.");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("could not save chunk: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        [action, path, x, z, file] if action == "read-chunk" => {
            let Some((chunk_x, chunk_z)) = parse_chunk_coordinates(x, z) else {
                return invalid_usage();
            };
            let world = match rubber::world::World::open(path) {
                Ok(world) => world,
                Err(error) => {
                    eprintln!("could not open world: {error}");
                    return ExitCode::FAILURE;
                }
            };
            match world.read_chunk(chunk_x, chunk_z) {
                Ok(Some(nbt)) => match std::fs::write(file, nbt) {
                    Ok(()) => {
                        println!("Wrote chunk ({chunk_x}, {chunk_z}) NBT to {file}.");
                        ExitCode::SUCCESS
                    }
                    Err(error) => {
                        eprintln!("could not write chunk NBT file: {error}");
                        ExitCode::FAILURE
                    }
                },
                Ok(None) => {
                    eprintln!("chunk ({chunk_x}, {chunk_z}) is not present in the world");
                    ExitCode::FAILURE
                }
                Err(error) => {
                    eprintln!("could not read chunk: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        [action, path, x, z] if action == "remove-chunk" => {
            let Some((chunk_x, chunk_z)) = parse_chunk_coordinates(x, z) else {
                return invalid_usage();
            };
            match rubber::world::World::open(path)
                .and_then(|world| world.remove_chunk(chunk_x, chunk_z))
            {
                Ok(()) => {
                    println!("Removed chunk ({chunk_x}, {chunk_z}) if it existed.");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("could not remove chunk: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => invalid_usage(),
    }
}

fn parse_chunk_coordinates(x: &str, z: &str) -> Option<(i32, i32)> {
    Some((x.parse().ok()?, z.parse().ok()?))
}

fn print_usage() {
    println!("Rubber v{}", env!("CARGO_PKG_VERSION"));
    println!("Usage: rubber <mods-directory> [--server]");
    println!("       rubber serve [--bind <address>]");
    println!("       rubber world <create|info|read-chunk|write-chunk|remove-chunk> ...");
    println!(
        "Scans JAR metadata, manages Anvil world storage, or starts the experimental listener."
    );
    println!("Optional JNI proof of concept: rubber --java-poc <compiled-class-directory>");
}

fn print_world_usage() {
    println!("Usage:");
    println!("  rubber world create <world-directory> <name>");
    println!("  rubber world info <world-directory>");
    println!("  rubber world write-chunk <world-directory> <chunk-x> <chunk-z> <chunk.nbt>");
    println!("  rubber world read-chunk <world-directory> <chunk-x> <chunk-z> <output.nbt>");
    println!("  rubber world remove-chunk <world-directory> <chunk-x> <chunk-z>");
    println!("Chunk files must contain an uncompressed NBT compound.");
}

fn print_server_usage() {
    println!("Usage: rubber serve [--bind <address>]");
    println!("Starts the experimental Minecraft 26.2 status and offline-login listener.");
    println!("Default bind address: 127.0.0.1:25565");
    println!("Login succeeds only to the point of returning a configuration-phase disconnect.");
    println!("Registry sync, world entry, authentication, and gameplay are not implemented.");
}
