//! The flap intermediate representation.
//!
//! `flap-spec` lowers a raw OpenAPI document into these types. `flap-emit-*`
//! crates consume them to produce language-specific output. Nothing in here
//! knows about YAML or Dart — it is the contract between the two halves.

use std::{collections::BTreeMap, fmt};

#[derive(Debug, Clone, PartialEq)]
pub enum DefaultValue {
    String(String),
    Integer(i64),
    Number(f64),
    Boolean(bool),
}

// ── Vendor extensions ─────────────────────────────────────────────────────────

/// A single value inside an `x-*` vendor-extension field.
/// Mirrors the YAML value space without pulling in a YAML crate.
#[derive(Debug, Clone, PartialEq)]
pub enum ExtensionValue {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Sequence(Vec<ExtensionValue>),
    Mapping(BTreeMap<String, ExtensionValue>),
}

/// All `x-*` keys declared on a single spec node.
/// Keys retain the `x-` prefix so callers can pattern-match on them.
pub type Extensions = BTreeMap<String, ExtensionValue>;

/// A single enum value. OpenAPI allows both string and integer enum values.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EnumValue {
    Str(String),
    Int(i64),
}

impl fmt::Display for EnumValue {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnumValue::Str(s) => f.write_str(s),
            EnumValue::Int(n) => write!(f, "{n}"),
        }
    }
}

// ── Top-level ────────────────────────────────────────────────────────────────

/// A fully-resolved API, ready for code generation.
#[derive(Debug)]
pub struct Api {
    pub title: String,
    /// All server URLs declared in `servers:`, in spec order.
    /// Empty when no `servers:` block is present.
    pub base_urls: Vec<String>,
    /// Ordered by path, then by HTTP method — deterministic output.
    pub operations: Vec<Operation>,
    /// Ordered by schema name — deterministic output.
    pub schemas: Vec<Schema>,
    /// All security schemes declared in `components.securitySchemes`,
    /// ordered alphabetically by scheme name (BTreeMap-driven, deterministic).
    /// Empty when the spec defines no security schemes — the emitter then
    /// skips generating any auth-related code.
    pub security_schemes: Vec<SecurityScheme>,
    /// The set of security scheme names applied to every operation by
    /// default (the top-level `security` block). Stored as a flat,
    /// deduplicated list of scheme names — OpenAPI's full
    /// list-of-AND-of-OR structure is collapsed, since the generated Dart
    /// client supports providing any combination of credentials and sending
    /// whichever ones are non-null. Each entry is expected to reference an
    /// entry in `security_schemes`.
    pub security: Vec<String>,
    pub extensions: Extensions,
    /// Non-fatal problems found while lowering the spec (unsupported
    /// constructs that were degraded to `dynamic`, skipped headers, …).
    /// The CLI prints these; nothing in the IR depends on them.
    pub warnings: Vec<String>,
}

// ── Operations ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HttpMethod {
    Delete,
    Get,
    Head,
    Options,
    Patch,
    Post,
    Put,
    Trace,
}

impl HttpMethod {
    /// Returns the uppercase HTTP method name as a static string slice.
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "DELETE",
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::Patch => "PATCH",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Trace => "TRACE",
        }
    }
}

impl fmt::Display for HttpMethod {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug)]
pub struct Operation {
    pub method: HttpMethod,
    pub path: String,
    pub operation_id: Option<String>,
    pub summary: Option<String>,
    /// Query, path, header, and cookie parameters for this operation.
    /// Path-level parameters are merged in ahead of operation-level ones;
    /// an operation-level parameter with the same `name` + `in` overrides
    /// the path-level declaration.
    pub parameters: Vec<Parameter>,
    /// The request body, if this operation accepts one.
    pub request_body: Option<RequestBody>,
    /// All declared responses for this operation, keyed by status code.
    ///
    /// Ordered deterministically: numeric status codes ascending first,
    /// then `default` last (so "200", "201", "404", "default"). Empty when
    /// no responses are declared.
    pub responses: Vec<Response>,
    /// Per-operation security override.
    pub security: Option<Vec<String>>,
    /// `deprecated: true` in the spec — emitters mark the method `@Deprecated`.
    pub deprecated: bool,
    pub extensions: Extensions,
}

// ── Parameters ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParameterLocation {
    Cookie,
    Header,
    Path,
    Query,
}

impl ParameterLocation {
    /// Returns the lowercase OpenAPI `in` value for this location.
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cookie => "cookie",
            Self::Header => "header",
            Self::Path => "path",
            Self::Query => "query",
        }
    }
}

impl fmt::Display for ParameterLocation {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// OpenAPI parameter serialization `style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterStyle {
    /// Default for query and cookie parameters.
    Form,
    /// Default for path and header parameters.
    Simple,
    /// `name[key]=value` for objects, `name[0]=value` for arrays.
    DeepObject,
    SpaceDelimited,
    PipeDelimited,
    Label,
    Matrix,
}

impl ParameterStyle {
    /// The default style for a location, per the OpenAPI specification.
    #[inline]
    pub fn default_for(location: ParameterLocation) -> Self {
        match location {
            ParameterLocation::Query | ParameterLocation::Cookie => Self::Form,
            ParameterLocation::Path | ParameterLocation::Header => Self::Simple,
        }
    }
}

#[derive(Debug)]
pub struct Parameter {
    pub name: String,
    pub location: ParameterLocation,
    pub type_ref: TypeRef,
    pub required: bool,
    /// Serialization style; defaults per location when the spec omits it.
    pub style: ParameterStyle,
    /// OpenAPI `explode`; defaults to true for `form`, false otherwise.
    pub explode: bool,
    pub extensions: Extensions,
}

// ── Request body ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct RequestBody {
    /// The selected media type, e.g. `application/json`.
    pub content_type: String,
    pub schema_ref: TypeRef,
    pub required: bool,
    /// True when `content_type` is `multipart/form-data`.
    pub is_multipart: bool,
    pub extensions: Extensions,
}

impl RequestBody {
    /// True when the body must be sent as `application/x-www-form-urlencoded`.
    #[inline]
    pub fn is_form_urlencoded(&self) -> bool {
        self.content_type
            .eq_ignore_ascii_case("application/x-www-form-urlencoded")
    }

    /// True when the body is JSON (`application/json`, `application/problem+json`, …).
    #[inline]
    pub fn is_json(&self) -> bool {
        let ct = self.content_type.to_ascii_lowercase();
        ct.starts_with("application/json") || ct.ends_with("+json")
    }
}

// ── Response headers ──────────────────────────────────────────────────────────

/// A single header declared on a response.
///
/// OpenAPI allows any header whose schema resolves to a scalar or array of
/// scalars. Headers whose schema is an object are skipped at lowering time.
#[derive(Debug)]
pub struct ResponseHeader {
    /// The wire-side header name, e.g. `"X-Rate-Limit-Remaining"`.
    pub name: String,
    pub type_ref: TypeRef,
    /// Whether the header is guaranteed to be present on every response.
    /// Emitters use this to decide between `String` and `String?`.
    pub required: bool,
}

#[derive(Debug)]
pub struct Response {
    pub status_code: String,
    /// The media type the body schema was taken from (e.g. `application/json`),
    /// when the response declares content.
    pub content_type: Option<String>,
    pub schema_ref: Option<TypeRef>,
    /// Response headers declared in the spec for this status code.
    /// Empty when no `headers:` block is present — the emitter then
    /// returns the body type directly rather than wrapping in a record.
    pub headers: Vec<ResponseHeader>,
    pub extensions: Extensions,
}

// ── Schemas ──────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct Schema {
    pub name: String,
    pub kind: SchemaKind,
    /// When true the emitter skips this schema – it's an implementation
    /// detail of the lowering (e.g. an anyOf wrapper for a primitive).
    pub internal: bool,
    /// Set to `Some(parent_name)` when this schema is defined as
    ///   `allOf: [$ref: Parent, {type: object, properties: {...}}]`
    /// i.e. classical OOP-style inheritance. Emitters can use this to
    /// generate proper subtype relationships rather than flat objects.
    pub extends: Option<String>,
    pub extensions: Extensions,
}

#[derive(Debug)]
pub enum SchemaKind {
    /// An object with a fixed set of named fields.
    Object { fields: Vec<Field> },
    /// A homogeneous list — e.g. OpenAPI `type: array`.
    Array { item: TypeRef },
    /// A homogeneous string-keyed dictionary at the top level.
    Map { value: TypeRef },
    /// A discriminated union — OpenAPI `oneOf` with a `discriminator`.
    Union {
        variants: Vec<TypeRef>,
        discriminator: String,
        variant_tags: Vec<String>,
    },
    /// A union of forms where no single explicit discriminator field
    /// exists. Deserialization must attempt each variant in order.
    UntaggedUnion { variants: Vec<TypeRef> },
    /// A top-level `$ref` alias — emitted as a Dart `typedef`.
    Alias { target: String },
    /// A named enum schema (`type: string, enum: [...]`) — emitted as a
    /// Dart `enum` with the schema's name.
    Enum { values: Vec<EnumValue> },
    /// A named primitive schema (`type: string`, `type: integer`, …) —
    /// emitted as a Dart `typedef` onto the primitive type.
    Primitive { type_ref: TypeRef },
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub type_ref: TypeRef,
    pub required: bool,
    /// True when the field's value may legitimately be JSON `null` on the
    /// wire — set by lowering when the schema declares `nullable: true`
    /// (OpenAPI 3.0). Orthogonal to `required`:
    ///
    /// | `required` | `nullable` | wire semantics                              |
    /// |------------|------------|---------------------------------------------|
    /// | true       | false      | key MUST be present, value non-null         |
    /// | true       | true       | key MUST be present, value MAY be null      |
    /// | false      | false      | key MAY be omitted, never null when present |
    /// | false      | true       | key MAY be omitted, value MAY be null       |
    ///
    /// The last row is the one that motivates the Dart `Optional<T?>`
    /// wrapper in `flap_emit_dart` — it's the only case where the
    /// receiver of a PATCH must be able to distinguish "client did not
    /// send this key" from "client explicitly set this key to null".
    ///
    /// OpenAPI 3.1's `type: [T, "null"]` is translated into this flag by
    /// the lowering pass — the spec crate's `is_nullable` helper detects
    /// `"null"` in the type array and the result is OR-merged with the 3.0
    /// `nullable: true` keyword before being stored here.
    pub nullable: bool,
    /// True when this field's type (directly, or via `List<>` / `Map<>`)
    /// points at the schema currently being lowered — i.e. self-recursion
    /// (`Node.children: List<Node>`) or a back-edge through an `allOf`
    /// chain. Set by the lowering pass; downstream emitters use this as
    /// an explicit signal that the field type must be rendered as the
    /// bare class name (no inline typedef wrapping that would break
    /// Freezed's generator).
    pub is_recursive: bool,
    /// A spec-declared default value for this field, if present and expressible
    /// as a Dart compile-time constant. `None` when the spec has no `default:`,
    /// or when the type is too complex (array, object) to inline.
    pub default_value: Option<DefaultValue>,
    pub extensions: Extensions,
}

impl Field {
    /// Construct a non-recursive, non-nullable field — the common case
    /// for hand-written IR (test fixtures, golden builders, etc.). The
    /// lowering pass in `flap_spec` constructs `Field` directly because
    /// it computes `is_recursive` and `nullable` from the source spec;
    /// everywhere else, prefer this.
    #[inline]
    #[must_use]
    pub fn new(name: impl Into<String>, type_ref: TypeRef, required: bool) -> Self {
        Self {
            name: name.into(),
            type_ref,
            required,
            nullable: false,
            is_recursive: false,
            default_value: None,
            extensions: Extensions::default(),
        }
    }
}

/// A reference to a concrete type, either primitive or a named schema.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeRef {
    String,
    Integer {
        format: Option<String>,
    },
    Number {
        format: Option<String>,
    },
    Boolean,
    /// `type: string, format: date-time` — emitted as Dart `DateTime`.
    DateTime,
    /// `type: string, format: binary` — raw bytes, emitted as Dart `List<int>`.
    Binary,
    /// A schema with no usable type information (`{}`, `type: object` with
    /// neither `properties` nor `additionalProperties`, unsupported
    /// constructs). Emitted as Dart `dynamic`.
    Any,
    /// A closed set of allowed values (`enum: [...]`).
    /// Supports both string and integer enum values.
    Enum(Vec<EnumValue>),
    /// A homogeneous string-keyed dictionary value type.
    Map(Box<TypeRef>),
    /// An inline homogeneous list — `{ type: array, items: ... }`.
    Array(Box<TypeRef>),
    /// Reference to a named component schema (the bare name, not the $ref path).
    Named(String),
}

impl TypeRef {
    /// True for scalar JSON types (string, number, integer, boolean, date-time).
    #[inline]
    pub fn is_scalar(&self) -> bool {
        matches!(
            self,
            TypeRef::String
                | TypeRef::Integer { .. }
                | TypeRef::Number { .. }
                | TypeRef::Boolean
                | TypeRef::DateTime
        )
    }
}

impl fmt::Display for TypeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeRef::String => f.write_str("string"),
            TypeRef::Integer { format } => match format {
                Some(s) => write!(f, "integer({s})"),
                None => f.write_str("integer"),
            },
            TypeRef::Number { format } => match format {
                Some(s) => write!(f, "number({s})"),
                None => f.write_str("number"),
            },
            TypeRef::Boolean => f.write_str("boolean"),
            TypeRef::DateTime => f.write_str("date-time"),
            TypeRef::Binary => f.write_str("binary"),
            TypeRef::Any => f.write_str("any"),
            TypeRef::Enum(values) => {
                f.write_str("enum[")?;
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            TypeRef::Map(inner) => write!(f, "map<{inner}>"),
            TypeRef::Array(inner) => write!(f, "array<{inner}>"),
            TypeRef::Named(n) => f.write_str(n),
        }
    }
}

// ── Security ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyLocation {
    Cookie,
    Header,
    Query,
}

impl ApiKeyLocation {
    /// Returns the lowercase OpenAPI `in` value for this location.
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cookie => "cookie",
            Self::Header => "header",
            Self::Query => "query",
        }
    }
}

impl fmt::Display for ApiKeyLocation {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── OAuth2 ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuth2FlowType {
    Implicit,
    Password,
    ClientCredentials,
    AuthorizationCode,
}

impl OAuth2FlowType {
    /// Returns the OAuth2 flow type name as used in OpenAPI specs.
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implicit => "implicit",
            Self::Password => "password",
            Self::ClientCredentials => "clientCredentials",
            Self::AuthorizationCode => "authorizationCode",
        }
    }
}

impl fmt::Display for OAuth2FlowType {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct OAuth2Flow {
    pub flow_type: OAuth2FlowType,
    /// Present for: password, clientCredentials, authorizationCode.
    pub token_url: Option<String>,
    /// Present for: implicit, authorizationCode.
    pub authorization_url: Option<String>,
    /// Scope names declared by the flow (descriptions are dropped).
    pub scopes: Vec<String>,
}

// Security schemes

/// The kind of security scheme, without the shared name.
#[derive(Debug, Clone)]
pub enum SecuritySchemeKind {
    ApiKey {
        parameter_name: String,
        location: ApiKeyLocation,
    },
    HttpBasic,
    HttpBearer {
        bearer_format: Option<String>,
    },
    /// At least one flow is always present - lowering rejects empty `flows` blocks.
    OAuth2 {
        flows: Vec<OAuth2Flow>,
    },
    OpenIdConnect {
        openid_connect_url: String,
    },
}

/// A resolved security scheme. The `name` is the key from
/// `components/securitySchemes`; `kind` carries the type-specific fields.
#[derive(Debug, Clone)]
pub struct SecurityScheme {
    /// The scheme name as declared in the spec (e.g. `"bearerAuth"`).
    pub name: String,
    pub kind: SecuritySchemeKind,
}
