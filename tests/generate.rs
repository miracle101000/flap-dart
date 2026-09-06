//! End-to-end tests: run the lowering + emitter over the fixture specs and
//! check structural properties of the generated Dart, plus CLI behaviour.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use flap_emit_dart::{ClientBackend, MappingConfig, NullSafety, TemplateConfig};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn fixture_paths() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("yaml" | "yml" | "json")
            )
        })
        .collect();
    v.sort();
    v
}

fn generate(spec: &Path, backend: ClientBackend) -> (HashMap<String, String>, String, String) {
    let api = flap_spec::load(spec).unwrap_or_else(|e| panic!("{}: {e:#}", spec.display()));
    let mappings = MappingConfig::default();
    let templates = TemplateConfig::default();
    let models = flap_emit_dart::emit_models(&api, NullSafety::Safe, &mappings, &templates);
    let (client_file, client_src) =
        flap_emit_dart::emit_client(&api, NullSafety::Safe, backend, &mappings, &templates);
    (models, client_file, client_src)
}

/// Every `part 'x.g.dart'` / `part 'x.freezed.dart'` must sit in `x.dart`,
/// and every relative import must resolve to a generated sibling file.
fn check_file_consistency(files: &HashMap<String, String>) {
    for (name, src) in files {
        let stem = name.trim_end_matches(".dart");
        for line in src.lines() {
            if let Some(part) = line.strip_prefix("part '") {
                let part = part.trim_end_matches("';");
                assert!(
                    part == format!("{stem}.freezed.dart") || part == format!("{stem}.g.dart"),
                    "{name}: unexpected part `{part}`"
                );
            }
            if let Some(import) = line.strip_prefix("import '")
                && !import.starts_with("package:")
                && !import.starts_with("dart:")
            {
                let target = import.trim_end_matches("';");
                assert!(
                    files.contains_key(target),
                    "{name} imports `{target}` which was not generated"
                );
            }
        }
    }
}

/// Patterns that were bugs in earlier releases and must never reappear.
fn check_forbidden_patterns(name: &str, src: &str) {
    let forbidden = [
        (
            "@JsonEnum(unknownValue",
            "JsonEnum has no unknownValue parameter",
        ),
        (
            "': ${",
            "query/header map entries must not be bare `${...}` expressions",
        ),
        (
            "package:flutter/",
            "generated code must not depend on Flutter",
        ),
        ("@_", "converters referenced across files must be public"),
        ("'/${", "path params must be URI-encoded"),
    ];
    for (pat, why) in forbidden {
        assert!(!src.contains(pat), "{name}: contains `{pat}` — {why}");
    }
    // Every non-`part` model file must import freezed_annotation if it uses @freezed.
    if src.contains("@freezed") || src.contains("@Freezed(") {
        assert!(
            src.contains("import 'package:freezed_annotation/freezed_annotation.dart';"),
            "{name}: missing freezed_annotation import"
        );
        assert!(
            src.contains("abstract class ") || src.contains("sealed class "),
            "{name}: freezed 3 requires abstract/sealed classes"
        );
    }
}

#[test]
fn all_fixtures_generate_for_both_backends() {
    for spec in fixture_paths() {
        for backend in [ClientBackend::Dio, ClientBackend::Http] {
            let (mut files, client_file, client_src) = generate(&spec, backend);
            assert!(
                client_file.ends_with("_client.dart"),
                "{}: client file `{client_file}`",
                spec.display()
            );
            files.insert(client_file.clone(), client_src);
            check_file_consistency(&files);
            for (name, src) in &files {
                check_forbidden_patterns(name, src);
                assert!(!src.trim().is_empty(), "{name} is empty");
            }
            // Filenames are valid Dart library names.
            for name in files.keys() {
                assert!(
                    name.chars().all(|c| c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || c == '_'
                        || c == '.'),
                    "{}: bad file name `{name}`",
                    spec.display()
                );
            }
        }
    }
}

#[test]
fn kitchen_sink_specifics() {
    let spec = fixtures_dir().join("kitchen_sink.yaml");
    let (files, _, dio) = generate(&spec, ClientBackend::Dio);
    let (_, _, http) = generate(&spec, ClientBackend::Http);

    // Title → client class; dotted schema name → PascalCase class.
    assert!(dio.contains("class KitchenSinkClient {"));
    assert!(files.contains_key("com_example_legacy.dart"));

    // Reserved words are escaped and mapped back to their JSON names.
    let shape = &files["shape.dart"];
    assert!(shape.contains("@JsonKey(name: 'class', includeIfNull: false) String? classValue"));
    assert!(shape.contains("@FreezedUnionValue('sq')"));
    assert!(shape.contains("sealed class Shape with _$Shape"));
    // The discriminator property is not duplicated as a variant field.
    assert!(!shape.contains("required String type,"));

    // Root enum with keyword values and a case-insensitive duplicate.
    let kind = &files["shape_kind.dart"];
    assert!(kind.contains("newValue('new')"));
    assert!(kind.contains("defaultValue('default')"));
    assert!(kind.contains("circle2('Circle')"));
    assert!(kind.contains("unknown(null)"));

    // Optional wrapper for nullable + optional scalars, hand-written toJson.
    let patch = &files["shape_patch.dart"];
    assert!(patch.contains("@Freezed(toJson: false)"));
    assert!(patch.contains(
        "@OptionalIntConverter() @Default(Optional<int?>.absent()) Optional<int?> priority"
    ));
    assert!(patch.contains("if (priority.isPresent) 'priority': priority.value,"));
    // nullable + allOf: [$ref] is a nullable reference, not an empty class.
    assert!(patch.contains("Shape? parent"), "{patch}");
    assert!(!files.contains_key("shape_patch_parent.dart"));

    // Inline enums inside untagged-union variants are plain strings.
    let hit = &files["search_hit.dart"];
    assert!(hit.contains("SearchHit.listValue(List<String> value)"));
    assert!(!hit.contains("(e) => e.toJson()"), "{hit}");
    // Typedef files import what they reference.
    assert!(files["prices.dart"].contains("import 'money.dart';"));
    assert!(files["id_list.dart"].contains("typedef IdList = List<int>;"));

    // Path parameters are URI-encoded; path-level params are merged in.
    assert!(dio.contains("'/shapes/${Uri.encodeComponent(shapeId)}'"));
    assert!(dio.contains("String? xTraceId,"));
    // Enum query params send their wire value; dates are ISO 8601.
    assert!(dio.contains("'kind': '${kind.value}'"));
    assert!(dio.contains("'since': since.toIso8601String()"));
    // Cookie params and cookie API keys are sent as a Cookie header.
    assert!(dio.contains("'Cookie': cookies.join('; ')"));
    assert!(dio.contains("options.headers['Cookie'] ="));
    // Response headers become a typed record.
    assert!(
        dio.contains(
            "Future<({List<Shape> body, String? xNextCursor, int xTotalCount})> listShapes"
        )
    );
    // Deprecated operations are annotated.
    assert!(dio.contains("@Deprecated("));
    // Multipart uploads wrap binary fields.
    assert!(dio.contains("MultipartFile.fromBytes(body.file, filename: 'file')"));
    assert!(http.contains("http.MultipartFile.fromBytes('file', body.file, filename: 'file')"));
    // Form bodies and binary downloads.
    assert!(dio.contains("contentType: Headers.formUrlEncodedContentType"));
    assert!(dio.contains("responseType: ResponseType.bytes"));
    assert!(http.contains("return response.bodyBytes;"));
    // Nested objects must serialise to maps (form encoders rely on it).
    assert!(files["square.dart"].contains("@JsonSerializable(explicitToJson: true)"));
    // Object-valued query params and form bodies use deepObject bracket keys.
    assert!(dio.contains("if (created != null) 'created': created.toJson(),"));
    assert!(http.contains("if (created != null) ..._deepObject('created', created.toJson()),"));
    assert!(http.contains("request.bodyFields = _formFields(body.toJson() as Map);"));
    // Array styles: form/explode=false joins, deepObject indexes, default repeats.
    assert!(dio.contains("'ids': ids.map((e) => e.toString()).toList().join(','),"));
    assert!(dio.contains("..._deepObject('expand', expand),"));
    assert!(dio.contains("'tags': tags,"));
    assert!(dio.contains("static Map<String, String> _deepObject("));
    assert!(http.contains("static Map<String, String> _deepObject("));
    // Basic auth is base64-encoded; the http client throws a typed exception.
    assert!(http.contains("base64Encode(utf8.encode(_basicAuth))"));
    assert!(http.contains("class KitchenSinkClientException implements Exception"));
}

#[test]
fn swagger_two_is_translated() {
    let spec = fixtures_dir().join("swagger_petstore_2.yaml");
    let (files, _, http) = generate(&spec, ClientBackend::Http);
    assert!(http.contains("String baseUrl = 'https://petstore.example.com/v2'"));
    // formData → request body class; file → List<int>.
    let upload = &files["upload_image_body.dart"];
    assert!(upload.contains("required List<int> file"));
    // x-nullable → Optional wrapper.
    assert!(files["pet.dart"].contains("Optional<String?> nickname"));
    // inline object property → synthesised class
    assert!(files.contains_key("pet_attributes.dart"));
    // basic auth definition is honoured
    assert!(http.contains("base64Encode(utf8.encode(_basic))"));
    // collectionFormat: multi → exploded form (repeated keys).
    assert!(
        http.contains("'status': status.map((e) => '${e.value}').toList(),"),
        "{http}"
    );
}

#[test]
fn type_map_replaces_schema_and_import() {
    let spec = fixtures_dir().join("petstore.yaml");
    let api = flap_spec::load(&spec).unwrap();
    let mut mappings = MappingConfig::default();
    mappings.type_map.insert("Pet".into(), "MyPet".into());
    mappings
        .import_map
        .insert("MyPet".into(), "package:myapp/my_pet.dart".into());
    let files = flap_emit_dart::emit_models(
        &api,
        NullSafety::Safe,
        &mappings,
        &TemplateConfig::default(),
    );
    assert!(!files.contains_key("pet.dart"));
    assert!(files["pets.dart"].contains("import 'package:myapp/my_pet.dart';"));
    assert!(files["pets.dart"].contains("typedef Pets = List<MyPet>;"));
    let (_, client) = flap_emit_dart::emit_client(
        &api,
        NullSafety::Safe,
        ClientBackend::Dio,
        &mappings,
        &TemplateConfig::default(),
    );
    assert!(client.contains("import 'package:myapp/my_pet.dart';"));
    assert!(client.contains("MyPet.fromJson("));
}

#[test]
fn unsafe_mode_is_self_consistent() {
    let spec = fixtures_dir().join("kitchen_sink.yaml");
    let api = flap_spec::load(&spec).unwrap();
    let files = flap_emit_dart::emit_models(
        &api,
        NullSafety::Unsafe,
        &MappingConfig::default(),
        &TemplateConfig::default(),
    );
    let (_, client) = flap_emit_dart::emit_client(
        &api,
        NullSafety::Unsafe,
        ClientBackend::Http,
        &MappingConfig::default(),
        &TemplateConfig::default(),
    );
    assert!(!files.contains_key("flap_utils.dart"));
    for (name, src) in files
        .iter()
        .chain(std::iter::once((&"client".to_string(), &client)))
    {
        assert!(
            !src.contains(" required "),
            "{name}: `required` keyword in unsafe mode"
        );
        assert!(!src.contains(" late "), "{name}: `late` in unsafe mode");
        assert!(
            !src.contains("Optional<"),
            "{name}: Optional wrapper in unsafe mode"
        );
        for nullable in [
            "String? ",
            "int? ",
            "double? ",
            "num? ",
            "bool? ",
            "DateTime? ",
            ">? ",
            "Options? ",
            "Adapter? ",
            "Token? ",
            "Client? ",
            "dynamic? ",
        ] {
            assert!(
                !src.contains(nullable),
                "{name}: nullable type `{nullable}` in unsafe mode"
            );
        }
    }
}

// ── CLI ───────────────────────────────────────────────────────────────────────

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_generate_dart"))
}

#[test]
fn cli_help_and_version() {
    let out = cli().arg("--help").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("USAGE:"));
    let out = cli().arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("flap "));
    let out = cli().arg("--bogus").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn cli_layout_lockfile_and_null_unsafe() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = fixtures_dir().join("petstore.yaml");
    let run = |extra: &[&str]| {
        let mut c = cli();
        c.arg("--out").arg(tmp.path()).args(extra).arg(&spec);
        let out = c.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    let first = run(&[]);
    assert!(first.contains("wrote"));
    let root = tmp.path().join("petstore");
    assert!(root.join("pet.dart").exists());
    assert!(root.join("swagger_petstore_client.dart").exists());
    assert!(root.join("flap_utils.dart").exists());
    assert!(root.join(".flap.lock.null_safe.dio").exists());
    assert!(!root.join("null_unsafe").exists());

    // Second run is skipped via the lockfile; --force regenerates.
    let second = run(&[]);
    assert!(second.contains("unchanged — skipping"));
    let forced = run(&["--force"]);
    assert!(forced.contains("wrote"));

    // Switching backends invalidates the lock and writes a separate lockfile.
    let http = run(&["--client=http"]);
    assert!(http.contains("wrote"));
    assert!(root.join(".flap.lock.null_safe.http").exists());

    // --null-unsafe writes to its own directory and never clobbers null-safe output.
    let safe_pet = std::fs::read_to_string(root.join("pet.dart")).unwrap();
    run(&["--null-unsafe", "--force"]);
    assert!(root.join("null_unsafe").join("pet.dart").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("pet.dart")).unwrap(),
        safe_pet
    );
    assert!(
        std::fs::read_to_string(root.join("null_unsafe").join("pet.dart"))
            .unwrap()
            .contains("@required")
    );
}

#[test]
fn cli_reports_spec_errors_without_panicking() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join("bad.yaml");
    std::fs::write(
        &bad,
        "openapi: 3.0.0\ninfo: {title: Bad}\npaths: {}\ncomponents:\n  schemas:\n    A:\n      type: object\n      properties:\n        b: {$ref: '#/components/schemas/Missing'}\n",
    )
    .unwrap();
    let out = cli()
        .arg("--out")
        .arg(tmp.path())
        .arg(&bad)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Missing"), "{err}");
}
