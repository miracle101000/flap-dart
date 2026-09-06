# Changelog

All notable changes to this project will be documented in this file.

## 0.0.6

Validated against the full Stripe OpenAPI document (419 paths, 594
operations, 1,454 schemas → ~11,000 generated files): `build_runner` and
`dart analyze` are clean for both backends.

### Fixed

* Nested objects are now serialised to maps (`@JsonSerializable(explicitToJson: true)`
  on every generated factory). Previously form-encoded and multipart bodies with
  nested objects were sent as `Instance of …`.
* OpenAPI parameter serialization is honoured: `style` (`form`, `deepObject`,
  `spaceDelimited`, `pipeDelimited`) and `explode` for query parameters, and
  Swagger 2.0 `collectionFormat`. Object-valued query parameters and
  `application/x-www-form-urlencoded` bodies use bracket keys
  (`created[gte]=1`, `address[city]=…`) as Stripe and most form APIs expect.
* Untagged unions whose variant is an inline `enum` (Stripe's
  `enum: ['']` "clear this field" pattern) no longer emit `String.toJson()`;
  the variant is a plain `String`.
* Untagged-union deserialisation no longer triggers `unnecessary_cast` warnings.

## 0.0.5

### Fixed — generated code

* Generated models now compile with `freezed` 3 / `json_serializable` 6.9+:
  classes are `abstract`, unions are `sealed`, and every model imports the
  types its generated `.g.dart` needs (including through `List<T>` typedefs).
* Enums no longer use the non-existent `@JsonEnum(unknownValue:)`; they are
  enhanced enums with a `value` field, an `unknown` fallback, `fromJson` /
  `toJson`, and `@JsonKey(unknownEnumValue:)` on every field that uses them.
  Enum values that are Dart keywords (`new`, `default`, …) or collide after
  case folding get safe, unique names.
* `Optional<T?>` tri-state PATCH fields work again: the class opts out of the
  freezed-generated `toJson` (`@Freezed(toJson: false)`) and emits one that
  omits `Optional.absent()`; per-type converters replace the generic one that
  `json_serializable` rejects.
* `package:http` client: query/header map entries were emitted as bare
  `${...}` expressions (a syntax error); the client now builds an
  `http.Request` for every HTTP method, sends query/cookie API keys, sets
  `Content-Type` only for JSON bodies (multipart uploads work), and throws a
  typed `<Client>Exception` on non-2xx responses.
* Both clients: path parameters are URI-encoded; enum parameters send their
  wire value instead of `EnumName.value`; `DateTime` parameters are ISO 8601;
  cookie parameters are sent; optional request bodies no longer dereference
  null; required response headers are null-checked before parsing.
* Untagged unions no longer import `package:flutter` and expose a public
  `<Name>Converter`.
* Array/map/alias typedef files import the types they reference.
* Field, parameter, method and class names are sanitised: reserved words
  (`class`, `default`, `in`, `hashCode`, …), dotted schema names
  (`com.example.Pet`), spaces and punctuation in `operationId`/titles all
  produce valid Dart identifiers with `@JsonKey(name:)` mappings.
* Client files import only the models they use; `dart:convert` only when needed.

### Fixed — spec loading

* Swagger 2.0 detection no longer depends on `swagger:` being the first line;
  JSON documents load from disk; YAML merge keys are resolved.
* `$ref` parameters, request bodies, responses and headers
  (`#/components/...`) resolve; path-level `parameters` are merged.
* Inline object schemas (properties, arrays of objects, `additionalProperties`
  objects) become named classes instead of aborting generation.
* Root-level enum and primitive schemas are supported; free-form objects
  (`type: object`, `{}`, `additionalProperties: true`) map to
  `Map<String, dynamic>` / `dynamic`; `format: binary` maps to `List<int>`.
* `nullable: true` + `allOf: [$ref]`, and `oneOf`/`anyOf` with a `null`
  variant, are recognised as nullable references.
* HTTP `basic` security schemes are supported; unsupported schemes and
  non-scalar response headers produce warnings instead of failing the run.
* Swagger 2.0: inline arrays/objects in definitions, `formData`/`file`
  parameters (→ multipart or form-urlencoded request bodies), global
  `parameters`/`responses`, `schemes`, `x-nullable`, path-level parameters.
* Duplicate parameter names across locations no longer panic.

### Fixed — CLI and Dart wrapper

* Null-safe and null-unsafe output were written to the same directory, so the
  second pass overwrote the first with code Dart 3 cannot compile. Null-safe
  output now goes to `<out>/<spec>/`; legacy null-unsafe output is opt-in via
  `--null-unsafe` and lands in `<out>/<spec>/null_unsafe/`.
* `flap --help` / `--version` work; unknown flags are rejected.
* Lockfiles fingerprint the spec content (SHA-256) instead of mtime + size.
* The client class name is a valid identifier for any API title
  (`Petstore 3.1` → `Petstore31Client`).
* The Dart wrapper downloaded the `v0.0.1` binary regardless of the package
  version; the version is now a single constant checked against
  `pubspec.yaml` and `Cargo.toml` by tests.
* `FLAP_BINARY` environment variable points the wrapper at a locally built
  generator; download/platform failures exit with code 70 instead of an
  uncaught exception.
* Linux arm64 binaries are published and recognised.

### Added

* Fixtures and Rust integration tests covering unions, inline objects, Swagger
  2.0, JSON input, multipart, form bodies, cookies and response headers.
* Dart unit tests for the wrapper package.

## 0.0.4

* Fixing pub points

## 0.0.3

* Fixing pub points

## 0.0.2

* Simple version update

## 0.0.1

* Initial pub.dev release.
* Supports OpenAPI 3.0, OpenAPI 3.1, and Swagger 2.0 spec formats.
* Generates `@freezed` Dart/Flutter client libraries from any valid spec.
* Two HTTP client backends: `package:dio` (default) and `package:http`.
* Sound null-safe output (`null_safe/`) and legacy null-unsafe output (`null_unsafe/`) generated side-by-side.
* Full security scheme support: `apiKey` (header/query/cookie), HTTP bearer, HTTP basic, OAuth 2.0, OpenID Connect.
* PATCH tri-state semantics via `Optional<T?>` for `nullable: true` + optional fields.
* Typed Dart 3 named-record return types for operations with declared response headers.
* Discriminated unions (`oneOf` + `discriminator`) → `@Freezed` sealed classes.
* Untagged unions (`anyOf` / `oneOf` without discriminator) → try-each deserialization.
* `allOf` inheritance detection with `Schema.extends` surfaced in the IR.
* Recursive schema detection and `is_recursive` flag on `Field`.
* String and integer `enum:` values with `unknown` sentinel.
* `default:` field values emitted as `@Default(...)` Freezed annotations.
* `format: date-time` → `DateTime`, `format: float/double` → `double`.
* `multipart/form-data` request bodies.
* Multiple `servers:` → constants class (`abstract final class FooClientUrls`).
* `x-*` vendor extensions captured on every IR node.
* `--type-map` / `--import-map` flags to replace spec schemas with hand-written types.
* `--template-dir` flag for Jinja2 template overrides.
* Incremental builds with lockfile per output mode + backend.
* Pre-generation validation accumulates all errors and reports them together.
