# Changelog

All notable changes to this project will be documented in this file.

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
