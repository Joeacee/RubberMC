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
                "STATUS: metadata prototype only; no Java execution, Fabric API bridge, Mixin transformation, or Minecraft server."
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

fn print_usage() {
    println!("Rubber v{}", env!("CARGO_PKG_VERSION"));
    println!("Usage: rubber <mods-directory> [--server]");
    println!("       rubber serve [--bind <address>]");
    println!("Scans JAR metadata only; it does not execute Java mod code.");
    println!("Optional JNI proof of concept: rubber --java-poc <compiled-class-directory>");
}

fn print_server_usage() {
    println!("Usage: rubber serve [--bind <address>]");
    println!("Starts the experimental Minecraft 26.2 status/ping listener.");
    println!("Default bind address: 127.0.0.1:25565");
    println!("Status/ping only; login and gameplay are not implemented.");
}
