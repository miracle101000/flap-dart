//! Dart / Flutter code emitter.
//!
//! Public API:
//! - [`emit_models`] — one Dart source per top-level schema, plus one per
//!   synthesised inline enum, plus the shared `flap_utils.dart` runtime.
//! - [`emit_client`] — a single client file with one method per operation.
//!   Pass [`ClientBackend::Dio`] (default) or [`ClientBackend::Http`].
//!
//! The generated null-safe output targets `freezed` ≥ 3, `json_serializable`
//! ≥ 6.9 and Dart ≥ 3.8 (json_serializable emits null-aware map elements).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use flap_ir::{
    Api, ApiKeyLocation, DefaultValue, EnumValue, Field, Operation, ParameterLocation, RequestBody,
    Response, Schema, SchemaKind, SecurityScheme, SecuritySchemeKind, TypeRef,
};

macro_rules! w {
    ($out:expr, $($arg:tt)*) => {{ let _ = write!($out, $($arg)*); }};
}
macro_rules! wl {
    ($out:expr) => {{ let _ = writeln!($out); }};
    ($out:expr, $($arg:tt)*) => {{ let _ = writeln!($out, $($arg)*); }};
}

// ── Identifier policy ────────────────────────────────────────────────────────

/// Dart core / commonly-imported names a generated class must not shadow.
const DART_CORE_COLLISIONS: &[&str] = &[
    "bool",
    "DateTime",
    "double",
    "Duration",
    "Error",
    "Exception",
    "Function",
    "Future",
    "int",
    "Iterable",
    "List",
    "Map",
    "num",
    "Object",
    "Pattern",
    "Record",
    "RegExp",
    "Set",
    "Stream",
    "String",
    "Symbol",
    "Type",
    "Uri",
    "dynamic",
    "void",
    "Null",
    "Never",
    "Enum",
    "Optional",
    "JsonKey",
    "JsonValue",
    "JsonEnum",
    "JsonConverter",
    "Freezed",
    "Default",
    "Dio",
    "Options",
    "Response",
    "CancelToken",
    "FormData",
    "MultipartFile",
    "Interceptor",
    "BaseOptions",
    "Headers",
];

const DART_RESERVED_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "assert",
    "async",
    "await",
    "base",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "covariant",
    "default",
    "deferred",
    "do",
    "dynamic",
    "else",
    "enum",
    "export",
    "extends",
    "extension",
    "external",
    "factory",
    "false",
    "final",
    "finally",
    "for",
    "get",
    "hide",
    "if",
    "implements",
    "import",
    "in",
    "interface",
    "is",
    "late",
    "library",
    "mixin",
    "new",
    "null",
    "of",
    "on",
    "operator",
    "part",
    "required",
    "rethrow",
    "return",
    "sealed",
    "set",
    "show",
    "static",
    "super",
    "switch",
    "sync",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "var",
    "void",
    "when",
    "while",
    "with",
    "yield",
];

/// Members every Dart object / freezed class already has.
const RESERVED_MEMBERS: &[&str] = &[
    "copyWith",
    "toJson",
    "fromJson",
    "toString",
    "hashCode",
    "runtimeType",
    "noSuchMethod",
];

/// Names an enum constant may not take.
const RESERVED_ENUM_MEMBERS: &[&str] = &["values", "index", "name", "value", "toJson", "fromJson"];

/// Split an arbitrary identifier into words: `XRateLimit` → [X, Rate, Limit],
/// `display_name` → [display, name], `HTTPResponse` → [HTTP, Response],
/// `com.example.Pet` → [com, example, Pet].
fn words(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if !cur.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let boundary = (prev.is_lowercase() || prev.is_ascii_digit()) && c.is_uppercase()
                || (prev.is_uppercase() && c.is_uppercase() && next_lower);
            if boundary {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
    }
}

fn to_pascal_case(s: &str) -> String {
    let mut out: String = words(s).iter().map(|w| capitalize(w)).collect();
    if out.is_empty() {
        out.push_str("Value");
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'N');
    }
    out
}

fn to_camel_case(s: &str) -> String {
    let ws = words(s);
    let mut out = String::new();
    for (i, w) in ws.iter().enumerate() {
        if i == 0 {
            out.push_str(&w.to_lowercase());
        } else {
            out.push_str(&capitalize(w));
        }
    }
    if out.is_empty() {
        out.push_str("value");
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'n');
    }
    out
}

fn to_snake_case(s: &str) -> String {
    let mut out = words(s)
        .iter()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("_");
    if out.is_empty() {
        out.push_str("value");
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'n');
    }
    out
}

/// Keyword- and reserved-member-safe camelCase identifier.
fn member_name(spec_name: &str) -> String {
    let name = to_camel_case(spec_name);
    if DART_RESERVED_KEYWORDS.contains(&name.as_str()) || RESERVED_MEMBERS.contains(&name.as_str())
    {
        format!("{name}Value")
    } else {
        name
    }
}

fn dart_class_name(schema_name: &str) -> String {
    let name = to_pascal_case(schema_name);
    if DART_CORE_COLLISIONS.contains(&name.as_str())
        || DART_RESERVED_KEYWORDS.contains(&name.as_str())
    {
        format!("{name}Model")
    } else {
        name
    }
}

/// A Dart single-quoted string literal for `s`.
fn dart_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Make `base` unique within `taken`, registering the result.
fn unique(base: &str, taken: &mut HashSet<String>, first_suffix: &str) -> String {
    if taken.insert(base.to_string()) {
        return base.to_string();
    }
    let with_suffix = format!("{base}{first_suffix}");
    if !first_suffix.is_empty() && taken.insert(with_suffix.clone()) {
        return with_suffix;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}{n}");
        if taken.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// Write import lines grouped `dart:` / `package:` / relative, each group
/// sorted, with a blank line between groups (matches `directives_ordering`).
fn write_imports(out: &mut String, imports: &BTreeSet<String>) {
    let rank = |l: &str| {
        if l.contains("'dart:") {
            0
        } else if l.contains("'package:") {
            1
        } else {
            2
        }
    };
    let mut last_rank: Option<u8> = None;
    for line in imports
        .iter()
        .map(|l| (rank(l), l))
        .collect::<BTreeSet<_>>()
    {
        if last_rank.is_some_and(|r| r != line.0) {
            wl!(out);
        }
        wl!(out, "{}", line.1);
        last_rank = Some(line.0);
    }
}

fn doc_comment(out: &mut String, indent: &str, text: &str) {
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            wl!(out, "{indent}///");
        } else {
            wl!(out, "{indent}/// {line}");
        }
    }
}

// ── Public configuration types ────────────────────────────────────────────────

/// Controls whether the emitted Dart code targets sound null safety (Dart ≥ 3)
/// or the legacy null-unsafe dialect (Dart < 2.12).
///
/// Dart 3 SDKs cannot compile null-unsafe code; `Unsafe` is retained for
/// projects pinned to old toolchains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NullSafety {
    Safe,
    Unsafe,
}

/// Controls which HTTP client package the generated client uses.
///
/// | concern              | Dio                          | Http                        |
/// |----------------------|------------------------------|-----------------------------|
/// | package              | `package:dio/dio.dart`       | `package:http/http.dart`    |
/// | CancelToken          | ✅ per-method param           | ❌                          |
/// | Interceptors         | ✅ constructor param          | ❌                          |
/// | BaseOptions          | ✅ constructor param          | ❌                          |
/// | HttpClientAdapter    | ✅ constructor param          | ❌                          |
/// | Injectable client    | ✅ via BaseOptions            | ✅ `http.Client?` param      |
/// | Multipart / FormData | ✅ `FormData`                 | ✅ `MultipartRequest`        |
/// | Response headers     | ✅ typed record               | ✅ typed record             |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientBackend {
    /// `package:dio` — full feature set. This is the default.
    Dio,
    /// `package:http` — simpler, no interceptors or cancel tokens.
    Http,
}

impl ClientBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            ClientBackend::Dio => "dio",
            ClientBackend::Http => "http",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MappingConfig {
    /// Schema name → replacement Dart type.  e.g. `"Pet" → "ExamplePet"`.
    pub type_map: HashMap<String, String>,
    /// Dart type name → import URI.  e.g. `"ExamplePet" → "package:myapp/example_pet.dart"`.
    pub import_map: HashMap<String, String>,
}

impl MappingConfig {
    pub fn is_empty(&self) -> bool {
        self.type_map.is_empty() && self.import_map.is_empty()
    }

    /// Resolve a schema name to its Dart class name, honouring any type mapping.
    pub fn resolve_class(&self, schema_name: &str) -> String {
        if let Some(mapped) = self.type_map.get(schema_name) {
            mapped.clone()
        } else {
            dart_class_name(schema_name)
        }
    }

    /// Returns a ready-to-emit import line for a Dart type, if one is registered.
    pub fn import_for(&self, dart_type: &str) -> Option<String> {
        self.import_map
            .get(dart_type)
            .map(|path| format!("import {};", dart_str(path)))
    }
}

#[derive(Debug, Clone, Default)]
pub struct TemplateConfig {
    /// Directory to search for template overrides.
    /// Resolution order per output file:
    ///   1. `{dir}/{exact_filename}`       — verbatim copy, no rendering
    ///   2. `{dir}/model.dart.jinja`       — Jinja2 template for every model
    ///   3. `{dir}/client.dart.jinja`      — Jinja2 template for the client
    ///   4. `{dir}/flap_utils.dart`        — verbatim override for the runtime file
    ///   5. built-in emitter               — fallback when nothing matches
    pub template_dir: Option<std::path::PathBuf>,
}

impl TemplateConfig {
    fn verbatim(&self, filename: &str) -> Option<String> {
        let dir = self.template_dir.as_ref()?;
        std::fs::read_to_string(dir.join(filename)).ok()
    }

    fn jinja(&self, name: &str) -> Option<String> {
        let dir = self.template_dir.as_ref()?;
        std::fs::read_to_string(dir.join(format!("{name}.jinja"))).ok()
    }
}

// ── Jinja2 template contexts ──────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct ModelTemplateCtx {
    /// Dart class name after collision-avoidance and type mapping.
    class_name: String,
    /// Original schema name from the spec.
    schema_name: String,
    /// snake_case class name — used for `part` directives.
    snake_name: String,
    /// "object" | "array" | "map" | "union" | "untagged_union" | "alias" | "enum" | "primitive"
    kind: String,
    fields: Vec<FieldTemplateCtx>,
    /// Ready-to-emit `import '...';` lines, sorted and deduped.
    imports: Vec<String>,
    has_optional_fields: bool,
    /// Parent schema name when the schema uses allOf inheritance.
    extends: Option<String>,
    /// "safe" or "unsafe"
    null_safety: String,
}

#[derive(serde::Serialize)]
struct FieldTemplateCtx {
    /// Original spec field name.
    spec_name: String,
    /// camelCase Dart identifier.
    dart_name: String,
    /// Full resolved Dart type, e.g. `"List<String>"`, `"Pet?"`, `"Optional<int?>"`.
    dart_type: String,
    required: bool,
    nullable: bool,
    /// True when this field uses the `Optional<T?>` absent/present wrapper.
    uses_optional_wrapper: bool,
    /// `@Default(...)` expression, if any.
    default_expr: Option<String>,
    /// Non-null when the Dart identifier differs from the spec name.
    json_name: Option<String>,
}

#[derive(serde::Serialize)]
struct ClientTemplateCtx {
    class_name: String,
    /// First server URL, or empty string.
    default_base_url: String,
    base_urls: Vec<String>,
    operations: Vec<OperationTemplateCtx>,
    credentials: Vec<CredentialTemplateCtx>,
    /// "dio" or "http"
    backend: String,
    /// "safe" or "unsafe"
    null_safety: String,
}

#[derive(serde::Serialize)]
struct OperationTemplateCtx {
    method: String,
    path: String,
    method_name: String,
    summary: Option<String>,
    deprecated: bool,
    return_type: String,
    parameters: Vec<ParamTemplateCtx>,
    has_body: bool,
    body_type: Option<String>,
    body_required: bool,
    is_multipart: bool,
    body_content_type: Option<String>,
}

#[derive(serde::Serialize)]
struct ParamTemplateCtx {
    spec_name: String,
    dart_name: String,
    dart_type: String,
    /// "query", "path", "header", or "cookie"
    location: String,
    required: bool,
}

#[derive(serde::Serialize)]
struct CredentialTemplateCtx {
    dart_param_name: String,
    /// "apiKey", "httpBearer", "httpBasic", "oauth2", "openIdConnect"
    scheme_type: String,
}

fn render_jinja(template_src: &str, context: impl serde::Serialize) -> Result<String, String> {
    let mut env = minijinja::Environment::new();
    env.add_template("t", template_src)
        .map_err(|e| format!("template parse error: {e}"))?;
    let tmpl = env
        .get_template("t")
        .map_err(|e| format!("template lookup error: {e}"))?;
    let ctx = minijinja::Value::from_serialize(&context);
    tmpl.render(ctx)
        .map_err(|e| format!("template render error: {e}"))
}

// ── Emission context ──────────────────────────────────────────────────────────

/// Everything the emitter needs about the API, precomputed once: class
/// names, synthesised enum names, method names — all collision-free.
struct Ctx<'a> {
    api: &'a Api,
    mode: NullSafety,
    mappings: &'a MappingConfig,
    /// schema name → Dart class name
    class_names: HashMap<String, String>,
    /// (schema name, field name) → synthesised enum class
    field_enums: HashMap<(String, String), String>,
    /// (operation index, parameter name, location) → synthesised enum class
    param_enums: HashMap<(usize, String, ParameterLocation), String>,
    /// operation index → synthesised enum class for the request body
    body_enums: HashMap<usize, String>,
    /// (operation index, status code) → synthesised enum class
    response_enums: HashMap<(usize, String), String>,
    /// synthesised enum class → values
    synth_enums: BTreeMap<String, Vec<EnumValue>>,
    /// operation index → Dart method name
    method_names: Vec<String>,
    /// union schema name → per-variant (factory name, class name)
    union_variant_names: HashMap<String, Vec<(String, String)>>,
}

impl<'a> Ctx<'a> {
    fn build(api: &'a Api, mode: NullSafety, mappings: &'a MappingConfig) -> Self {
        let mut taken: HashSet<String> = HashSet::new();
        // Never generate a class that shadows a mapped-in external type.
        for mapped in mappings.type_map.values() {
            taken.insert(mapped.clone());
        }
        let client_class = api_client_name(&api.title);
        taken.insert(client_class.clone());
        taken.insert(format!("{client_class}Urls"));
        taken.insert(format!("{client_class}Exception"));

        let mut class_names: HashMap<String, String> = HashMap::new();
        for schema in &api.schemas {
            let cls = if let Some(mapped) = mappings.type_map.get(&schema.name) {
                mapped.clone()
            } else {
                unique(&dart_class_name(&schema.name), &mut taken, "Model")
            };
            class_names.insert(schema.name.clone(), cls);
        }

        let mut union_variant_names: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for schema in &api.schemas {
            if let SchemaKind::Union { variants, .. } = &schema.kind {
                let union_cls = class_names[&schema.name].clone();
                let mut factories: HashSet<String> = HashSet::new();
                factories.insert("fromJson".to_string());
                let mut names = Vec::new();
                for v in variants {
                    let raw = match v {
                        TypeRef::Named(n) => n.as_str(),
                        _ => "variant",
                    };
                    let factory = unique(&member_name(raw), &mut factories, "Variant");
                    let cls = unique(
                        &format!("{union_cls}{}", to_pascal_case(&factory)),
                        &mut taken,
                        "Case",
                    );
                    names.push((factory, cls));
                }
                union_variant_names.insert(schema.name.clone(), names);
            }
        }

        let mut synth_enums: BTreeMap<String, Vec<EnumValue>> = BTreeMap::new();
        let mut field_enums = HashMap::new();
        let mut param_enums = HashMap::new();
        let mut body_enums = HashMap::new();
        let mut response_enums = HashMap::new();

        let mut register =
            |hint: String, values: &[EnumValue], taken: &mut HashSet<String>| -> String {
                let name = unique(&dart_class_name(&hint), taken, "Enum");
                synth_enums.insert(name.clone(), values.to_vec());
                name
            };

        for schema in &api.schemas {
            if let SchemaKind::Object { fields } = &schema.kind {
                for field in fields {
                    if let Some(values) = first_enum(&field.type_ref) {
                        let hint = format!(
                            "{}{}",
                            class_names[&schema.name],
                            to_pascal_case(&field.name)
                        );
                        let name = register(hint, values, &mut taken);
                        field_enums.insert((schema.name.clone(), field.name.clone()), name);
                    }
                }
            }
        }

        let mut method_taken: HashSet<String> = HashSet::new();
        let mut method_names = Vec::with_capacity(api.operations.len());
        for op in &api.operations {
            let base = match &op.operation_id {
                Some(id) if !id.trim().is_empty() => member_name(id),
                _ => {
                    let slug: String = op
                        .path
                        .split('/')
                        .filter(|s| !s.is_empty())
                        .map(|s| to_pascal_case(s.trim_matches(|c| c == '{' || c == '}')))
                        .collect();
                    member_name(&format!("{}{slug}", op.method.as_str().to_lowercase()))
                }
            };
            method_names.push(unique(&base, &mut method_taken, ""));
        }

        for (i, op) in api.operations.iter().enumerate() {
            let op_pascal = to_pascal_case(&method_names[i]);
            for param in &op.parameters {
                if let Some(values) = first_enum(&param.type_ref) {
                    let hint = format!("{op_pascal}{}", to_pascal_case(&param.name));
                    let name = register(hint, values, &mut taken);
                    param_enums.insert((i, param.name.clone(), param.location), name);
                }
            }
            if let Some(body) = &op.request_body
                && let Some(values) = first_enum(&body.schema_ref)
            {
                let name = register(format!("{op_pascal}Body"), values, &mut taken);
                body_enums.insert(i, name);
            }
            for resp in &op.responses {
                if let Some(values) = resp.schema_ref.as_ref().and_then(first_enum) {
                    let code: String = resp
                        .status_code
                        .chars()
                        .filter(|c| c.is_alphanumeric())
                        .collect();
                    let name = register(
                        format!("{op_pascal}{}Response", to_pascal_case(&code)),
                        values,
                        &mut taken,
                    );
                    response_enums.insert((i, resp.status_code.clone()), name);
                }
            }
        }

        Self {
            api,
            mode,
            mappings,
            class_names,
            field_enums,
            param_enums,
            body_enums,
            response_enums,
            synth_enums,
            method_names,
            union_variant_names,
        }
    }

    fn safe(&self) -> bool {
        self.mode == NullSafety::Safe
    }

    fn class_name(&self, schema_name: &str) -> String {
        self.class_names
            .get(schema_name)
            .cloned()
            .unwrap_or_else(|| self.mappings.resolve_class(schema_name))
    }

    fn schema(&self, name: &str) -> Option<&'a Schema> {
        self.api.schemas.iter().find(|s| s.name == name)
    }

    fn kind(&self, name: &str) -> Option<&'a SchemaKind> {
        self.schema(name).map(|s| &s.kind)
    }

    fn is_mapped(&self, schema_name: &str) -> bool {
        self.mappings.type_map.contains_key(schema_name)
    }

    /// Import line for a generated (or mapped) Dart class.
    fn import_for_class(&self, cls: &str) -> String {
        self.mappings
            .import_for(cls)
            .unwrap_or_else(|| format!("import '{}.dart';", to_snake_case(cls)))
    }

    /// `T?` in safe mode (never for `dynamic`), `T` in unsafe mode.
    fn nullable(&self, base: &str) -> String {
        if !self.safe() || base == "dynamic" || base.ends_with('?') {
            base.to_string()
        } else {
            format!("{base}?")
        }
    }

    /// `required ` (safe) or `@required ` (unsafe) prefix for a named parameter.
    fn required_kw(&self) -> &'static str {
        if self.safe() {
            "required "
        } else {
            "@required "
        }
    }

    /// Postfix null-assertion, when the language has one.
    fn bang(&self) -> &'static str {
        if self.safe() { "!" } else { "" }
    }

    /// The Dart type for `t`. `enum_name` names the synthesised enum used
    /// for any inline `TypeRef::Enum` in the tree.
    fn dart_type(&self, t: &TypeRef, enum_name: Option<&str>) -> String {
        match t {
            TypeRef::String => "String".into(),
            TypeRef::Integer { .. } => "int".into(),
            TypeRef::Number { format } => match format.as_deref() {
                Some("float" | "double") => "double".into(),
                _ => "num".into(),
            },
            TypeRef::Boolean => "bool".into(),
            TypeRef::DateTime => "DateTime".into(),
            TypeRef::Binary => "List<int>".into(),
            TypeRef::Any => "dynamic".into(),
            TypeRef::Enum(_) => enum_name
                .map(str::to_string)
                .unwrap_or_else(|| "String".into()),
            TypeRef::Map(inner) => format!("Map<String, {}>", self.dart_type(inner, enum_name)),
            TypeRef::Array(inner) => format!("List<{}>", self.dart_type(inner, enum_name)),
            TypeRef::Named(name) => self.class_name(name),
        }
    }

    /// The enum class used somewhere in `t`'s type tree (for
    /// `@JsonKey(unknownEnumValue:)`), if any.
    fn enum_class_in(&self, t: &TypeRef, enum_name: Option<&str>) -> Option<String> {
        match t {
            TypeRef::Enum(_) => enum_name.map(str::to_string),
            TypeRef::Map(inner) | TypeRef::Array(inner) => self.enum_class_in(inner, enum_name),
            TypeRef::Named(name) => match self.kind(name) {
                Some(SchemaKind::Enum { .. }) => Some(self.class_name(name)),
                Some(SchemaKind::Alias { target }) => {
                    self.enum_class_in(&TypeRef::Named(target.clone()), None)
                }
                Some(SchemaKind::Array { item }) => self.enum_class_in(item, None),
                Some(SchemaKind::Map { value }) => self.enum_class_in(value, None),
                _ => None,
            },
            _ => None,
        }
    }

    /// The untagged-union class used somewhere in `t` (needs a converter).
    fn untagged_union_in(&self, t: &TypeRef) -> Option<String> {
        match t {
            TypeRef::Map(inner) | TypeRef::Array(inner) => self.untagged_union_in(inner),
            TypeRef::Named(name) => match self.kind(name) {
                Some(SchemaKind::UntaggedUnion { .. }) if !self.is_mapped(name) => {
                    Some(self.class_name(name))
                }
                Some(SchemaKind::Alias { target }) => {
                    self.untagged_union_in(&TypeRef::Named(target.clone()))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Collect import lines needed to *use and (de)serialise* `t` from a
    /// file that defines `own_class`. Typedef'd collections do not
    /// re-export their element types, so those are followed transitively.
    fn collect_imports(
        &self,
        t: &TypeRef,
        enum_name: Option<&str>,
        own_class: &str,
        out: &mut BTreeSet<String>,
    ) {
        self.collect_imports_inner(t, enum_name, own_class, out, &mut HashSet::new());
    }

    fn collect_imports_inner(
        &self,
        t: &TypeRef,
        enum_name: Option<&str>,
        own_class: &str,
        out: &mut BTreeSet<String>,
        seen: &mut HashSet<String>,
    ) {
        match t {
            TypeRef::Enum(_) => {
                if let Some(e) = enum_name
                    && e != own_class
                {
                    out.insert(self.import_for_class(e));
                }
            }
            TypeRef::Map(inner) | TypeRef::Array(inner) => {
                self.collect_imports_inner(inner, enum_name, own_class, out, seen);
            }
            TypeRef::Named(name) => {
                if !seen.insert(name.clone()) {
                    return;
                }
                let cls = self.class_name(name);
                if self.is_mapped(name) {
                    if let Some(line) = self.mappings.import_for(&cls) {
                        out.insert(line);
                    }
                    return;
                }
                match self.kind(name) {
                    Some(SchemaKind::Object { .. })
                    | Some(SchemaKind::Union { .. })
                    | Some(SchemaKind::UntaggedUnion { .. })
                    | Some(SchemaKind::Enum { .. }) => {
                        if cls != own_class {
                            out.insert(self.import_for_class(&cls));
                        }
                    }
                    Some(SchemaKind::Array { item }) => {
                        if cls != own_class {
                            out.insert(self.import_for_class(&cls));
                        }
                        self.collect_imports_inner(item, None, own_class, out, seen);
                    }
                    Some(SchemaKind::Map { value }) => {
                        if cls != own_class {
                            out.insert(self.import_for_class(&cls));
                        }
                        self.collect_imports_inner(value, None, own_class, out, seen);
                    }
                    Some(SchemaKind::Alias { target }) => {
                        if cls != own_class {
                            out.insert(self.import_for_class(&cls));
                        }
                        self.collect_imports_inner(
                            &TypeRef::Named(target.clone()),
                            None,
                            own_class,
                            out,
                            seen,
                        );
                    }
                    Some(SchemaKind::Primitive { type_ref }) => {
                        if cls != own_class {
                            out.insert(self.import_for_class(&cls));
                        }
                        self.collect_imports_inner(type_ref, None, own_class, out, seen);
                    }
                    None => {
                        if cls != own_class {
                            out.insert(self.import_for_class(&cls));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// True when `to_json_expr` would return `expr` unchanged.
    fn json_passthrough(&self, t: &TypeRef) -> bool {
        match t {
            TypeRef::String
            | TypeRef::Integer { .. }
            | TypeRef::Number { .. }
            | TypeRef::Boolean
            | TypeRef::Binary
            | TypeRef::Any => true,
            TypeRef::Map(inner) | TypeRef::Array(inner) => self.json_passthrough(inner),
            TypeRef::Named(name) => match self.kind(name) {
                Some(SchemaKind::Array { item }) => self.json_passthrough(item),
                Some(SchemaKind::Map { value }) => self.json_passthrough(value),
                Some(SchemaKind::Alias { target }) => {
                    self.json_passthrough(&TypeRef::Named(target.clone()))
                }
                Some(SchemaKind::Primitive { type_ref }) => self.json_passthrough(type_ref),
                _ => false,
            },
            TypeRef::DateTime | TypeRef::Enum(_) => false,
        }
    }

    /// Dart expression converting the non-null value `expr` of type `t`
    /// into a JSON-encodable value.
    fn to_json_expr(&self, t: &TypeRef, expr: &str) -> String {
        if self.json_passthrough(t) {
            return expr.to_string();
        }
        match t {
            TypeRef::DateTime => format!("{expr}.toIso8601String()"),
            TypeRef::Enum(_) => format!("{expr}.toJson()"),
            TypeRef::Array(inner) => {
                format!(
                    "{expr}.map((e) => {}).toList()",
                    self.to_json_expr(inner, "e")
                )
            }
            TypeRef::Map(inner) => format!(
                "{expr}.map((k, v) => MapEntry(k, {}))",
                self.to_json_expr(inner, "v")
            ),
            TypeRef::Named(name) => match self.kind(name) {
                Some(SchemaKind::Array { item }) => {
                    format!(
                        "{expr}.map((e) => {}).toList()",
                        self.to_json_expr(item, "e")
                    )
                }
                Some(SchemaKind::Map { value }) => format!(
                    "{expr}.map((k, v) => MapEntry(k, {}))",
                    self.to_json_expr(value, "v")
                ),
                Some(SchemaKind::Alias { target }) => {
                    self.to_json_expr(&TypeRef::Named(target.clone()), expr)
                }
                Some(SchemaKind::Primitive { type_ref }) => self.to_json_expr(type_ref, expr),
                _ => format!("{expr}.toJson()"),
            },
            _ => expr.to_string(),
        }
    }

    /// Dart expression turning the decoded-JSON value `expr` into type `t`.
    fn deserialize_expr(&self, t: &TypeRef, expr: &str, enum_name: Option<&str>) -> String {
        match t {
            TypeRef::String => format!("{expr} as String"),
            TypeRef::Integer { .. } => format!("({expr} as num).toInt()"),
            TypeRef::Number { format } => match format.as_deref() {
                Some("float" | "double") => format!("({expr} as num).toDouble()"),
                _ => format!("{expr} as num"),
            },
            TypeRef::Boolean => format!("{expr} as bool"),
            TypeRef::DateTime => format!("DateTime.parse({expr} as String)"),
            TypeRef::Binary => format!("({expr} as List<dynamic>).cast<int>()"),
            TypeRef::Any => expr.to_string(),
            TypeRef::Enum(_) => match enum_name {
                Some(e) => format!("{e}.fromJson({expr})"),
                None => format!("{expr} as String"),
            },
            TypeRef::Array(inner) => {
                if matches!(**inner, TypeRef::Any) {
                    format!("{expr} as List<dynamic>")
                } else {
                    format!(
                        "({expr} as List<dynamic>).map((e) => {}).toList()",
                        self.deserialize_expr(inner, "e", enum_name)
                    )
                }
            }
            TypeRef::Map(inner) => {
                if matches!(**inner, TypeRef::Any) {
                    format!("{expr} as Map<String, dynamic>")
                } else {
                    format!(
                        "({expr} as Map<String, dynamic>).map((k, v) => MapEntry(k, {}))",
                        self.deserialize_expr(inner, "v", enum_name)
                    )
                }
            }
            TypeRef::Named(name) => {
                let cls = self.class_name(name);
                match self.kind(name) {
                    Some(SchemaKind::Object { .. }) | Some(SchemaKind::Union { .. }) => {
                        format!("{cls}.fromJson({expr} as Map<String, dynamic>)")
                    }
                    Some(SchemaKind::UntaggedUnion { .. }) | Some(SchemaKind::Enum { .. }) => {
                        format!("{cls}.fromJson({expr})")
                    }
                    Some(SchemaKind::Array { item }) => {
                        self.deserialize_expr(&TypeRef::Array(Box::new(item.clone())), expr, None)
                    }
                    Some(SchemaKind::Map { value }) => {
                        self.deserialize_expr(&TypeRef::Map(Box::new(value.clone())), expr, None)
                    }
                    Some(SchemaKind::Alias { target }) => {
                        self.deserialize_expr(&TypeRef::Named(target.clone()), expr, None)
                    }
                    Some(SchemaKind::Primitive { type_ref }) => {
                        self.deserialize_expr(type_ref, expr, None)
                    }
                    // Mapped-away schema: the external type is assumed to follow
                    // the standard `fromJson(Map<String, dynamic>)` convention.
                    None => format!("{cls}.fromJson({expr} as Map<String, dynamic>)"),
                }
            }
        }
    }

    /// Dart expression rendering the non-null value `expr` of type `t` as a
    /// query/header/path wire value: a `String` for scalars, `List<String>`
    /// for arrays.
    fn wire_value_expr(
        &self,
        t: &TypeRef,
        expr: &str,
        enum_name: Option<&str>,
        needs: &mut Needs,
    ) -> String {
        match t {
            TypeRef::String => expr.to_string(),
            TypeRef::Integer { .. } | TypeRef::Number { .. } | TypeRef::Boolean => {
                format!("{expr}.toString()")
            }
            TypeRef::DateTime => format!("{expr}.toIso8601String()"),
            TypeRef::Enum(_) => match enum_name {
                Some(_) => format!("'${{{expr}.value}}'"),
                None => expr.to_string(),
            },
            TypeRef::Binary => {
                needs.convert = true;
                format!("base64Encode({expr})")
            }
            TypeRef::Any => format!("{expr}.toString()"),
            TypeRef::Array(inner) if matches!(**inner, TypeRef::String) => expr.to_string(),
            TypeRef::Array(inner) => format!(
                "{expr}.map((e) => {}).toList()",
                self.wire_scalar_expr(inner, "e", enum_name, needs)
            ),
            TypeRef::Map(_) => {
                needs.convert = true;
                format!("jsonEncode({})", self.to_json_expr(t, expr))
            }
            TypeRef::Named(name) => match self.kind(name) {
                Some(SchemaKind::Enum { .. }) => format!("'${{{expr}.value}}'"),
                Some(SchemaKind::Array {
                    item: TypeRef::String,
                }) => expr.to_string(),
                Some(SchemaKind::Array { item }) => format!(
                    "{expr}.map((e) => {}).toList()",
                    self.wire_scalar_expr(item, "e", None, needs)
                ),
                Some(SchemaKind::Alias { target }) => {
                    self.wire_value_expr(&TypeRef::Named(target.clone()), expr, None, needs)
                }
                Some(SchemaKind::Primitive { type_ref }) => {
                    self.wire_value_expr(type_ref, expr, None, needs)
                }
                _ => {
                    needs.convert = true;
                    format!("jsonEncode({})", self.to_json_expr(t, expr))
                }
            },
        }
    }

    /// Like `wire_value_expr` but always yields a single `String`
    /// (arrays are comma-joined).
    fn wire_scalar_expr(
        &self,
        t: &TypeRef,
        expr: &str,
        enum_name: Option<&str>,
        needs: &mut Needs,
    ) -> String {
        let v = self.wire_value_expr(t, expr, enum_name, needs);
        if self.is_array_like(t) {
            format!("{v}.join(',')")
        } else {
            v
        }
    }

    fn is_array_like(&self, t: &TypeRef) -> bool {
        match t {
            TypeRef::Array(_) => true,
            TypeRef::Named(name) => match self.kind(name) {
                Some(SchemaKind::Array { .. }) => true,
                Some(SchemaKind::Alias { target }) => {
                    self.is_array_like(&TypeRef::Named(target.clone()))
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn is_binary(&self, t: &TypeRef) -> bool {
        match t {
            TypeRef::Binary => true,
            TypeRef::Named(name) => match self.kind(name) {
                Some(SchemaKind::Primitive { type_ref }) => self.is_binary(type_ref),
                Some(SchemaKind::Alias { target }) => {
                    self.is_binary(&TypeRef::Named(target.clone()))
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Fields of the object schema behind `t`, if it is one.
    fn object_fields(&self, t: &TypeRef) -> Option<(&'a str, &'a [Field])> {
        let TypeRef::Named(name) = t else { return None };
        let schema = self.schema(name)?;
        match &schema.kind {
            SchemaKind::Object { fields } => Some((schema.name.as_str(), fields.as_slice())),
            SchemaKind::Alias { target } => self.object_fields(&TypeRef::Named(target.clone())),
            _ => None,
        }
    }
}

fn first_enum(t: &TypeRef) -> Option<&Vec<EnumValue>> {
    match t {
        TypeRef::Enum(values) => Some(values),
        TypeRef::Map(inner) | TypeRef::Array(inner) => first_enum(inner),
        _ => None,
    }
}

/// Imports a generated client file must add beyond its backend package.
#[derive(Default)]
struct Needs {
    convert: bool,
}

// ── Public entry point: models ────────────────────────────────────────────────

pub fn emit_models(
    api: &Api,
    mode: NullSafety,
    mappings: &MappingConfig,
    templates: &TemplateConfig,
) -> HashMap<String, String> {
    let ctx = Ctx::build(api, mode, mappings);
    let mut files = HashMap::new();

    if ctx.safe() {
        let utils = templates
            .verbatim("flap_utils.dart")
            .unwrap_or_else(emit_flap_utils);
        files.insert("flap_utils.dart".to_string(), utils);
    }

    let model_jinja = templates.jinja("model.dart");

    for schema in &api.schemas {
        if schema.internal || ctx.is_mapped(&schema.name) {
            continue;
        }
        let class_name = ctx.class_name(&schema.name);
        let filename = format!("{}.dart", to_snake_case(&class_name));

        let source = if let Some(verbatim) = templates.verbatim(&filename) {
            verbatim
        } else if let Some(jinja_src) = &model_jinja {
            match render_jinja(jinja_src, build_model_ctx(&ctx, schema, &class_name)) {
                Ok(rendered) => rendered,
                Err(e) => {
                    eprintln!("warning: model.dart.jinja error for `{}`: {e}", schema.name);
                    emit_schema(&ctx, schema, &class_name)
                }
            }
        } else {
            emit_schema(&ctx, schema, &class_name)
        };
        files.insert(filename, source);
    }

    for (name, values) in &ctx.synth_enums {
        let filename = format!("{}.dart", to_snake_case(name));
        let source = templates
            .verbatim(&filename)
            .unwrap_or_else(|| emit_enum(&ctx, name, values, None));
        files.insert(filename, source);
    }

    files
}

// ── Public entry point: client ────────────────────────────────────────────────

/// Returns `(filename, dart_source)`.
pub fn emit_client(
    api: &Api,
    mode: NullSafety,
    backend: ClientBackend,
    mappings: &MappingConfig,
    templates: &TemplateConfig,
) -> (String, String) {
    let ctx = Ctx::build(api, mode, mappings);
    let class_name = api_client_name(&api.title);
    let filename = format!("{}.dart", to_snake_case(&class_name));

    let builtin = || match backend {
        ClientBackend::Dio => emit_client_dio(&ctx, &class_name),
        ClientBackend::Http => emit_client_http(&ctx, &class_name),
    };

    let source = if let Some(verbatim) = templates.verbatim(&filename) {
        verbatim
    } else if let Some(jinja_src) = templates.jinja("client.dart") {
        match render_jinja(&jinja_src, build_client_ctx(&ctx, &class_name, backend)) {
            Ok(rendered) => rendered,
            Err(e) => {
                eprintln!("warning: client.dart.jinja render error: {e}");
                builtin()
            }
        }
    } else {
        builtin()
    };

    (filename, source)
}

/// `"Swagger Petstore"` → `SwaggerPetstoreClient`; `"Petstore 3.1"` → `Petstore31Client`.
fn api_client_name(title: &str) -> String {
    let pascal = to_pascal_case(title);
    let pascal = if pascal == "Value" {
        "Api".to_string()
    } else {
        pascal
    };
    format!("{pascal}Client")
}

// ── Template context builders ─────────────────────────────────────────────────

fn build_model_ctx(ctx: &Ctx, schema: &Schema, class_name: &str) -> ModelTemplateCtx {
    let kind = match &schema.kind {
        SchemaKind::Object { .. } => "object",
        SchemaKind::Array { .. } => "array",
        SchemaKind::Map { .. } => "map",
        SchemaKind::Union { .. } => "union",
        SchemaKind::UntaggedUnion { .. } => "untagged_union",
        SchemaKind::Alias { .. } => "alias",
        SchemaKind::Enum { .. } => "enum",
        SchemaKind::Primitive { .. } => "primitive",
    };
    let plans = match &schema.kind {
        SchemaKind::Object { fields } => plan_fields(ctx, &schema.name, fields, true),
        _ => vec![],
    };
    let fields: Vec<FieldTemplateCtx> = plans
        .iter()
        .map(|p| FieldTemplateCtx {
            spec_name: p.field.name.clone(),
            dart_name: p.dart_name.clone(),
            dart_type: p.declared_type.clone(),
            required: p.field.required,
            nullable: p.field.nullable,
            uses_optional_wrapper: p.optional_wrapper.is_some(),
            default_expr: p.default_expr.clone(),
            json_name: (p.dart_name != p.field.name).then(|| p.field.name.clone()),
        })
        .collect();
    let mut imports = BTreeSet::new();
    for p in &plans {
        ctx.collect_imports(
            &p.field.type_ref,
            p.enum_name.as_deref(),
            class_name,
            &mut imports,
        );
    }
    ModelTemplateCtx {
        class_name: class_name.to_string(),
        schema_name: schema.name.clone(),
        snake_name: to_snake_case(class_name),
        kind: kind.to_string(),
        has_optional_fields: plans.iter().any(|p| p.optional_wrapper.is_some()),
        fields,
        imports: imports.into_iter().collect(),
        extends: schema.extends.clone(),
        null_safety: if ctx.safe() { "safe" } else { "unsafe" }.to_string(),
    }
}

fn build_client_ctx(ctx: &Ctx, class_name: &str, backend: ClientBackend) -> ClientTemplateCtx {
    let credentials = ctx
        .api
        .security_schemes
        .iter()
        .map(|s| CredentialTemplateCtx {
            dart_param_name: credential_param_name(s),
            scheme_type: match &s.kind {
                SecuritySchemeKind::ApiKey { .. } => "apiKey",
                SecuritySchemeKind::HttpBasic => "httpBasic",
                SecuritySchemeKind::HttpBearer { .. } => "httpBearer",
                SecuritySchemeKind::OAuth2 { .. } => "oauth2",
                SecuritySchemeKind::OpenIdConnect { .. } => "openIdConnect",
            }
            .to_string(),
        })
        .collect();

    let operations = ctx
        .api
        .operations
        .iter()
        .enumerate()
        .map(|(i, op)| {
            let plan = plan_method(ctx, i, op, backend);
            OperationTemplateCtx {
                method: op.method.to_string(),
                path: op.path.clone(),
                method_name: plan.name.clone(),
                summary: op.summary.clone(),
                deprecated: op.deprecated,
                return_type: plan.return_type.clone(),
                parameters: plan
                    .params
                    .iter()
                    .map(|p| ParamTemplateCtx {
                        spec_name: p.spec_name.clone(),
                        dart_name: p.dart_name.clone(),
                        dart_type: p.dart_type.clone(),
                        location: p.location.to_string(),
                        required: p.required,
                    })
                    .collect(),
                has_body: plan.body.is_some(),
                body_type: plan.body.as_ref().map(|b| b.dart_type.clone()),
                body_required: plan.body.as_ref().is_some_and(|b| b.required),
                is_multipart: plan.body.as_ref().is_some_and(|b| b.body.is_multipart),
                body_content_type: plan.body.as_ref().map(|b| b.body.content_type.clone()),
            }
        })
        .collect();

    ClientTemplateCtx {
        class_name: class_name.to_string(),
        default_base_url: ctx.api.base_urls.first().cloned().unwrap_or_default(),
        base_urls: ctx.api.base_urls.clone(),
        operations,
        credentials,
        backend: backend.as_str().to_string(),
        null_safety: if ctx.safe() { "safe" } else { "unsafe" }.to_string(),
    }
}

// ── flap_utils.dart runtime ───────────────────────────────────────────────────

fn emit_flap_utils() -> String {
    let mut out = String::from(
        r#"// GENERATED by flap — do not edit by hand.
//
// `Optional<T?>` distinguishes "key absent" from "key present with value
// null" for PATCH-style request bodies. Fields declared `nullable: true`
// but not `required` use it.
import 'package:freezed_annotation/freezed_annotation.dart';

sealed class Optional<T> {
  const Optional();
  const factory Optional.present(T value) = OptionalPresent<T>;
  const factory Optional.absent() = OptionalAbsent<T>;

  /// `Optional.absent()` when [value] is null, otherwise `Optional.present(value)`.
  static Optional<T?> of<T>(T? value) =>
      value == null ? Optional<T?>.absent() : Optional<T?>.present(value);

  bool get isPresent => this is OptionalPresent<T>;
  bool get isAbsent => this is OptionalAbsent<T>;

  /// The wrapped value. Throws [StateError] when absent.
  T get value => switch (this) {
        OptionalPresent<T>(:final value) => value,
        OptionalAbsent<T>() =>
          throw StateError('Optional.value called on Optional.absent()'),
      };

  /// The wrapped value, or null when absent.
  T? get valueOrNull => switch (this) {
        OptionalPresent<T>(:final value) => value,
        OptionalAbsent<T>() => null,
      };

  @override
  String toString() => switch (this) {
        OptionalPresent<T>(:final value) => 'Optional.present($value)',
        OptionalAbsent<T>() => 'Optional.absent()',
      };
}

final class OptionalPresent<T> extends Optional<T> {
  const OptionalPresent(this.value);

  @override
  final T value;

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is OptionalPresent<T> && other.value == value);

  @override
  int get hashCode => Object.hash(OptionalPresent, value);
}

final class OptionalAbsent<T> extends Optional<T> {
  const OptionalAbsent();

  @override
  bool operator ==(Object other) => other is OptionalAbsent<T>;

  @override
  int get hashCode => (OptionalAbsent).hashCode;
}
"#,
    );

    // json_serializable only accepts non-generic converters whose declared
    // field type matches exactly, so one converter per supported scalar.
    for (suffix, ty) in [
        ("String", "String"),
        ("Int", "int"),
        ("Double", "double"),
        ("Num", "num"),
        ("Bool", "bool"),
    ] {
        wl!(out);
        wl!(out, "class Optional{suffix}Converter");
        wl!(
            out,
            "    implements JsonConverter<Optional<{ty}?>, Object?> {{"
        );
        wl!(out, "  const Optional{suffix}Converter();");
        wl!(out);
        wl!(out, "  @override");
        wl!(out, "  Optional<{ty}?> fromJson(Object? json) =>");
        if ty == "double" {
            wl!(
                out,
                "      Optional<{ty}?>.present((json as num?)?.toDouble());"
            );
        } else if ty == "int" {
            wl!(
                out,
                "      Optional<{ty}?>.present((json as num?)?.toInt());"
            );
        } else {
            wl!(out, "      Optional<{ty}?>.present(json as {ty}?);");
        }
        wl!(out);
        wl!(out, "  @override");
        wl!(
            out,
            "  Object? toJson(Optional<{ty}?> optional) => optional.valueOrNull;"
        );
        wl!(out, "}}");
    }
    out
}

fn optional_converter_for(t: &TypeRef) -> Option<&'static str> {
    match t {
        TypeRef::String => Some("OptionalStringConverter"),
        TypeRef::Integer { .. } => Some("OptionalIntConverter"),
        TypeRef::Number { format } => match format.as_deref() {
            Some("float" | "double") => Some("OptionalDoubleConverter"),
            _ => Some("OptionalNumConverter"),
        },
        TypeRef::Boolean => Some("OptionalBoolConverter"),
        _ => None,
    }
}

// ── Schema-shape dispatch ─────────────────────────────────────────────────────

fn emit_schema(ctx: &Ctx, schema: &Schema, class_name: &str) -> String {
    match &schema.kind {
        SchemaKind::Object { fields } => emit_freezed_class(ctx, schema, class_name, fields),
        SchemaKind::Array { item } => emit_typedef(
            ctx,
            class_name,
            item,
            &format!("List<{}>", ctx.dart_type(item, None)),
            &format!("Generated from OpenAPI array schema `{}`.", schema.name),
        ),
        SchemaKind::Map { value } => emit_typedef(
            ctx,
            class_name,
            value,
            &format!("Map<String, {}>", ctx.dart_type(value, None)),
            &format!(
                "Generated from OpenAPI map schema `{}`\n(object with `additionalProperties` and no fixed properties).",
                schema.name
            ),
        ),
        SchemaKind::Alias { target } => {
            let t = TypeRef::Named(target.clone());
            emit_typedef(
                ctx,
                class_name,
                &t,
                &ctx.dart_type(&t, None),
                &format!(
                    "Generated from OpenAPI $ref alias `{}` → `{target}`.",
                    schema.name
                ),
            )
        }
        SchemaKind::Primitive { type_ref } => emit_typedef(
            ctx,
            class_name,
            type_ref,
            &ctx.dart_type(type_ref, None),
            &format!("Generated from OpenAPI primitive schema `{}`.", schema.name),
        ),
        SchemaKind::Enum { values } => emit_enum(ctx, class_name, values, Some(&schema.name)),
        SchemaKind::Union {
            variants,
            discriminator,
            variant_tags,
        } => emit_freezed_union(
            ctx,
            schema,
            class_name,
            variants,
            discriminator,
            variant_tags,
        ),
        SchemaKind::UntaggedUnion { variants } => {
            emit_untagged_union(ctx, schema, class_name, variants)
        }
    }
}

fn emit_typedef(
    ctx: &Ctx,
    class_name: &str,
    inner: &TypeRef,
    dart_type: &str,
    comment: &str,
) -> String {
    let mut out = String::new();
    let mut imports = BTreeSet::new();
    ctx.collect_imports(inner, None, class_name, &mut imports);
    write_imports(&mut out, &imports);
    if !imports.is_empty() {
        wl!(out);
    }
    doc_comment(&mut out, "", comment);
    wl!(out, "typedef {class_name} = {dart_type};");
    out
}

// ── Field planning ────────────────────────────────────────────────────────────

/// Everything needed to emit one constructor parameter and its toJson line.
struct FieldPlan<'f> {
    field: &'f Field,
    dart_name: String,
    /// Synthesised enum class for inline `enum:` fields.
    enum_name: Option<String>,
    /// Base Dart type (never nullable).
    base_type: String,
    /// Type as it appears in the constructor signature (may include `?` or `Optional<>`).
    declared_type: String,
    /// Converter class when the `Optional<T?>` wrapper is used.
    optional_wrapper: Option<&'static str>,
    default_expr: Option<String>,
    /// Constructor parameter is `required`.
    is_required_param: bool,
    /// Dart-side value may be null (i.e. declared with `?`).
    dart_nullable: bool,
}

fn plan_fields<'f>(
    ctx: &Ctx,
    schema_name: &str,
    fields: &'f [Field],
    allow_optional_wrapper: bool,
) -> Vec<FieldPlan<'f>> {
    let mut taken: HashSet<String> = HashSet::new();
    fields
        .iter()
        .map(|field| {
            let dart_name = unique(&member_name(&field.name), &mut taken, "");
            let enum_name = ctx
                .field_enums
                .get(&(schema_name.to_string(), field.name.clone()))
                .cloned();
            let base_type = ctx.dart_type(&field.type_ref, enum_name.as_deref());
            let force_nullable = field.is_recursive && matches!(field.type_ref, TypeRef::Named(_));

            let default_expr = field
                .default_value
                .as_ref()
                .and_then(|d| default_expr(ctx, &field.type_ref, enum_name.as_deref(), d));

            let optional_wrapper = if ctx.safe()
                && allow_optional_wrapper
                && !field.required
                && field.nullable
                && !force_nullable
            {
                optional_converter_for(&field.type_ref)
            } else {
                None
            };

            let (declared_type, is_required_param, dart_nullable) = if optional_wrapper.is_some() {
                (format!("Optional<{base_type}?>"), false, false)
            } else if force_nullable {
                (ctx.nullable(&base_type), false, true)
            } else if field.required && !field.nullable {
                (base_type.clone(), true, false)
            } else if field.required {
                (ctx.nullable(&base_type), true, true)
            } else if default_expr.is_some() && !field.nullable {
                (base_type.clone(), false, false)
            } else {
                (ctx.nullable(&base_type), false, true)
            };

            FieldPlan {
                field,
                dart_name,
                enum_name,
                base_type,
                declared_type,
                optional_wrapper,
                default_expr: if optional_wrapper.is_some() {
                    None
                } else {
                    default_expr
                },
                is_required_param,
                dart_nullable,
            }
        })
        .collect()
}

fn default_expr(
    ctx: &Ctx,
    t: &TypeRef,
    enum_name: Option<&str>,
    d: &DefaultValue,
) -> Option<String> {
    match t {
        TypeRef::Enum(values) => {
            let e = enum_name?;
            let target = match d {
                DefaultValue::String(s) => EnumValue::Str(s.clone()),
                DefaultValue::Integer(n) => EnumValue::Int(*n),
                _ => return None,
            };
            let cases = enum_case_names(values);
            values
                .iter()
                .position(|v| *v == target)
                .map(|i| format!("{e}.{}", cases[i]))
        }
        TypeRef::Named(name) => match ctx.kind(name) {
            Some(SchemaKind::Enum { values }) => {
                let cls = ctx.class_name(name);
                default_expr(ctx, &TypeRef::Enum(values.clone()), Some(&cls), d)
            }
            Some(SchemaKind::Primitive { type_ref }) => default_expr(ctx, type_ref, None, d),
            Some(SchemaKind::Alias { target }) => {
                default_expr(ctx, &TypeRef::Named(target.clone()), None, d)
            }
            _ => None,
        },
        TypeRef::String => match d {
            DefaultValue::String(s) => Some(dart_str(s)),
            _ => None,
        },
        TypeRef::Integer { .. } => match d {
            DefaultValue::Integer(n) => Some(n.to_string()),
            DefaultValue::Number(n) if n.fract() == 0.0 => Some(format!("{}", *n as i64)),
            _ => None,
        },
        TypeRef::Number { format } => {
            let is_double = matches!(format.as_deref(), Some("float" | "double"));
            match d {
                DefaultValue::Integer(n) => Some(if is_double {
                    format!("{n}.0")
                } else {
                    n.to_string()
                }),
                DefaultValue::Number(n) => Some(if n.fract() == 0.0 {
                    format!("{n:.1}")
                } else {
                    n.to_string()
                }),
                _ => None,
            }
        }
        TypeRef::Boolean => match d {
            DefaultValue::Boolean(b) => Some(b.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// One constructor-parameter line (with annotations) for a field plan.
fn emit_field_line(ctx: &Ctx, p: &FieldPlan) -> String {
    let mut json_key_args: Vec<String> = Vec::new();
    if p.dart_name != p.field.name {
        json_key_args.push(format!("name: {}", dart_str(&p.field.name)));
    }
    if p.optional_wrapper.is_none() && !p.is_required_param && p.default_expr.is_none() {
        json_key_args.push("includeIfNull: false".to_string());
    }
    if let Some(e) = ctx.enum_class_in(&p.field.type_ref, p.enum_name.as_deref()) {
        json_key_args.push(format!("unknownEnumValue: {e}.unknown"));
    }

    let mut annotations: Vec<String> = Vec::new();
    if !json_key_args.is_empty() {
        annotations.push(format!("@JsonKey({})", json_key_args.join(", ")));
    }
    if let Some(u) = ctx.untagged_union_in(&p.field.type_ref) {
        annotations.push(format!("@{u}Converter()"));
    }
    if let Some(conv) = p.optional_wrapper {
        annotations.push(format!("@{conv}()"));
        annotations.push(format!("@Default(Optional<{}?>.absent())", p.base_type));
    } else if let Some(d) = &p.default_expr {
        annotations.push(format!("@Default({d})"));
    }

    let mut line = String::from("    ");
    for a in &annotations {
        line.push_str(a);
        line.push(' ');
    }
    if p.is_required_param {
        line.push_str(ctx.required_kw());
    }
    w!(line, "{} {},", p.declared_type, p.dart_name);
    line
}

/// A `'json': value` entry for a hand-written toJson.
fn emit_to_json_entry(ctx: &Ctx, p: &FieldPlan) -> String {
    let key = dart_str(&p.field.name);
    let name = &p.dart_name;
    let t = &p.field.type_ref;
    if p.optional_wrapper.is_some() {
        return format!("        if ({name}.isPresent) {key}: {name}.value,");
    }
    if p.declared_type == "dynamic" {
        return if p.is_required_param {
            format!("        {key}: {name},")
        } else {
            format!("        if ({name} != null) {key}: {name},")
        };
    }
    let passthrough = ctx.json_passthrough(t);
    if p.dart_nullable {
        if p.is_required_param {
            if passthrough {
                format!("        {key}: {name},")
            } else {
                format!(
                    "        {key}: {name} == null ? null : {},",
                    ctx.to_json_expr(t, &format!("{name}{}", ctx.bang()))
                )
            }
        } else if passthrough {
            format!("        if ({name} != null) {key}: {name},")
        } else {
            format!(
                "        if ({name} != null) {key}: {},",
                ctx.to_json_expr(t, &format!("{name}{}", ctx.bang()))
            )
        }
    } else {
        format!("        {key}: {},", ctx.to_json_expr(t, name))
    }
}

// ── @freezed class ────────────────────────────────────────────────────────────

fn emit_freezed_class(ctx: &Ctx, schema: &Schema, class_name: &str, fields: &[Field]) -> String {
    let snake = to_snake_case(class_name);
    let plans = plan_fields(ctx, &schema.name, fields, true);
    let custom_to_json = plans.iter().any(|p| p.optional_wrapper.is_some());
    let mut out = String::new();

    let mut imports = BTreeSet::new();
    imports.insert("import 'package:freezed_annotation/freezed_annotation.dart';".to_string());
    if !ctx.safe() {
        imports.insert("import 'package:meta/meta.dart';".to_string());
    }
    if custom_to_json {
        imports.insert("import 'flap_utils.dart';".to_string());
    }
    for p in &plans {
        ctx.collect_imports(
            &p.field.type_ref,
            p.enum_name.as_deref(),
            class_name,
            &mut imports,
        );
    }
    write_imports(&mut out, &imports);
    wl!(out);
    wl!(out, "part '{snake}.freezed.dart';");
    wl!(out, "part '{snake}.g.dart';");
    wl!(out);
    doc_comment(
        &mut out,
        "",
        &format!("Generated from OpenAPI schema `{}`.", schema.name),
    );
    if let Some(parent) = &schema.extends {
        doc_comment(
            &mut out,
            "",
            &format!("Extends `{parent}` (flattened `allOf`)."),
        );
    }
    if custom_to_json {
        wl!(out, "@Freezed(toJson: false)");
    } else {
        wl!(out, "@freezed");
    }
    let class_kw = if ctx.safe() {
        "abstract class"
    } else {
        "class"
    };
    wl!(out, "{class_kw} {class_name} with _${class_name} {{");
    if custom_to_json {
        wl!(out, "  const {class_name}._();");
        wl!(out);
    }
    if plans.is_empty() {
        wl!(out, "  const factory {class_name}() = _{class_name};");
    } else {
        wl!(out, "  const factory {class_name}({{");
        for p in &plans {
            wl!(out, "{}", emit_field_line(ctx, p));
        }
        wl!(out, "  }}) = _{class_name};");
    }
    wl!(out);
    wl!(
        out,
        "  factory {class_name}.fromJson(Map<String, dynamic> json) =>"
    );
    wl!(out, "      _${class_name}FromJson(json);");
    if custom_to_json {
        wl!(out);
        wl!(
            out,
            "  /// Serialises this object, omitting `Optional.absent()` fields."
        );
        wl!(
            out,
            "  Map<String, dynamic> toJson() => <String, dynamic>{{"
        );
        for p in &plans {
            wl!(out, "{}", emit_to_json_entry(ctx, p));
        }
        wl!(out, "      }};");
    }
    wl!(out, "}}");
    out
}

// ── @Freezed discriminated union ──────────────────────────────────────────────

fn emit_freezed_union(
    ctx: &Ctx,
    schema: &Schema,
    class_name: &str,
    variants: &[TypeRef],
    discriminator: &str,
    variant_tags: &[String],
) -> String {
    let snake = to_snake_case(class_name);
    let names = &ctx.union_variant_names[&schema.name];
    let mut out = String::new();

    // Per variant: rendered constructor-parameter lines. The discriminator
    // property is dropped — freezed writes/reads it through `unionKey`.
    let mut imports = BTreeSet::new();
    imports.insert("import 'package:freezed_annotation/freezed_annotation.dart';".to_string());
    if !ctx.safe() {
        imports.insert("import 'package:meta/meta.dart';".to_string());
    }
    let mut variant_lines: Vec<Vec<String>> = Vec::with_capacity(variants.len());
    for v in variants {
        let TypeRef::Named(variant_name) = v else {
            variant_lines.push(vec![]);
            continue;
        };
        let fields: Vec<Field> = match ctx.kind(variant_name) {
            Some(SchemaKind::Object { fields }) => fields
                .iter()
                .filter(|f| f.name != discriminator)
                .cloned()
                .collect(),
            _ => vec![],
        };
        let plans = plan_fields(ctx, variant_name, &fields, false);
        for p in &plans {
            ctx.collect_imports(
                &p.field.type_ref,
                p.enum_name.as_deref(),
                class_name,
                &mut imports,
            );
        }
        variant_lines.push(plans.iter().map(|p| emit_field_line(ctx, p)).collect());
    }

    write_imports(&mut out, &imports);
    wl!(out);
    wl!(out, "part '{snake}.freezed.dart';");
    wl!(out, "part '{snake}.g.dart';");
    wl!(out);
    doc_comment(
        &mut out,
        "",
        &format!(
            "Generated from OpenAPI schema `{}` — a `oneOf` discriminated on `{discriminator}`.",
            schema.name
        ),
    );
    wl!(out, "@Freezed(unionKey: {})", dart_str(discriminator));
    let class_kw = if ctx.safe() {
        "sealed class"
    } else {
        "abstract class"
    };
    wl!(out, "{class_kw} {class_name} with _${class_name} {{");

    for (i, (lines, tag)) in variant_lines.iter().zip(variant_tags.iter()).enumerate() {
        let (factory, variant_cls) = &names[i];
        if factory != tag {
            wl!(out, "  @FreezedUnionValue({})", dart_str(tag));
        }
        if lines.is_empty() {
            wl!(
                out,
                "  const factory {class_name}.{factory}() = {variant_cls};"
            );
        } else {
            wl!(out, "  const factory {class_name}.{factory}({{");
            for l in lines {
                wl!(out, "{l}");
            }
            wl!(out, "  }}) = {variant_cls};");
        }
        wl!(out);
    }

    wl!(
        out,
        "  factory {class_name}.fromJson(Map<String, dynamic> json) =>"
    );
    wl!(out, "      _${class_name}FromJson(json);");
    wl!(out, "}}");
    out
}

// ── Untagged union ────────────────────────────────────────────────────────────

struct UntaggedVariant {
    factory: String,
    class_name: String,
    /// Dart type of the wrapped value.
    dart_type: String,
    type_ref: TypeRef,
}

fn untagged_variants(ctx: &Ctx, class_name: &str, variants: &[TypeRef]) -> Vec<UntaggedVariant> {
    let mut taken: HashSet<String> = HashSet::new();
    taken.insert("fromJson".into());
    taken.insert("toJson".into());
    variants
        .iter()
        .map(|v| {
            // Internal single-field wrappers stand in for primitive variants.
            let inner: TypeRef = match v {
                TypeRef::Named(name) => match ctx.schema(name) {
                    Some(s) if s.internal => match &s.kind {
                        SchemaKind::Object { fields } if fields.len() == 1 => {
                            fields[0].type_ref.clone()
                        }
                        _ => v.clone(),
                    },
                    _ => v.clone(),
                },
                other => other.clone(),
            };
            let dart_type = ctx.dart_type(&inner, None);
            let hint = match &inner {
                TypeRef::Named(n) => to_camel_case(n),
                TypeRef::Array(_) => "list".to_string(),
                TypeRef::Map(_) => "map".to_string(),
                TypeRef::Any => "any".to_string(),
                TypeRef::Binary => "bytes".to_string(),
                TypeRef::DateTime => "dateTime".to_string(),
                TypeRef::Enum(_) => "text".to_string(),
                other => to_camel_case(&ctx.dart_type(other, None)),
            };
            let hint = if matches!(inner, TypeRef::Named(_)) {
                hint
            } else {
                format!("{hint}Value")
            };
            let factory = unique(&member_name(&hint), &mut taken, "Variant");
            UntaggedVariant {
                class_name: format!("{class_name}{}", to_pascal_case(&factory)),
                factory,
                dart_type,
                type_ref: inner,
            }
        })
        .collect()
}

/// `(type test, converted expression)` for parsing a JSON value into a
/// primitive-ish untagged variant. `None` for object-like variants.
fn primitive_probe(ctx: &Ctx, t: &TypeRef) -> Option<(String, String)> {
    match t {
        TypeRef::String => Some(("json is String".into(), "json".into())),
        TypeRef::Integer { .. } => Some(("json is int".into(), "json".into())),
        TypeRef::Number { format } => match format.as_deref() {
            Some("float" | "double") => Some(("json is num".into(), "json.toDouble()".into())),
            _ => Some(("json is num".into(), "json".into())),
        },
        TypeRef::Boolean => Some(("json is bool".into(), "json".into())),
        TypeRef::DateTime => Some((
            "json is String && DateTime.tryParse(json) != null".into(),
            "DateTime.parse(json)".into(),
        )),
        TypeRef::Any => Some(("true".into(), "json".into())),
        TypeRef::Binary | TypeRef::Array(_) => Some((
            "json is List<dynamic>".into(),
            ctx.deserialize_expr(t, "json", None),
        )),
        TypeRef::Map(_) => Some((
            "json is Map<String, dynamic>".into(),
            ctx.deserialize_expr(t, "json", None),
        )),
        TypeRef::Enum(_) => Some(("json is String".into(), "json".into())),
        TypeRef::Named(name) => match ctx.kind(name) {
            Some(SchemaKind::Array { .. })
            | Some(SchemaKind::Map { .. })
            | Some(SchemaKind::Primitive { .. }) => {
                let probe = if matches!(ctx.kind(name), Some(SchemaKind::Array { .. })) {
                    "json is List<dynamic>"
                } else if matches!(ctx.kind(name), Some(SchemaKind::Map { .. })) {
                    "json is Map<String, dynamic>"
                } else {
                    "true"
                };
                Some((probe.into(), ctx.deserialize_expr(t, "json", None)))
            }
            Some(SchemaKind::Enum { .. }) => Some((
                format!(
                    "{}.fromJson(json) != {}.unknown",
                    ctx.class_name(name),
                    ctx.class_name(name)
                ),
                format!("{}.fromJson(json)", ctx.class_name(name)),
            )),
            _ => None,
        },
    }
}

fn emit_untagged_union(
    ctx: &Ctx,
    schema: &Schema,
    class_name: &str,
    variants: &[TypeRef],
) -> String {
    let vs = untagged_variants(ctx, class_name, variants);
    let obj = if ctx.safe() { "Object?" } else { "Object" };
    let mut out = String::new();

    let mut imports = BTreeSet::new();
    imports.insert("import 'package:freezed_annotation/freezed_annotation.dart';".to_string());
    for v in &vs {
        ctx.collect_imports(&v.type_ref, None, class_name, &mut imports);
    }
    write_imports(&mut out, &imports);
    wl!(out);
    doc_comment(
        &mut out,
        "",
        &format!(
            "Generated from OpenAPI schema `{}` — an untagged `anyOf`/`oneOf`.\n\nDeserialisation tries each variant in declaration order.",
            schema.name
        ),
    );
    let class_kw = if ctx.safe() {
        "sealed class"
    } else {
        "abstract class"
    };
    wl!(out, "{class_kw} {class_name} {{");
    wl!(out, "  const {class_name}._();");
    wl!(out);
    for v in &vs {
        wl!(
            out,
            "  const factory {class_name}.{}({} value) = {};",
            v.factory,
            v.dart_type,
            v.class_name
        );
    }
    wl!(out);
    wl!(out, "  factory {class_name}.fromJson({obj} json) {{");
    for v in &vs {
        match primitive_probe(ctx, &v.type_ref) {
            Some((test, expr)) => {
                if test == "true" {
                    wl!(out, "    return {class_name}.{}({expr});", v.factory);
                } else {
                    wl!(
                        out,
                        "    if ({test}) return {class_name}.{}({expr});",
                        v.factory
                    );
                }
            }
            None => {
                wl!(out, "    if (json is Map<String, dynamic>) {{");
                wl!(out, "      try {{");
                wl!(
                    out,
                    "        return {class_name}.{}({}.fromJson(json));",
                    v.factory,
                    v.dart_type
                );
                wl!(out, "      }} catch (_) {{}}");
                wl!(out, "    }}");
            }
        }
    }
    if !vs
        .iter()
        .any(|v| matches!(primitive_probe(ctx, &v.type_ref), Some((t, _)) if t == "true"))
    {
        wl!(
            out,
            "    throw ArgumentError.value(json, 'json', 'Cannot deserialize into {class_name}');"
        );
    }
    wl!(out, "  }}");
    wl!(out);
    wl!(out, "  {obj} toJson();");
    wl!(out, "}}");

    for v in &vs {
        wl!(out);
        let final_kw = if ctx.safe() { "final class" } else { "class" };
        wl!(out, "{final_kw} {} extends {class_name} {{", v.class_name);
        wl!(out, "  const {}(this.value) : super._();", v.class_name);
        wl!(out);
        wl!(out, "  final {} value;", v.dart_type);
        wl!(out);
        wl!(out, "  @override");
        wl!(
            out,
            "  {obj} toJson() => {};",
            ctx.to_json_expr(&v.type_ref, "value")
        );
        wl!(out);
        wl!(out, "  @override");
        wl!(out, "  bool operator ==(Object other) =>");
        wl!(
            out,
            "      identical(this, other) || (other is {} && other.value == value);",
            v.class_name
        );
        wl!(out);
        wl!(out, "  @override");
        wl!(
            out,
            "  int get hashCode => Object.hash({}, value);",
            v.class_name
        );
        wl!(out);
        wl!(out, "  @override");
        wl!(
            out,
            "  String toString() => '{class_name}.{}($value)';",
            v.factory
        );
        wl!(out, "}}");
    }

    wl!(out);
    wl!(
        out,
        "/// `JsonConverter` for fields of type [{class_name}]."
    );
    wl!(
        out,
        "class {class_name}Converter implements JsonConverter<{class_name}, {obj}> {{"
    );
    wl!(out, "  const {class_name}Converter();");
    wl!(out);
    wl!(out, "  @override");
    wl!(
        out,
        "  {class_name} fromJson({obj} json) => {class_name}.fromJson(json);"
    );
    wl!(out);
    wl!(out, "  @override");
    wl!(
        out,
        "  {obj} toJson({class_name} object) => object.toJson();"
    );
    wl!(out, "}}");
    out
}

// ── Enums ─────────────────────────────────────────────────────────────────────

/// Dart constant names for enum values, unique and keyword-safe. The
/// `unknown` sentinel is reserved.
fn enum_case_names(values: &[EnumValue]) -> Vec<String> {
    let mut reserved: HashSet<String> = HashSet::new();
    reserved.insert("unknown".to_string());
    for r in RESERVED_ENUM_MEMBERS {
        reserved.insert((*r).to_string());
    }
    let mut taken: HashSet<String> = reserved.clone();
    values
        .iter()
        .map(|v| {
            let mut base = match v {
                EnumValue::Str(s) => to_camel_case(s),
                EnumValue::Int(n) if *n < 0 => format!("vMinus{}", -n),
                EnumValue::Int(n) => format!("v{n}"),
            };
            if DART_RESERVED_KEYWORDS.contains(&base.as_str()) || reserved.contains(&base) {
                base.push_str("Value");
            }
            unique(&base, &mut taken, "")
        })
        .collect()
}

fn enum_value_literal(v: &EnumValue) -> String {
    match v {
        EnumValue::Str(s) => dart_str(s),
        EnumValue::Int(n) => n.to_string(),
    }
}

fn emit_enum(ctx: &Ctx, name: &str, values: &[EnumValue], schema_name: Option<&str>) -> String {
    let cases = enum_case_names(values);
    let all_str = values.iter().all(|v| matches!(v, EnumValue::Str(_)));
    let all_int = values.iter().all(|v| matches!(v, EnumValue::Int(_)));
    let value_type = if all_str {
        "String"
    } else if all_int {
        "int"
    } else {
        "Object"
    };
    let mut out = String::new();
    wl!(
        out,
        "import 'package:freezed_annotation/freezed_annotation.dart';"
    );
    wl!(out);
    match schema_name {
        Some(s) => doc_comment(
            &mut out,
            "",
            &format!("Generated from OpenAPI enum schema `{s}`."),
        ),
        None => doc_comment(&mut out, "", "Generated from an inline OpenAPI `enum`."),
    }
    doc_comment(
        &mut out,
        "",
        "\nValues not known at generation time deserialise to [unknown].",
    );

    if ctx.safe() {
        wl!(out, "@JsonEnum(valueField: 'value')");
        wl!(out, "enum {name} {{");
        for (v, case) in values.iter().zip(&cases) {
            wl!(out, "  {case}({}),", enum_value_literal(v));
        }
        wl!(out, "  unknown(null);");
        wl!(out);
        wl!(out, "  const {name}(this.value);");
        wl!(out);
        wl!(out, "  /// The wire value, or `null` for [unknown].");
        wl!(out, "  final {value_type}? value;");
        wl!(out);
        wl!(
            out,
            "  static {name} fromJson(Object? json) => values.firstWhere("
        );
        wl!(out, "        (e) => e.value == json,");
        wl!(out, "        orElse: () => {name}.unknown,");
        wl!(out, "      );");
        wl!(out);
        wl!(out, "  Object? toJson() => value;");
        wl!(out, "}}");
    } else {
        // Legacy dialect: no enhanced enums. Classic enum + extension.
        wl!(out, "enum {name} {{");
        for (v, case) in values.iter().zip(&cases) {
            wl!(out, "  @JsonValue({})", enum_value_literal(v));
            wl!(out, "  {case},");
        }
        wl!(out, "  @JsonValue(null)");
        wl!(out, "  unknown,");
        wl!(out, "}}");
        wl!(out);
        wl!(out, "extension {name}Flap on {name} {{");
        wl!(out, "  /// The wire value, or `null` for [{name}.unknown].");
        wl!(out, "  {value_type} get value {{");
        wl!(out, "    switch (this) {{");
        for (v, case) in values.iter().zip(&cases) {
            wl!(out, "      case {name}.{case}:");
            wl!(out, "        return {};", enum_value_literal(v));
        }
        wl!(out, "      default:");
        wl!(out, "        return null;");
        wl!(out, "    }}");
        wl!(out, "  }}");
        wl!(out);
        wl!(out, "  Object toJson() => value;");
        wl!(out);
        wl!(
            out,
            "  static {name} fromJson(Object json) => {name}.values.firstWhere("
        );
        wl!(out, "        (e) => e.value == json,");
        wl!(out, "        orElse: () => {name}.unknown,");
        wl!(out, "      );");
        wl!(out, "}}");
    }
    out
}

// ── Client planning (shared by both backends) ─────────────────────────────────

struct ParamPlan<'o> {
    spec_name: String,
    dart_name: String,
    location: ParameterLocation,
    type_ref: &'o TypeRef,
    enum_name: Option<String>,
    /// Non-null Dart type.
    dart_type: String,
    required: bool,
}

struct BodyPlan<'o> {
    body: &'o RequestBody,
    dart_type: String,
    required: bool,
}

struct MethodPlan<'o> {
    name: String,
    op: &'o Operation,
    params: Vec<ParamPlan<'o>>,
    body: Option<BodyPlan<'o>>,
    success: Option<&'o Response>,
    response_enum: Option<String>,
    return_type: String,
    /// Dart string-literal body for the path, e.g. `/pets/${Uri.encodeComponent(petId)}`.
    path_expr: String,
    needs: Needs,
}

fn success_response(responses: &[Response]) -> Option<&Response> {
    responses
        .iter()
        .find(|r| matches!(r.status_code.parse::<u16>(), Ok(c) if (200..300).contains(&c)))
        .or_else(|| {
            responses
                .iter()
                .find(|r| r.status_code == "2XX" || r.status_code == "2xx")
        })
}

fn plan_method<'o>(
    ctx: &Ctx,
    index: usize,
    op: &'o Operation,
    backend: ClientBackend,
) -> MethodPlan<'o> {
    let mut taken: HashSet<String> = HashSet::new();
    if op.request_body.is_some() {
        taken.insert("body".to_string());
    }
    if backend == ClientBackend::Dio {
        taken.insert("cancelToken".to_string());
    }
    // Names used inside method bodies.
    for reserved in [
        "queryParameters",
        "headers",
        "cookies",
        "response",
        "request",
        "uri",
        "data",
        "formData",
    ] {
        taken.insert(reserved.to_string());
    }

    let mut params: Vec<ParamPlan> = op
        .parameters
        .iter()
        .map(|p| {
            let enum_name = ctx
                .param_enums
                .get(&(index, p.name.clone(), p.location))
                .cloned();
            let base = member_name(&p.name);
            let dart_name = if taken.contains(&base) {
                unique(
                    &format!("{base}{}", to_pascal_case(p.location.as_str())),
                    &mut taken,
                    "",
                )
            } else {
                unique(&base, &mut taken, "")
            };
            ParamPlan {
                spec_name: p.name.clone(),
                dart_name,
                location: p.location,
                type_ref: &p.type_ref,
                enum_name: enum_name.clone(),
                dart_type: ctx.dart_type(&p.type_ref, enum_name.as_deref()),
                required: p.required,
            }
        })
        .collect();
    // Deterministic signature order: required first, both groups alphabetical.
    params.sort_by(|a, b| {
        b.required
            .cmp(&a.required)
            .then(a.dart_name.cmp(&b.dart_name))
    });

    let body = op.request_body.as_ref().map(|rb| {
        let enum_name = ctx.body_enums.get(&index).cloned();
        BodyPlan {
            body: rb,
            dart_type: ctx.dart_type(&rb.schema_ref, enum_name.as_deref()),
            required: rb.required,
        }
    });

    let success = success_response(&op.responses);
    let response_enum = success.and_then(|r| {
        ctx.response_enums
            .get(&(index, r.status_code.clone()))
            .cloned()
    });
    let return_type = success_return_type(ctx, success, response_enum.as_deref());

    let mut needs = Needs::default();
    let mut path_expr = op.path.clone();
    for p in params
        .iter()
        .filter(|p| p.location == ParameterLocation::Path)
    {
        let value =
            ctx.wire_scalar_expr(p.type_ref, &p.dart_name, p.enum_name.as_deref(), &mut needs);
        path_expr = path_expr.replace(
            &format!("{{{}}}", p.spec_name),
            &format!("${{Uri.encodeComponent({value})}}"),
        );
    }
    // Escape the literal parts of the path (outside our interpolations).
    let path_expr = escape_path_literal(&path_expr);

    MethodPlan {
        name: ctx.method_names[index].clone(),
        op,
        params,
        body,
        success,
        response_enum,
        return_type,
        path_expr,
        needs,
    }
}

/// Escape `'`, `\` and stray `$` in a path template while leaving the
/// `${Uri.encodeComponent(...)}` interpolations intact.
fn escape_path_literal(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(idx) = rest.find("${Uri.encodeComponent(") {
        out.push_str(&escape_literal_fragment(&rest[..idx]));
        let after = &rest[idx..];
        // Find the matching close: our interpolations end with ")}".
        let end = after.find(")}").map(|e| e + 2).unwrap_or(after.len());
        out.push_str(&after[..end]);
        rest = &after[end..];
    }
    out.push_str(&escape_literal_fragment(rest));
    out
}

fn escape_literal_fragment(s: &str) -> String {
    let lit = dart_str(s);
    lit[1..lit.len() - 1].to_string()
}

fn success_return_type(
    ctx: &Ctx,
    success: Option<&Response>,
    response_enum: Option<&str>,
) -> String {
    let Some(resp) = success else {
        return "void".into();
    };
    let body_type = resp
        .schema_ref
        .as_ref()
        .map(|t| ctx.dart_type(t, response_enum));

    if !ctx.safe() || resp.headers.is_empty() {
        return body_type.unwrap_or_else(|| "void".into());
    }

    let mut fields: Vec<String> = Vec::new();
    if let Some(bt) = body_type {
        fields.push(format!("{bt} body"));
    }
    for hdr in &resp.headers {
        let dart_type = ctx.dart_type(&hdr.type_ref, None);
        let dart_name = header_var_name(&hdr.name);
        if hdr.required {
            fields.push(format!("{dart_type} {dart_name}"));
        } else {
            fields.push(format!("{} {dart_name}", ctx.nullable(&dart_type)));
        }
    }
    format!("({{{}}})", fields.join(", "))
}

fn header_var_name(header: &str) -> String {
    let name = member_name(header);
    if name == "body" {
        "bodyHeader".to_string()
    } else {
        name
    }
}

/// `int.parse(raw)`-style conversion of a header string into `t`.
fn header_parse_expr(t: &TypeRef, raw: &str) -> String {
    match t {
        TypeRef::Integer { .. } => format!("int.parse({raw})"),
        TypeRef::Number { format } => match format.as_deref() {
            Some("float" | "double") => format!("double.parse({raw})"),
            _ => format!("num.parse({raw})"),
        },
        TypeRef::Boolean => format!("({raw} == 'true')"),
        TypeRef::DateTime => format!("DateTime.parse({raw})"),
        TypeRef::Array(inner) => format!(
            "{raw}.split(',').map((e) => {}).toList()",
            header_parse_expr(inner, "e.trim()")
        ),
        TypeRef::Enum(_) => raw.to_string(),
        _ => raw.to_string(),
    }
}

fn emit_header_bindings(
    ctx: &Ctx,
    out: &mut String,
    resp: &Response,
    raw_expr: impl Fn(&str) -> String,
) {
    for hdr in &resp.headers {
        let var = header_var_name(&hdr.name);
        let raw = raw_expr(&hdr.name.to_lowercase());
        if hdr.required {
            wl!(
                out,
                "    final {var} = {};",
                header_parse_expr(&hdr.type_ref, &format!("{raw}{}", ctx.bang()))
            );
        } else {
            wl!(out, "    final {var}Raw = {raw};");
            if matches!(hdr.type_ref, TypeRef::String) {
                wl!(out, "    final {var} = {var}Raw;");
            } else {
                wl!(
                    out,
                    "    final {var} = {var}Raw == null ? null : {};",
                    header_parse_expr(&hdr.type_ref, &format!("{var}Raw"))
                );
            }
        }
    }
}

/// `typed` means `data_expr` already has the method's body type (e.g.
/// `response.bodyBytes`), so no conversion is needed.
fn emit_return_statement(
    ctx: &Ctx,
    out: &mut String,
    plan: &MethodPlan,
    data_expr: &str,
    typed: bool,
) {
    let Some(resp) = plan.success else { return };
    let body_expr = resp.schema_ref.as_ref().map(|t| {
        if typed {
            data_expr.to_string()
        } else {
            ctx.deserialize_expr(t, data_expr, plan.response_enum.as_deref())
        }
    });
    if !ctx.safe() || resp.headers.is_empty() {
        if let Some(b) = body_expr {
            wl!(out, "    return {b};");
        }
        return;
    }
    let mut fields: Vec<String> = Vec::new();
    if let Some(b) = body_expr {
        fields.push(format!("body: {b}"));
    }
    for hdr in &resp.headers {
        let var = header_var_name(&hdr.name);
        fields.push(format!("{var}: {var}"));
    }
    wl!(out, "    return ({});", fields.join(", "));
}

fn emit_method_doc(out: &mut String, plan: &MethodPlan) {
    if let Some(summary) = &plan.op.summary {
        doc_comment(out, "  ", summary);
        wl!(out, "  ///");
    }
    wl!(out, "  /// `{} {}`", plan.op.method, plan.op.path);
    if plan.op.deprecated {
        wl!(
            out,
            "  @Deprecated('This operation is marked deprecated in the API specification.')"
        );
    }
}

/// Signature parameter lines shared by both backends.
fn emit_signature_params(ctx: &Ctx, out: &mut String, plan: &MethodPlan) {
    for p in plan.params.iter().filter(|p| p.required) {
        wl!(
            out,
            "    {}{} {},",
            ctx.required_kw(),
            p.dart_type,
            p.dart_name
        );
    }
    if let Some(b) = &plan.body {
        if b.required {
            wl!(out, "    {}{} body,", ctx.required_kw(), b.dart_type);
        } else {
            wl!(out, "    {} body,", ctx.nullable(&b.dart_type));
        }
    }
    for p in plan.params.iter().filter(|p| !p.required) {
        wl!(out, "    {} {},", ctx.nullable(&p.dart_type), p.dart_name);
    }
}

/// `final queryParameters = <String, dynamic>{...};` — values are `String`
/// or `List<String>`. Returns false when there are no query params at all.
fn emit_query_map(
    ctx: &Ctx,
    out: &mut String,
    plan: &MethodPlan,
    needs: &mut Needs,
    extra_entries: &[String],
) -> bool {
    let qs: Vec<&ParamPlan> = plan
        .params
        .iter()
        .filter(|p| p.location == ParameterLocation::Query)
        .collect();
    if qs.is_empty() && extra_entries.is_empty() {
        return false;
    }
    wl!(out, "    final queryParameters = <String, dynamic>{{");
    for e in extra_entries {
        wl!(out, "      {e}");
    }
    for p in &qs {
        let key = dart_str(&p.spec_name);
        if p.required {
            wl!(
                out,
                "      {key}: {},",
                ctx.wire_value_expr(p.type_ref, &p.dart_name, p.enum_name.as_deref(), needs)
            );
        } else {
            let v = ctx.wire_value_expr(p.type_ref, &p.dart_name, p.enum_name.as_deref(), needs);
            wl!(out, "      if ({} != null) {key}: {v},", p.dart_name);
        }
    }
    wl!(out, "    }};");
    true
}

/// `final headers = <String, String>{...};` including cookie assembly.
/// Returns false when nothing needs to be added.
fn emit_header_map(
    ctx: &Ctx,
    out: &mut String,
    plan: &MethodPlan,
    needs: &mut Needs,
    extra_entries: &[String],
    cookie_extra: &[String],
) -> bool {
    let hs: Vec<&ParamPlan> = plan
        .params
        .iter()
        .filter(|p| p.location == ParameterLocation::Header)
        .collect();
    let cs: Vec<&ParamPlan> = plan
        .params
        .iter()
        .filter(|p| p.location == ParameterLocation::Cookie)
        .collect();
    if hs.is_empty() && cs.is_empty() && extra_entries.is_empty() && cookie_extra.is_empty() {
        return false;
    }
    if !cs.is_empty() || !cookie_extra.is_empty() {
        wl!(out, "    final cookies = <String>[");
        for e in cookie_extra {
            wl!(out, "      {e}");
        }
        for p in &cs {
            let key = escape_literal_fragment(&p.spec_name);
            if p.required {
                let v =
                    ctx.wire_scalar_expr(p.type_ref, &p.dart_name, p.enum_name.as_deref(), needs);
                wl!(out, "      '{key}=${{Uri.encodeComponent({v})}}',",);
            } else {
                let v =
                    ctx.wire_scalar_expr(p.type_ref, &p.dart_name, p.enum_name.as_deref(), needs);
                wl!(
                    out,
                    "      if ({} != null) '{key}=${{Uri.encodeComponent({v})}}',",
                    p.dart_name
                );
            }
        }
        wl!(out, "    ];");
    }
    wl!(out, "    final headers = <String, String>{{");
    for e in extra_entries {
        wl!(out, "      {e}");
    }
    for p in &hs {
        let key = dart_str(&p.spec_name);
        if p.required {
            wl!(
                out,
                "      {key}: {},",
                ctx.wire_scalar_expr(p.type_ref, &p.dart_name, p.enum_name.as_deref(), needs)
            );
        } else {
            let v = ctx.wire_scalar_expr(p.type_ref, &p.dart_name, p.enum_name.as_deref(), needs);
            wl!(out, "      if ({} != null) {key}: {v},", p.dart_name);
        }
    }
    if !cs.is_empty() || !cookie_extra.is_empty() {
        wl!(
            out,
            "      if (cookies.isNotEmpty) 'Cookie': cookies.join('; '),"
        );
    }
    wl!(out, "    }};");
    true
}

// ── Credentials ───────────────────────────────────────────────────────────────

fn credential_param_name(scheme: &SecurityScheme) -> String {
    let base = member_name(&scheme.name);
    if base == "baseUrl"
        || base == "options"
        || base == "interceptors"
        || base == "httpClientAdapter"
        || base == "client"
    {
        format!("{base}Credential")
    } else {
        base
    }
}

struct Credential<'a> {
    scheme: &'a SecurityScheme,
    param: String,
}

fn credentials(api: &Api) -> Vec<Credential<'_>> {
    let mut taken: HashSet<String> = HashSet::new();
    api.security_schemes
        .iter()
        .map(|s| Credential {
            scheme: s,
            param: unique(&credential_param_name(s), &mut taken, "Credential"),
        })
        .collect()
}

/// A short doc line describing what to pass for a credential.
fn credential_doc(scheme: &SecurityScheme) -> String {
    match &scheme.kind {
        SecuritySchemeKind::ApiKey {
            parameter_name,
            location,
        } => {
            format!("API key sent as the `{parameter_name}` {location}.")
        }
        SecuritySchemeKind::HttpBasic => {
            "`username:password` for HTTP Basic auth (base64-encoded automatically).".to_string()
        }
        SecuritySchemeKind::HttpBearer { bearer_format } => match bearer_format {
            Some(f) => format!("Bearer token ({f}) sent in the `Authorization` header."),
            None => "Bearer token sent in the `Authorization` header.".to_string(),
        },
        SecuritySchemeKind::OAuth2 { .. } => {
            "OAuth 2.0 access token sent as a Bearer `Authorization` header.".to_string()
        }
        SecuritySchemeKind::OpenIdConnect { .. } => {
            "OpenID Connect access token sent as a Bearer `Authorization` header.".to_string()
        }
    }
}

// ── Request body helpers ──────────────────────────────────────────────────────

enum BodyKind {
    Json,
    Multipart,
    FormUrlEncoded,
    Binary,
    Other,
}

fn body_kind(ctx: &Ctx, b: &BodyPlan) -> BodyKind {
    if b.body.is_multipart {
        BodyKind::Multipart
    } else if b.body.is_form_urlencoded() {
        BodyKind::FormUrlEncoded
    } else if ctx.is_binary(&b.body.schema_ref) {
        BodyKind::Binary
    } else if b.body.is_json() {
        BodyKind::Json
    } else {
        BodyKind::Other
    }
}

/// JSON-encodable expression for the (non-null) body value.
fn body_json_expr(ctx: &Ctx, b: &BodyPlan) -> String {
    match &b.body.schema_ref {
        TypeRef::Enum(_) => "body.toJson()".to_string(),
        t => ctx.to_json_expr(t, "body"),
    }
}

/// Multipart parts as `(key, expression, is_conditional_on_null_of)` lines.
/// Returns `FormData.fromMap({...})`-style entries; `file_wrap` renders a
/// bytes value as a multipart file for the backend.
fn multipart_entries(
    ctx: &Ctx,
    b: &BodyPlan,
    file_wrap: &dyn Fn(&str, &str) -> String,
    needs: &mut Needs,
) -> Vec<String> {
    let mut entries = Vec::new();
    if let Some((schema_name, fields)) = ctx.object_fields(&b.body.schema_ref) {
        let plans = plan_fields(ctx, schema_name, fields, false);
        for p in &plans {
            let key = dart_str(&p.field.name);
            let access = format!("body.{}", p.dart_name);
            let non_null = if p.dart_nullable {
                format!("{access}{}", ctx.bang())
            } else {
                access.clone()
            };
            let value = if ctx.is_binary(&p.field.type_ref) {
                file_wrap(&non_null, &p.field.name)
            } else if ctx.is_array_like(&p.field.type_ref)
                && matches!(inner_type(ctx, &p.field.type_ref), Some(TypeRef::Binary))
            {
                format!(
                    "{non_null}.map((e) => {}).toList()",
                    file_wrap("e", &p.field.name)
                )
            } else if p.field.type_ref.is_scalar() || matches!(p.field.type_ref, TypeRef::Enum(_)) {
                ctx.wire_scalar_expr(&p.field.type_ref, &non_null, p.enum_name.as_deref(), needs)
            } else {
                needs.convert = true;
                format!(
                    "jsonEncode({})",
                    ctx.to_json_expr(&p.field.type_ref, &non_null)
                )
            };
            if p.dart_nullable || p.declared_type == "dynamic" {
                entries.push(format!("if ({access} != null) {key}: {value},"));
            } else {
                entries.push(format!("{key}: {value},"));
            }
        }
    } else if ctx.is_binary(&b.body.schema_ref) {
        entries.push(format!("'file': {},", file_wrap("body", "file")));
    } else {
        needs.convert = true;
        entries.push(format!("'data': jsonEncode({}),", body_json_expr(ctx, b)));
    }
    entries
}

fn inner_type<'c>(ctx: &Ctx<'c>, t: &'c TypeRef) -> Option<&'c TypeRef> {
    match t {
        TypeRef::Array(inner) => Some(inner),
        TypeRef::Named(name) => match ctx.kind(name) {
            Some(SchemaKind::Array { item }) => Some(item),
            _ => None,
        },
        _ => None,
    }
}

// ── Model imports for the client ──────────────────────────────────────────────

fn client_imports(ctx: &Ctx, class_name: &str) -> BTreeSet<String> {
    let mut imports = BTreeSet::new();
    for (i, op) in ctx.api.operations.iter().enumerate() {
        for p in &op.parameters {
            let e = ctx
                .param_enums
                .get(&(i, p.name.clone(), p.location))
                .cloned();
            ctx.collect_imports(&p.type_ref, e.as_deref(), class_name, &mut imports);
        }
        if let Some(rb) = &op.request_body {
            let e = ctx.body_enums.get(&i).cloned();
            ctx.collect_imports(&rb.schema_ref, e.as_deref(), class_name, &mut imports);
        }
        if let Some(resp) = success_response(&op.responses)
            && let Some(t) = &resp.schema_ref
        {
            let e = ctx
                .response_enums
                .get(&(i, resp.status_code.clone()))
                .cloned();
            ctx.collect_imports(t, e.as_deref(), class_name, &mut imports);
        }
    }
    imports
}

fn emit_server_urls(out: &mut String, class_name: &str, base_urls: &[String]) {
    if base_urls.len() > 1 {
        wl!(
            out,
            "/// Server URLs declared in the specification, in order."
        );
        wl!(out, "abstract final class {class_name}Urls {{");
        for (i, url) in base_urls.iter().enumerate() {
            wl!(out, "  static const String server{i} = {};", dart_str(url));
        }
        wl!(out, "}}");
        wl!(out);
    }
}

// ── DIO client emitter ────────────────────────────────────────────────────────

fn emit_client_dio(ctx: &Ctx, class_name: &str) -> String {
    let creds = credentials(ctx.api);
    let mut needs = Needs::default();
    let mut body = String::new();

    // Methods first so we know which imports they need.
    for (i, op) in ctx.api.operations.iter().enumerate() {
        let plan = plan_method(ctx, i, op, ClientBackend::Dio);
        wl!(body);
        emit_method_dio(ctx, &mut body, plan, &mut needs);
    }
    if creds
        .iter()
        .any(|c| matches!(c.scheme.kind, SecuritySchemeKind::HttpBasic))
    {
        needs.convert = true;
    }

    let mut out = String::new();
    wl!(out, "// GENERATED by flap — do not edit by hand.");
    if needs.convert {
        wl!(out, "import 'dart:convert';");
        wl!(out);
    }
    wl!(out, "import 'package:dio/dio.dart';");
    if !ctx.safe() {
        wl!(out, "import 'package:meta/meta.dart';");
    }
    let imports = client_imports(ctx, class_name);
    if !imports.is_empty() {
        wl!(out);
        write_imports(&mut out, &imports);
    }
    wl!(out);
    emit_server_urls(&mut out, class_name, &ctx.api.base_urls);

    let default_url = dart_str(ctx.api.base_urls.first().map(String::as_str).unwrap_or(""));
    doc_comment(
        &mut out,
        "",
        &format!("Dio client for the `{}` API.", ctx.api.title),
    );
    wl!(out, "class {class_name} {{");
    wl!(out, "  {class_name}({{");
    wl!(out, "    String baseUrl = {default_url},");
    for c in &creds {
        wl!(out, "    /// {}", credential_doc(c.scheme));
        wl!(out, "    {} {},", ctx.nullable("String"), c.param);
    }
    wl!(out, "    {} options,", ctx.nullable("BaseOptions"));
    wl!(out, "    List<Interceptor> interceptors = const [],");
    wl!(
        out,
        "    {} httpClientAdapter,",
        ctx.nullable("HttpClientAdapter")
    );
    wl!(
        out,
        "  }}) : _dio = Dio((options ?? BaseOptions()).copyWith(baseUrl: baseUrl)) {{"
    );
    wl!(out, "    if (httpClientAdapter != null) {{");
    wl!(out, "      _dio.httpClientAdapter = httpClientAdapter;");
    wl!(out, "    }}");
    wl!(out, "    _dio.interceptors.addAll(interceptors);");
    if !creds.is_empty() {
        wl!(out, "    _dio.interceptors.add(");
        wl!(out, "      InterceptorsWrapper(");
        wl!(out, "        onRequest: (options, handler) {{");
        for c in &creds {
            emit_credential_injection_dio(&mut out, c);
        }
        wl!(out, "          handler.next(options);");
        wl!(out, "        }},");
        wl!(out, "      ),");
        wl!(out, "    );");
    }
    wl!(out, "  }}");
    wl!(out);
    wl!(out, "  final Dio _dio;");
    wl!(out);
    wl!(
        out,
        "  /// The underlying [Dio] instance, for advanced configuration."
    );
    wl!(out, "  Dio get dio => _dio;");
    out.push_str(&body);
    wl!(out, "}}");
    out
}

fn emit_credential_injection_dio(out: &mut String, c: &Credential) {
    let p = &c.param;
    wl!(out, "          if ({p} != null) {{");
    match &c.scheme.kind {
        SecuritySchemeKind::HttpBasic => {
            wl!(out, "            options.headers['Authorization'] =");
            wl!(
                out,
                "                'Basic ${{base64Encode(utf8.encode({p}))}}';"
            );
        }
        SecuritySchemeKind::HttpBearer { .. }
        | SecuritySchemeKind::OAuth2 { .. }
        | SecuritySchemeKind::OpenIdConnect { .. } => {
            wl!(
                out,
                "            options.headers['Authorization'] = 'Bearer ${p}';"
            );
        }
        SecuritySchemeKind::ApiKey {
            parameter_name,
            location,
        } => match location {
            ApiKeyLocation::Header => {
                wl!(
                    out,
                    "            options.headers[{}] = {p};",
                    dart_str(parameter_name)
                );
            }
            ApiKeyLocation::Query => {
                wl!(
                    out,
                    "            options.queryParameters[{}] = {p};",
                    dart_str(parameter_name)
                );
            }
            ApiKeyLocation::Cookie => {
                wl!(
                    out,
                    "            final existing = options.headers['Cookie'];"
                );
                wl!(
                    out,
                    "            final cookie = '{}=${{Uri.encodeComponent({p})}}';",
                    escape_literal_fragment(parameter_name)
                );
                wl!(out, "            options.headers['Cookie'] =");
                wl!(
                    out,
                    "                existing == null ? cookie : '$existing; $cookie';"
                );
            }
        },
    }
    wl!(out, "          }}");
}

fn emit_method_dio(ctx: &Ctx, out: &mut String, mut plan: MethodPlan, needs: &mut Needs) {
    emit_method_doc(out, &plan);
    wl!(out, "  Future<{}> {}({{", plan.return_type, plan.name);
    emit_signature_params(ctx, out, &plan);
    wl!(out, "    {} cancelToken,", ctx.nullable("CancelToken"));
    wl!(out, "  }}) async {{");

    let mut local_needs = std::mem::take(&mut plan.needs);
    let has_query = emit_query_map(ctx, out, &plan, &mut local_needs, &[]);
    let has_headers = emit_header_map(ctx, out, &plan, &mut local_needs, &[], &[]);

    // Body
    let mut data_expr: Option<String> = None;
    let mut content_type: Option<String> = None;
    if let Some(b) = &plan.body {
        let kind = body_kind(ctx, b);
        let nullable_body = !b.required;
        let expr = match kind {
            BodyKind::Multipart => {
                let entries = multipart_entries(
                    ctx,
                    b,
                    &|bytes, name| {
                        format!(
                            "MultipartFile.fromBytes({bytes}, filename: {})",
                            dart_str(name)
                        )
                    },
                    &mut local_needs,
                );
                if nullable_body {
                    wl!(out, "    final formData = body == null");
                    wl!(out, "        ? null");
                    wl!(out, "        : FormData.fromMap(<String, dynamic>{{");
                    for e in entries {
                        wl!(out, "            {e}");
                    }
                    wl!(out, "          }});");
                } else {
                    wl!(
                        out,
                        "    final formData = FormData.fromMap(<String, dynamic>{{"
                    );
                    for e in entries {
                        wl!(out, "      {e}");
                    }
                    wl!(out, "    }});");
                }
                "formData".to_string()
            }
            BodyKind::FormUrlEncoded => {
                content_type = Some("Headers.formUrlEncodedContentType".to_string());
                body_json_expr(ctx, b)
            }
            BodyKind::Binary => {
                content_type = Some(dart_str(&b.body.content_type));
                "body".to_string()
            }
            BodyKind::Json => body_json_expr(ctx, b),
            BodyKind::Other => {
                content_type = Some(dart_str(&b.body.content_type));
                match &b.body.schema_ref {
                    TypeRef::String => "body".to_string(),
                    _ => {
                        local_needs.convert = true;
                        format!("jsonEncode({})", body_json_expr(ctx, b))
                    }
                }
            }
        };
        data_expr = Some(if nullable_body && expr != "body" && expr != "formData" {
            // `body` is a promoted local: `body?.toJson()` when the expression
            // is a plain member access, otherwise an explicit null check.
            match expr.strip_prefix("body.") {
                Some(rest) if !rest.contains("body") => format!("body?.{rest}"),
                _ => format!("body == null ? null : {expr}"),
            }
        } else {
            expr
        });
    }

    let wants_bytes = plan
        .success
        .and_then(|r| r.schema_ref.as_ref())
        .is_some_and(|t| ctx.is_binary(t));
    let needs_response = plan
        .success
        .is_some_and(|r| r.schema_ref.is_some() || (!r.headers.is_empty() && ctx.safe()));

    if needs_response {
        wl!(out, "    final response = await _dio.request<dynamic>(");
    } else {
        wl!(out, "    await _dio.request<dynamic>(");
    }
    wl!(out, "      '{}',", plan.path_expr);
    let mut opts: Vec<String> = vec![format!("method: {}", dart_str(plan.op.method.as_str()))];
    if has_headers {
        opts.push("headers: headers".to_string());
    }
    if let Some(ct) = content_type {
        opts.push(format!("contentType: {ct}"));
    }
    if wants_bytes {
        opts.push("responseType: ResponseType.bytes".to_string());
    }
    wl!(out, "      options: Options({}),", opts.join(", "));
    if has_query {
        wl!(out, "      queryParameters: queryParameters,");
    }
    if let Some(d) = data_expr {
        wl!(out, "      data: {d},");
    }
    wl!(out, "      cancelToken: cancelToken,");
    wl!(out, "    );");

    if let Some(resp) = plan.success
        && ctx.safe()
        && !resp.headers.is_empty()
    {
        emit_header_bindings(ctx, out, resp, |name| {
            format!("response.headers.value({})", dart_str(name))
        });
    }
    emit_return_statement(ctx, out, &plan, "response.data", false);
    wl!(out, "  }}");
    needs.convert |= local_needs.convert;
}

// ── HTTP client emitter ───────────────────────────────────────────────────────

fn emit_client_http(ctx: &Ctx, class_name: &str) -> String {
    let creds = credentials(ctx.api);
    let query_creds: Vec<&Credential> = creds
        .iter()
        .filter(|c| {
            matches!(
                c.scheme.kind,
                SecuritySchemeKind::ApiKey {
                    location: ApiKeyLocation::Query,
                    ..
                }
            )
        })
        .collect();
    let cookie_creds: Vec<&Credential> = creds
        .iter()
        .filter(|c| {
            matches!(
                c.scheme.kind,
                SecuritySchemeKind::ApiKey {
                    location: ApiKeyLocation::Cookie,
                    ..
                }
            )
        })
        .collect();
    let header_creds: Vec<&Credential> = creds
        .iter()
        .filter(|c| {
            !query_creds.iter().any(|q| q.param == c.param)
                && !cookie_creds.iter().any(|q| q.param == c.param)
        })
        .collect();

    let mut needs = Needs { convert: true };
    let mut body = String::new();
    for (i, op) in ctx.api.operations.iter().enumerate() {
        let plan = plan_method(ctx, i, op, ClientBackend::Http);
        wl!(body);
        emit_method_http(
            ctx,
            &mut body,
            plan,
            &mut needs,
            class_name,
            !query_creds.is_empty(),
            !cookie_creds.is_empty(),
            !header_creds.is_empty(),
        );
    }

    let mut out = String::new();
    wl!(out, "// GENERATED by flap — do not edit by hand.");
    wl!(out, "import 'dart:convert';");
    wl!(out);
    wl!(out, "import 'package:http/http.dart' as http;");
    if !ctx.safe() {
        wl!(out, "import 'package:meta/meta.dart';");
    }
    let imports = client_imports(ctx, class_name);
    if !imports.is_empty() {
        wl!(out);
        write_imports(&mut out, &imports);
    }
    wl!(out);
    emit_server_urls(&mut out, class_name, &ctx.api.base_urls);

    let default_url = dart_str(ctx.api.base_urls.first().map(String::as_str).unwrap_or(""));
    doc_comment(
        &mut out,
        "",
        &format!("`package:http` client for the `{}` API.", ctx.api.title),
    );
    wl!(out, "class {class_name} {{");
    wl!(out, "  {class_name}({{");
    wl!(out, "    String baseUrl = {default_url},");
    for c in &creds {
        wl!(out, "    /// {}", credential_doc(c.scheme));
        wl!(out, "    {} {},", ctx.nullable("String"), c.param);
    }
    wl!(out, "    {} client,", ctx.nullable("http.Client"));
    wl!(out, "  }})  : _baseUrl = baseUrl.endsWith('/')");
    wl!(
        out,
        "            ? baseUrl.substring(0, baseUrl.length - 1)"
    );
    wl!(out, "            : baseUrl,");
    w!(out, "        _client = client ?? http.Client()");
    for c in &creds {
        w!(out, ",\n        _{} = {}", c.param, c.param);
    }
    wl!(out, ";");
    wl!(out);
    wl!(out, "  final String _baseUrl;");
    wl!(out, "  final http.Client _client;");
    for c in &creds {
        wl!(out, "  final {} _{};", ctx.nullable("String"), c.param);
    }
    wl!(out);
    wl!(out, "  /// The underlying [http.Client].");
    wl!(out, "  http.Client get client => _client;");

    if !header_creds.is_empty() {
        wl!(out);
        wl!(
            out,
            "  Map<String, String> get _authHeaders => <String, String>{{"
        );
        for c in &header_creds {
            let p = format!("_{}", c.param);
            match &c.scheme.kind {
                SecuritySchemeKind::HttpBasic => {
                    wl!(out, "        if ({p} != null)");
                    wl!(
                        out,
                        "          'Authorization': 'Basic ${{base64Encode(utf8.encode({p}))}}',"
                    );
                }
                SecuritySchemeKind::ApiKey { parameter_name, .. } => {
                    wl!(
                        out,
                        "        if ({p} != null) {}: {p},",
                        dart_str(parameter_name)
                    );
                }
                _ => {
                    wl!(
                        out,
                        "        if ({p} != null) 'Authorization': 'Bearer ${p}',"
                    );
                }
            }
        }
        wl!(out, "      }};");
    }
    if !query_creds.is_empty() {
        wl!(out);
        wl!(
            out,
            "  Map<String, String> get _authQuery => <String, String>{{"
        );
        for c in &query_creds {
            let p = format!("_{}", c.param);
            if let SecuritySchemeKind::ApiKey { parameter_name, .. } = &c.scheme.kind {
                wl!(
                    out,
                    "        if ({p} != null) {}: {p},",
                    dart_str(parameter_name)
                );
            }
        }
        wl!(out, "      }};");
    }
    if !cookie_creds.is_empty() {
        wl!(out);
        wl!(out, "  List<String> get _authCookies => <String>[");
        for c in &cookie_creds {
            let p = format!("_{}", c.param);
            if let SecuritySchemeKind::ApiKey { parameter_name, .. } = &c.scheme.kind {
                wl!(
                    out,
                    "        if ({p} != null) '{}=${{Uri.encodeComponent({p})}}',",
                    escape_literal_fragment(parameter_name)
                );
            }
        }
        wl!(out, "      ];");
    }

    wl!(out);
    wl!(
        out,
        "  Future<http.Response> _send(http.BaseRequest request) async {{"
    );
    wl!(out, "    final streamed = await _client.send(request);");
    wl!(out, "    return http.Response.fromStream(streamed);");
    wl!(out, "  }}");
    wl!(out);
    wl!(
        out,
        "  Uri _uri(String path, Map<String, dynamic> queryParameters) {{"
    );
    wl!(out, "    final uri = Uri.parse('$_baseUrl$path');");
    wl!(out, "    if (queryParameters.isEmpty) return uri;");
    wl!(
        out,
        "    return uri.replace(queryParameters: <String, dynamic>{{"
    );
    wl!(out, "      ...uri.queryParametersAll,");
    wl!(out, "      ...queryParameters,");
    wl!(out, "    }});");
    wl!(out, "  }}");
    out.push_str(&body);
    wl!(out, "}}");
    wl!(out);
    wl!(
        out,
        "/// Thrown by [{class_name}] when the server responds with a non-2xx status."
    );
    wl!(out, "class {class_name}Exception implements Exception {{");
    wl!(out, "  const {class_name}Exception({{");
    wl!(out, "    {}this.statusCode,", ctx.required_kw());
    wl!(out, "    {}this.body,", ctx.required_kw());
    wl!(out, "    {}this.method,", ctx.required_kw());
    wl!(out, "    {}this.path,", ctx.required_kw());
    wl!(out, "  }});");
    wl!(out);
    wl!(out, "  final int statusCode;");
    wl!(out, "  final String body;");
    wl!(out, "  final String method;");
    wl!(out, "  final String path;");
    wl!(out);
    wl!(out, "  @override");
    wl!(out, "  String toString() =>");
    wl!(
        out,
        "      '{class_name}Exception: $method $path returned $statusCode: $body';"
    );
    wl!(out, "}}");
    let _ = needs;
    out
}

#[allow(clippy::too_many_arguments)]
fn emit_method_http(
    ctx: &Ctx,
    out: &mut String,
    mut plan: MethodPlan,
    needs: &mut Needs,
    class_name: &str,
    has_auth_query: bool,
    has_auth_cookies: bool,
    has_auth_headers: bool,
) {
    emit_method_doc(out, &plan);
    let has_params = !plan.params.is_empty() || plan.body.is_some();
    if has_params {
        wl!(out, "  Future<{}> {}({{", plan.return_type, plan.name);
        emit_signature_params(ctx, out, &plan);
        wl!(out, "  }}) async {{");
    } else {
        wl!(
            out,
            "  Future<{}> {}() async {{",
            plan.return_type,
            plan.name
        );
    }

    let mut local_needs = std::mem::take(&mut plan.needs);
    let auth_query_entry: Vec<String> = if has_auth_query {
        vec!["..._authQuery,".to_string()]
    } else {
        vec![]
    };
    let has_query = emit_query_map(ctx, out, &plan, &mut local_needs, &auth_query_entry);
    let auth_header_entry: Vec<String> = if has_auth_headers {
        vec!["..._authHeaders,".to_string()]
    } else {
        vec![]
    };
    let auth_cookie_entry: Vec<String> = if has_auth_cookies {
        vec!["..._authCookies,".to_string()]
    } else {
        vec![]
    };
    let has_headers = emit_header_map(
        ctx,
        out,
        &plan,
        &mut local_needs,
        &auth_header_entry,
        &auth_cookie_entry,
    );

    let query_arg = if has_query {
        "queryParameters"
    } else {
        "const <String, dynamic>{}"
    };
    let method_lit = dart_str(plan.op.method.as_str());
    let path_lit = format!("'{}'", plan.path_expr);

    let is_multipart = plan.body.as_ref().is_some_and(|b| b.body.is_multipart);
    if is_multipart {
        wl!(
            out,
            "    final request = http.MultipartRequest({method_lit}, _uri({path_lit}, {query_arg}));"
        );
    } else {
        wl!(
            out,
            "    final request = http.Request({method_lit}, _uri({path_lit}, {query_arg}));"
        );
    }
    if has_headers {
        wl!(out, "    request.headers.addAll(headers);");
    }

    if let Some(b) = &plan.body {
        let nullable_body = !b.required;
        let indent = if nullable_body { "      " } else { "    " };
        if nullable_body {
            wl!(out, "    if (body != null) {{");
        }
        let body_var = "body".to_string();
        match body_kind(ctx, b) {
            BodyKind::Multipart => {
                let entries = multipart_entries(
                    ctx,
                    b,
                    &|bytes, name| {
                        format!(
                            "http.MultipartFile.fromBytes({}, {bytes}, filename: {})",
                            dart_str(name),
                            dart_str(name)
                        )
                    },
                    &mut local_needs,
                );
                // Split entries into files vs fields by inspecting the value expression.
                wl!(out, "{indent}final parts = <String, dynamic>{{");
                for e in entries {
                    wl!(
                        out,
                        "{indent}  {}",
                        e.replace("body.", &format!("{body_var}."))
                    );
                }
                wl!(out, "{indent}}};");
                wl!(out, "{indent}parts.forEach((key, value) {{");
                wl!(out, "{indent}  if (value is http.MultipartFile) {{");
                wl!(out, "{indent}    request.files.add(value);");
                wl!(
                    out,
                    "{indent}  }} else if (value is Iterable<http.MultipartFile>) {{"
                );
                wl!(out, "{indent}    request.files.addAll(value);");
                wl!(out, "{indent}  }} else if (value is Iterable) {{");
                wl!(out, "{indent}    request.fields[key] = value.join(',');");
                wl!(out, "{indent}  }} else {{");
                wl!(out, "{indent}    request.fields[key] = value.toString();");
                wl!(out, "{indent}  }}");
                wl!(out, "{indent}}});");
            }
            BodyKind::FormUrlEncoded => {
                let json = body_json_expr(ctx, b).replace("body", &body_var);
                wl!(
                    out,
                    "{indent}final fields = Map<String, dynamic>.from({json} as Map)"
                );
                wl!(out, "{indent}  ..removeWhere((_, v) => v == null);");
                wl!(
                    out,
                    "{indent}request.bodyFields = fields.map((k, v) => MapEntry(k, v is Iterable ? v.join(',') : v.toString()));"
                );
            }
            BodyKind::Binary => {
                wl!(
                    out,
                    "{indent}request.headers['Content-Type'] = {};",
                    dart_str(&b.body.content_type)
                );
                wl!(out, "{indent}request.bodyBytes = {body_var};");
            }
            BodyKind::Json => {
                let json = body_json_expr(ctx, b).replace("body", &body_var);
                wl!(
                    out,
                    "{indent}request.headers['Content-Type'] = 'application/json';"
                );
                wl!(out, "{indent}request.body = jsonEncode({json});");
            }
            BodyKind::Other => {
                wl!(
                    out,
                    "{indent}request.headers['Content-Type'] = {};",
                    dart_str(&b.body.content_type)
                );
                match &b.body.schema_ref {
                    TypeRef::String => wl!(out, "{indent}request.body = {body_var};"),
                    _ => {
                        let json = body_json_expr(ctx, b).replace("body", &body_var);
                        wl!(out, "{indent}request.body = jsonEncode({json});");
                    }
                }
            }
        }
        if nullable_body {
            wl!(out, "    }}");
        }
    }

    wl!(out, "    final response = await _send(request);");
    wl!(
        out,
        "    if (response.statusCode < 200 || response.statusCode >= 300) {{"
    );
    wl!(out, "      throw {class_name}Exception(");
    wl!(out, "        statusCode: response.statusCode,");
    wl!(out, "        body: response.body,");
    wl!(out, "        method: {method_lit},");
    wl!(out, "        path: {},", dart_str(&plan.op.path));
    wl!(out, "      );");
    wl!(out, "    }}");

    if let Some(resp) = plan.success
        && ctx.safe()
        && !resp.headers.is_empty()
    {
        emit_header_bindings(ctx, out, resp, |name| {
            format!("response.headers[{}]", dart_str(name))
        });
    }

    let (data_expr, typed) = match plan.success.and_then(|r| r.schema_ref.as_ref()) {
        Some(t) if ctx.is_binary(t) => ("response.bodyBytes".to_string(), true),
        Some(TypeRef::String)
            if plan
                .success
                .and_then(|r| r.content_type.as_deref())
                .is_some_and(|ct| !ct.to_ascii_lowercase().contains("json")) =>
        {
            ("response.body".to_string(), true)
        }
        Some(_) => ("jsonDecode(response.body)".to_string(), false),
        None => (String::new(), false),
    };
    emit_return_statement(ctx, out, &plan, &data_expr, typed);
    wl!(out, "  }}");
    needs.convert |= local_needs.convert;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_splitting_and_cases() {
        assert_eq!(to_snake_case("XRateLimit"), "x_rate_limit");
        assert_eq!(to_snake_case("HTTPResponse"), "http_response");
        assert_eq!(to_snake_case("Petstore31Client"), "petstore31_client");
        assert_eq!(to_snake_case("com.example.Pet"), "com_example_pet");
        assert_eq!(to_camel_case("display_name"), "displayName");
        assert_eq!(to_camel_case("X-API-Key"), "xApiKey");
        assert_eq!(to_camel_case("DisplayName"), "displayName");
        assert_eq!(to_camel_case("2fa"), "n2fa");
        assert_eq!(to_pascal_case("get-pets"), "GetPets");
        assert_eq!(to_pascal_case(""), "Value");
    }

    #[test]
    fn keywords_and_reserved_members_are_escaped() {
        assert_eq!(member_name("default"), "defaultValue");
        assert_eq!(member_name("class"), "classValue");
        assert_eq!(member_name("hashCode"), "hashCodeValue");
        assert_eq!(member_name("in"), "inValue");
        assert_eq!(member_name("id"), "id");
        assert_eq!(dart_class_name("String"), "StringModel");
        assert_eq!(dart_class_name("Pet"), "Pet");
        assert_eq!(dart_class_name("pet.Response"), "PetResponse");
    }

    #[test]
    fn client_name_is_an_identifier() {
        assert_eq!(api_client_name("Swagger Petstore"), "SwaggerPetstoreClient");
        assert_eq!(api_client_name("Petstore 3.1"), "Petstore31Client");
        assert_eq!(api_client_name("my-api (v2)"), "MyApiV2Client");
        assert_eq!(api_client_name(""), "ApiClient");
    }

    #[test]
    fn string_literals_are_escaped() {
        assert_eq!(dart_str("a'b"), r"'a\'b'");
        assert_eq!(dart_str("$100"), r"'\$100'");
        assert_eq!(dart_str("back\\slash"), r"'back\\slash'");
        assert_eq!(dart_str("line\nbreak"), r"'line\nbreak'");
    }

    #[test]
    fn enum_cases_are_unique_and_safe() {
        let values = vec![
            EnumValue::Str("default".into()),
            EnumValue::Str("Foo".into()),
            EnumValue::Str("foo".into()),
            EnumValue::Str("unknown".into()),
            EnumValue::Str("values".into()),
            EnumValue::Int(-1),
            EnumValue::Int(2),
        ];
        assert_eq!(
            enum_case_names(&values),
            vec![
                "defaultValue",
                "foo",
                "foo2",
                "unknownValue",
                "valuesValue",
                "vMinus1",
                "v2"
            ]
        );
    }

    #[test]
    fn path_literal_escaping_keeps_interpolations() {
        let p = escape_path_literal("/a'b/${Uri.encodeComponent(id)}/c$d");
        assert_eq!(p, "/a\\'b/${Uri.encodeComponent(id)}/c\\$d");
    }
}
