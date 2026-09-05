//! Generates a Dart SDK from one or more OpenAPI specs and writes every file
//! to disk. Each spec gets its own subdirectory under the shared output root.
//!
//! Usage:
//!   flap --out <out-dir> [--force] [--client=dio|http] [--null-unsafe] <spec> [<spec> ...]
//!
//! Examples:
//!   # Single local file, Dio client (default)
//!   flap --out ./sdks fixtures/petstore.yaml
//!
//!   # http package client
//!   flap --out ./sdks --client=http fixtures/petstore.yaml
//!
//!   # Multiple specs, force regeneration
//!   flap --out ./sdks --force fixtures/petstore.yaml https://petstore3.swagger.io/api/v3/openapi.yaml

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use flap_emit_dart::{ClientBackend, MappingConfig, NullSafety, TemplateConfig};
use sha2::{Digest, Sha256};

const FLAP_VERSION: &str = env!("CARGO_PKG_VERSION");
const LOCK_FILE: &str = ".flap.lock";

const USAGE: &str = "\
flap — OpenAPI → Dart/Flutter client generator

USAGE:
    flap --out <dir> [OPTIONS] <spec> [<spec> ...]

ARGS:
    <spec>                     Path or http(s) URL of an OpenAPI 3.x / Swagger 2.0 document (YAML or JSON)

OPTIONS:
    -o, --out <dir>            Output root. Each spec is written to <dir>/<spec-stem>/
    -f, --force                Regenerate even when the lockfile says nothing changed
        --client=<dio|http>    HTTP backend for the generated client (default: dio)
        --null-unsafe          Additionally emit legacy null-unsafe code to <dir>/<spec-stem>/null_unsafe/
                               (Dart 3 SDKs cannot compile this output)
        --type-map=<Schema=DartType>
                               Replace a spec schema with a hand-written Dart type (repeatable)
        --import-map=<DartType=package:...>
                               Import to use for a mapped Dart type (repeatable)
    -t, --template-dir <dir>   Directory of Jinja2 / verbatim template overrides
    -h, --help                 Print this help
    -V, --version              Print the version
";

struct Args {
    out_dir: PathBuf,
    specs: Vec<String>,
    force: bool,
    backend: ClientBackend,
    null_unsafe: bool,
    mappings: MappingConfig,
    templates: TemplateConfig,
}

enum Parsed {
    Run(Args),
    Help,
    Version,
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

    let args = match parse_args(&args) {
        Ok(Parsed::Run(a)) => a,
        Ok(Parsed::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Version) => {
            println!("flap {FLAP_VERSION}");
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("error: {msg}\n");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    if args.specs.is_empty() {
        eprintln!("error: at least one spec path or URL is required\n");
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }

    println!("client backend : {}", args.backend.as_str());
    for (k, v) in sorted(&args.mappings.type_map) {
        println!("  type-map     : {k} → {v}");
    }
    for (k, v) in sorted(&args.mappings.import_map) {
        println!("  import-map   : {k} → {v}");
    }
    if let Some(dir) = &args.templates.template_dir {
        println!("template dir   : {}", dir.display());
    }
    if args.null_unsafe {
        println!("null-unsafe    : enabled (note: Dart 3 SDKs cannot compile null-unsafe code)");
    }

    let mut modes: Vec<(NullSafety, Option<&str>)> = vec![(NullSafety::Safe, None)];
    if args.null_unsafe {
        modes.push((NullSafety::Unsafe, Some("null_unsafe")));
    }

    let mut any_failed = false;

    for spec in &args.specs {
        println!("\n── {spec} ──");

        let fingerprint = local_fingerprint(spec, args.backend, &args.mappings, &args.templates);
        let spec_root = args.out_dir.join(spec_to_dir_name(spec));

        let mut api: Option<flap_ir::Api> = None;

        for (mode, subdir) in &modes {
            let mode_label = match mode {
                NullSafety::Safe => "null_safe",
                NullSafety::Unsafe => "null_unsafe",
            };
            let spec_out = match subdir {
                Some(sub) => spec_root.join(sub),
                None => spec_root.clone(),
            };
            let lock_path = spec_out.join(format!(
                "{LOCK_FILE}.{mode_label}.{}",
                args.backend.as_str()
            ));

            if !args.force
                && let Some(fp) = &fingerprint
                && read_lock(&lock_path).as_deref() == Some(fp.as_str())
            {
                println!(
                    "  [{mode_label}/{}] unchanged — skipping",
                    args.backend.as_str()
                );
                continue;
            }

            // Load lazily so an up-to-date lockfile never triggers a network fetch.
            if api.is_none() {
                match flap_spec::load_path_or_url(spec) {
                    Ok(loaded) => {
                        for w in &loaded.warnings {
                            eprintln!("  warning: {w}");
                        }
                        api = Some(loaded);
                    }
                    Err(e) => {
                        eprintln!("  error loading spec: {e:#}");
                        any_failed = true;
                        break;
                    }
                }
            }
            let api_ref = api.as_ref().expect("loaded above");

            if let Err(e) = fs::create_dir_all(&spec_out) {
                eprintln!("  error creating {}: {e}", spec_out.display());
                any_failed = true;
                continue;
            }

            let models =
                flap_emit_dart::emit_models(api_ref, *mode, &args.mappings, &args.templates);
            let (client_filename, client_src) = flap_emit_dart::emit_client(
                api_ref,
                *mode,
                args.backend,
                &args.mappings,
                &args.templates,
            );

            let mut write_ok = true;
            let mut filenames: Vec<&String> = models.keys().collect();
            filenames.sort();
            for filename in filenames {
                let path = spec_out.join(filename);
                if let Err(e) = fs::write(&path, &models[filename]) {
                    eprintln!("  error writing {}: {e}", path.display());
                    write_ok = false;
                    continue;
                }
                println!("  wrote {}", path.display());
            }
            let client_path = spec_out.join(&client_filename);
            if let Err(e) = fs::write(&client_path, &client_src) {
                eprintln!("  error writing {}: {e}", client_path.display());
                write_ok = false;
            } else {
                println!("  wrote {}", client_path.display());
            }

            println!(
                "  [{mode_label}/{}] {} model file(s) + 1 client → {}",
                args.backend.as_str(),
                models.len(),
                spec_out.display()
            );

            if write_ok {
                if let Some(fp) = &fingerprint {
                    write_lock(&lock_path, fp);
                }
            } else {
                any_failed = true;
            }
        }
    }

    if any_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn sorted(map: &std::collections::HashMap<String, String>) -> Vec<(&String, &String)> {
    let mut v: Vec<_> = map.iter().collect();
    v.sort();
    v
}

/// Parse the command line. `--help`/`--version` short-circuit.
fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut out_dir: Option<PathBuf> = None;
    let mut specs: Vec<String> = Vec::new();
    let mut force = false;
    let mut backend = ClientBackend::Dio;
    let mut null_unsafe = false;
    let mut mappings = MappingConfig::default();
    let mut templates = TemplateConfig::default();
    let mut i = 0;
    let mut only_positional = false;

    while i < args.len() {
        let arg = args[i].as_str();
        if only_positional {
            specs.push(arg.to_string());
            i += 1;
            continue;
        }
        match arg {
            "--" => only_positional = true,
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "--out" | "-o" => {
                i += 1;
                let dir = args
                    .get(i)
                    .ok_or_else(|| "--out requires a directory argument".to_string())?;
                out_dir = Some(PathBuf::from(dir));
            }
            _ if arg.starts_with("--out=") => {
                out_dir = Some(PathBuf::from(&arg["--out=".len()..]));
            }
            "--force" | "-f" => force = true,
            "--null-unsafe" => null_unsafe = true,
            "--client" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "--client requires `dio` or `http`".to_string())?;
                backend = parse_backend(val)?;
            }
            _ if arg.starts_with("--client=") => {
                backend = parse_backend(&arg["--client=".len()..])?;
            }
            _ if arg.starts_with("--type-map=") => {
                let pair = &arg["--type-map=".len()..];
                let (k, v) = pair
                    .split_once('=')
                    .ok_or_else(|| format!("--type-map requires KEY=VALUE, got `{pair}`"))?;
                mappings.type_map.insert(k.to_string(), v.to_string());
            }
            _ if arg.starts_with("--import-map=") => {
                let pair = &arg["--import-map=".len()..];
                let (k, v) = pair
                    .split_once('=')
                    .ok_or_else(|| format!("--import-map requires KEY=VALUE, got `{pair}`"))?;
                mappings.import_map.insert(k.to_string(), v.to_string());
            }
            "--template-dir" | "-t" => {
                i += 1;
                let dir = args
                    .get(i)
                    .ok_or_else(|| "--template-dir requires a path argument".to_string())?;
                templates.template_dir = Some(PathBuf::from(dir));
            }
            _ if arg.starts_with("--template-dir=") => {
                templates.template_dir = Some(PathBuf::from(&arg["--template-dir=".len()..]));
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                return Err(format!("unknown option `{arg}`"));
            }
            other => specs.push(other.to_string()),
        }
        i += 1;
    }

    let out_dir = out_dir.ok_or_else(|| "--out <dir> is required".to_string())?;
    Ok(Parsed::Run(Args {
        out_dir,
        specs,
        force,
        backend,
        null_unsafe,
        mappings,
        templates,
    }))
}

fn parse_backend(val: &str) -> Result<ClientBackend, String> {
    match val {
        "dio" => Ok(ClientBackend::Dio),
        "http" => Ok(ClientBackend::Http),
        other => Err(format!(
            "unknown client backend `{other}` — expected `dio` or `http`"
        )),
    }
}

/// Fingerprint a local spec: SHA-256 of its content plus everything else
/// that influences the output (flap version, backend, mappings, template
/// files). Returns `None` for remote URLs — those always regenerate.
fn local_fingerprint(
    spec: &str,
    backend: ClientBackend,
    mappings: &MappingConfig,
    templates: &TemplateConfig,
) -> Option<String> {
    if spec.starts_with("http://") || spec.starts_with("https://") {
        return None;
    }
    let content = fs::read(spec).ok()?;

    let mut hasher = Sha256::new();
    hasher.update(FLAP_VERSION.as_bytes());
    hasher.update(b"|backend:");
    hasher.update(backend.as_str().as_bytes());
    hasher.update(b"|spec:");
    hasher.update(&content);

    hasher.update(b"|type-map:");
    for (k, v) in sorted(&mappings.type_map) {
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b",");
    }
    hasher.update(b"|import-map:");
    for (k, v) in sorted(&mappings.import_map) {
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b",");
    }

    hasher.update(b"|templates:");
    if let Some(dir) = &templates.template_dir {
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        if let Ok(rd) = fs::read_dir(dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.is_file()
                    && let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && let Ok(bytes) = fs::read(&path)
                {
                    entries.push((name.to_string(), bytes));
                }
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, bytes) in entries {
            hasher.update(name.as_bytes());
            hasher.update(b":");
            hasher.update(&bytes);
            hasher.update(b";");
        }
    }

    Some(format!("sha256:{:x}", hasher.finalize()))
}

fn read_lock(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn write_lock(path: &Path, fingerprint: &str) {
    if let Err(e) = fs::write(path, fingerprint) {
        eprintln!(
            "  warning: could not write lockfile {}: {e}",
            path.display()
        );
    }
}

/// Convert a spec path or URL into a safe single-directory-component name.
///
/// Examples:
///   "tests/fixtures/petstore.yaml"            → "petstore"
///   "https://example.com/api/v3/openapi.yaml" → "openapi"
fn spec_to_dir_name(spec: &str) -> String {
    let without_suffix = spec
        .split('?')
        .next()
        .unwrap_or(spec)
        .split('#')
        .next()
        .unwrap_or(spec);

    let basename = without_suffix
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(without_suffix);

    let stem = basename
        .strip_suffix(".yaml")
        .or_else(|| basename.strip_suffix(".yml"))
        .or_else(|| basename.strip_suffix(".json"))
        .unwrap_or(basename);

    let sanitised: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    if sanitised.is_empty() || sanitised.chars().all(|c| c == '_') {
        "spec".to_string()
    } else {
        sanitised
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_names() {
        assert_eq!(spec_to_dir_name("tests/fixtures/petstore.yaml"), "petstore");
        assert_eq!(
            spec_to_dir_name("https://example.com/api/v3/openapi.yaml?x=1"),
            "openapi"
        );
        assert_eq!(spec_to_dir_name("C:\\specs\\my api.json"), "my_api");
        assert_eq!(spec_to_dir_name("..."), "spec");
    }

    #[test]
    fn parses_flags() {
        let args: Vec<String> = [
            "--out",
            "o",
            "--client=http",
            "--null-unsafe",
            "-f",
            "a.yaml",
            "b.yaml",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let Parsed::Run(a) = parse_args(&args).unwrap() else {
            panic!()
        };
        assert_eq!(a.out_dir, PathBuf::from("o"));
        assert_eq!(a.backend, ClientBackend::Http);
        assert!(a.null_unsafe && a.force);
        assert_eq!(a.specs, vec!["a.yaml", "b.yaml"]);
        assert!(matches!(
            parse_args(&["--help".to_string()]).unwrap(),
            Parsed::Help
        ));
        assert!(parse_args(&["--bogus".to_string()]).is_err());
        assert!(
            parse_args(&["a.yaml".to_string()]).is_err(),
            "--out is required"
        );
    }
}
