# flap

**Fast, zero-dependency OpenAPI → Dart/Flutter client generator.**

`flap` lowers OpenAPI 3.0, 3.1, and Swagger 2.0 specs into idiomatic, production-ready Dart/Flutter clients using [`package:freezed`](https://pub.dev/packages/freezed) and `json_serializable` — no Java, no Node.js, no Docker required.

---

## Features

- **Spec support** — OpenAPI 3.0, OpenAPI 3.1, and Swagger 2.0 (YAML or JSON, local file or remote URL)
- **Two HTTP backends** — `package:dio` (default, full-featured) or `package:http` (lightweight)
- **Sound null safety** — generates both `null_safe/` and `null_unsafe/` output side-by-side
- **Full schema coverage** — objects, arrays, maps, enums, discriminated unions (`oneOf` + discriminator), untagged unions (`anyOf`/`oneOf`), `allOf` inheritance, recursive types
- **PATCH tri-state semantics** — `Optional<T?>` wrapper for `nullable: true` + optional fields, distinguishing "key absent" from "key explicitly null"
- **Security schemes** — `apiKey` (header, query, cookie), HTTP Bearer, HTTP Basic, OAuth 2.0, OpenID Connect; credentials injected via Dio interceptors or `http` headers automatically
- **Response headers** — typed Dart 3 named-record return types for operations with declared response headers
- **`default:` values** — emitted as Freezed `@Default(...)` annotations
- **`date-time` format** → `DateTime`; `float`/`double` format → `double`
- **Multipart uploads** — `multipart/form-data` request bodies via `FormData` / `MultipartRequest`
- **Multiple servers** — `servers:` array emitted as a typed `abstract final class FooClientUrls`
- **Incremental builds** — lockfile per spec × mode × backend; unchanged specs are skipped
- **Template overrides** — swap any generated file with a Jinja2 template or a verbatim file
- **Type/import mapping** — replace generated schema types with your own hand-written classes

---

## Installation

```sh
dart pub global activate flap
```

On first run, `flap` downloads the pre-built Rust binary for your platform and caches it at `~/.flap/bin/<version>/`. Subsequent runs are instant.

### Supported platforms

| Platform | Architecture | Asset |
|---|---|---|
| Linux | x86_64 | `flap-linux-x64.tar.gz` |
| macOS | x86_64 | `flap-macos-x64.tar.gz` |
| macOS | arm64 (Apple Silicon) | `flap-macos-arm64.tar.gz` |
| Windows | x86_64 | `flap-windows-x64.zip` |

### Build from source

Requires the [Rust toolchain](https://rustup.rs) (stable).

```sh
git clone https://github.com/your-org/flap
cd flap
cargo build --release --bin generate_dart
```

---

## Quick start

```sh
# Generate a Dio client (default)
flap --out ./generated path/to/openapi.yaml

# Generate an http-package client
flap --out ./generated --client=http path/to/openapi.yaml

# Generate from a remote URL
flap --out ./generated https://petstore3.swagger.io/api/v3/openapi.yaml

# Multiple specs in one pass
flap --out ./generated specs/users.yaml specs/payments.yaml

# Force regeneration even if the spec has not changed
flap --out ./generated --force path/to/openapi.yaml
```

Each spec gets its own subdirectory under `--out`, named after the spec file stem (e.g. `openapi.yaml` → `generated/openapi/`). Inside it you will find one Dart file per schema, one client file, `flap_utils.dart` (null-safe mode only), and `.flap.lock.*` incremental build markers.

---

## Output layout

```
generated/
└── petstore/
    ├── pet.dart              # @freezed model
    ├── pets.dart             # typedef List<Pet>
    ├── error.dart            # @freezed model
    ├── swagger_petstore_client.dart   # API client
    ├── flap_utils.dart       # Optional<T?> runtime (null-safe only)
    ├── .flap.lock.null_safe.dio
    └── .flap.lock.null_unsafe.dio
```

---

## Generated client

### Dio (default)

```dart
final client = SwaggerPetstoreClient(
  baseUrl: SwaggerPetstoreClientUrls.server0, // when multiple servers declared
  bearerAuth: myJwtToken,                     // or apiKey: ..., appId: ...
);

// Typed return — body deserialized automatically
final pet = await client.showPetById(petId: '42');

// Response headers included in a named record when declared in the spec
final (:body, :xRateLimitRemaining) = await client.listPets(limit: 10);
```

### http

```dart
final client = SwaggerPetstoreClient(
  bearerAuth: myJwtToken,
  client: myCustomHttpClient, // optional; defaults to http.Client()
);
```

---

## PATCH / tri-state fields

For `nullable: true` + optional fields (the "PATCH cell" — absent vs. explicitly null), `flap` emits `Optional<T?>`:

```dart
await client.updateUser(
  id: '42',
  body: UpdateUserRequest(
    id: 'required-field',
    bio: null,                         // required + nullable → String?
    nickname: Optional.present(null),  // optional + nullable → set to null
    displayName: Optional.absent(),    // optional + nullable → omit key entirely
  ),
);
```

The `stripOptionalAbsent` helper in `flap_utils.dart` removes absent keys from the JSON map before serialization.

---

## Schema mapping

### Type mapping

Replace a generated schema with your own class:

```sh
flap --out ./generated \
  --type-map=Pet=MyAppPet \
  --import-map=MyAppPet=package:myapp/models/pet.dart \
  path/to/petstore.yaml
```

The generated client will import and use `MyAppPet` wherever `Pet` would have appeared. The `Pet` model file is not emitted.

### Template overrides

Customise any generated file via Jinja2 or verbatim replacement:

```sh
flap --out ./generated --template-dir=./templates path/to/openapi.yaml
```

Resolution order per output file:
1. `{template-dir}/{exact-filename}` — verbatim copy, highest priority
2. `{template-dir}/model.dart.jinja` — Jinja2 template applied to every model
3. `{template-dir}/client.dart.jinja` — Jinja2 template applied to the client
4. `{template-dir}/flap_utils.dart` — verbatim override for the runtime file
5. Built-in emitter — fallback

The Jinja2 context exposes `class_name`, `schema_name`, `fields` (with `dart_name`, `dart_type`, `required`, `nullable`, `uses_optional_wrapper`, `json_name`, `default_expr`), `imports`, `extends`, `null_safety`, and more.

---

## Programmatic use

```dart
import 'package:flap/flap.dart';

Future<void> main() async {
  final exitCode = await FlapRunner().run([
    '--out', './generated',
    '--client=http',
    'api/openapi.yaml',
  ]);
  if (exitCode != 0) throw Exception('flap failed with exit code $exitCode');
}
```

---

## Project structure

```
flap/                          # Dart package (pub.dev distribution)
├── bin/flap.dart              # CLI entry point
├── lib/
│   ├── flap.dart              # Public API export
│   └── src/
│       ├── runner.dart        # FlapRunner — locates and invokes the binary
│       ├── binary_manager.dart # Downloads and caches the platform binary
│       └── platform_info.dart  # Platform slug / archive extension helpers
│
crates/                        # Rust workspace
├── flap-ir/                   # Intermediate representation (language-agnostic)
│   └── src/lib.rs             # Api, Schema, Operation, TypeRef, SecurityScheme …
├── flap-spec/                 # OpenAPI / Swagger loader and lowering pass
│   └── src/
│       ├── lib.rs             # OpenAPI 3.x parser → IR
│       └── swagger.rs         # Swagger 2.0 parser → IR
└── flap-emit-dart/            # Dart code emitter
    └── src/lib.rs             # emit_models(), emit_client(), Jinja2 support
│
src/
└── bin/generate_dart.rs       # Rust CLI binary (the thing Dart shells out to)
│
fixtures/                      # OpenAPI spec fixtures used in tests
```

### Crate responsibilities

| Crate | Role |
|---|---|
| `flap-ir` | Pure data types. No YAML, no Dart. The contract between loader and emitter. |
| `flap-spec` | Parses raw YAML/JSON, validates `$ref` integrity, operationId uniqueness, security scheme references, then lowers to IR. |
| `flap-emit-dart` | Consumes IR; emits `@freezed` models, client files, and `flap_utils.dart`. Supports Jinja2 template overrides via `minijinja`. |

---

## CLI reference

```
flap --out <dir> [options] <spec> [<spec> ...]

Arguments:
  <spec>                  Path to a local .yaml/.yml/.json file, or an https:// URL.
                          Multiple specs may be provided; each is written to its own subdirectory.

Options:
  --out, -o <dir>         Output directory (required).
  --client=<backend>      HTTP client backend: dio (default) or http.
  --force, -f             Regenerate even if the spec has not changed.
  --type-map=A=B          Replace schema A with Dart type B in all generated files.
                          May be repeated.
  --import-map=B=pkg:...  Import URI for a type introduced by --type-map.
                          May be repeated.
  --template-dir, -t <d>  Directory of Jinja2 / verbatim template overrides.
```

---

## Dependencies

The generated Dart code requires these packages in the consuming project:

```yaml
# pubspec.yaml
dependencies:
  dio: ^5.0.0              # if using --client=dio (default)
  http: ^1.0.0             # if using --client=http
  freezed_annotation: ^2.0.0
  json_annotation: ^4.0.0

dev_dependencies:
  build_runner: ^2.0.0
  freezed: ^2.0.0
  json_serializable: ^6.0.0
```

After generation, run the build runner once:

```sh
dart run build_runner build --delete-conflicting-outputs
```

---

## CI / release workflow

The GitHub Actions workflow (`.github/workflows/ci.yml`) runs on every push and pull request:

- **Rust** — `cargo fmt`, `cargo clippy`, `cargo test` on Ubuntu, macOS, and Windows
- **Dart** — `dart analyze`, `dart pub publish --dry-run`

The release workflow (`.github/workflows/release.yml`) triggers on `v*` tags:
1. Builds the Rust binary for all four targets in parallel
2. Packages each into a `.tar.gz` (Unix) or `.zip` (Windows) with a SHA-256 sidecar
3. Creates a GitHub Release and attaches all archives
4. Publishes the Dart package to pub.dev via OIDC

---

## License

MIT — see [LICENSE](LICENSE).