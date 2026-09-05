//! OpenAPI 3.x / Swagger 2.0 loader and lowering pass.
//!
//! Entry points: [`load`], [`load_str`], [`load_url`], [`load_path_or_url`].
//! Every one of them accepts YAML or JSON and auto-detects OpenAPI 3.x
//! (`openapi:`) versus Swagger 2.0 (`swagger:`). Swagger documents are
//! translated into the OpenAPI 3 raw shape (see [`swagger`]) so a single
//! validation + lowering pass serves both.
//!
//! Lowering is deliberately lenient: constructs the generator cannot express
//! degrade to `dynamic` and are reported through [`Api::warnings`] instead of
//! aborting the whole run. Only genuine spec errors (dangling `$ref`s,
//! duplicate operationIds, …) are fatal.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use flap_ir::{
    Api, ApiKeyLocation, EnumValue, ExtensionValue, Extensions, Field, HttpMethod, OAuth2Flow,
    OAuth2FlowType, Operation, Parameter, ParameterLocation, RequestBody, Response, Schema,
    SchemaKind, SecurityScheme, SecuritySchemeKind, TypeRef,
};
use serde::Deserialize;
use serde::de;

pub mod swagger;

use swagger::SwaggerSpec;

// ── Extension helpers ─────────────────────────────────────────────────────────

fn yaml_to_extension(v: &serde_yaml::Value) -> ExtensionValue {
    match v {
        serde_yaml::Value::Null => ExtensionValue::Null,
        serde_yaml::Value::Bool(b) => ExtensionValue::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                ExtensionValue::Integer(i)
            } else {
                ExtensionValue::Float(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        serde_yaml::Value::String(s) => ExtensionValue::String(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            ExtensionValue::Sequence(seq.iter().map(yaml_to_extension).collect())
        }
        serde_yaml::Value::Mapping(map) => ExtensionValue::Mapping(
            map.iter()
                .filter_map(|(k, v)| k.as_str().map(|s| (s.to_string(), yaml_to_extension(v))))
                .collect(),
        ),
        _ => ExtensionValue::Null,
    }
}

fn collect_extensions(extra: &BTreeMap<String, serde_yaml::Value>) -> Extensions {
    extra
        .iter()
        .filter(|(k, _)| k.starts_with("x-"))
        .map(|(k, v)| (k.clone(), yaml_to_extension(v)))
        .collect()
}

// ── Public entry points ───────────────────────────────────────────────────────

/// Load a spec from a local file (YAML or JSON, OpenAPI 3.x or Swagger 2.0).
pub fn load(path: impl AsRef<Path>) -> Result<Api> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading spec file {}", path.display()))?;
    load_str(&text).with_context(|| format!("in spec file {}", path.display()))
}

/// Load a spec from its textual content (YAML or JSON, OpenAPI 3.x or Swagger 2.0).
pub fn load_str(text: &str) -> Result<Api> {
    let value = parse_document(text)?;
    lower_document(value)
}

/// Load a spec from a remote URL (YAML or JSON, OpenAPI 3.x or Swagger 2.0).
pub fn load_url(url: &str) -> Result<Api> {
    let raw_text = ureq::get(url)
        .call()
        .with_context(|| format!("fetching remote spec from {url}"))?
        .into_string()
        .with_context(|| format!("reading response body from {url}"))?;
    load_str(&raw_text).with_context(|| format!("in remote spec {url}"))
}

/// Load from a local path or, when `spec` starts with `http://`/`https://`, a URL.
pub fn load_path_or_url(spec: &str) -> Result<Api> {
    if spec.starts_with("http://") || spec.starts_with("https://") {
        load_url(spec)
    } else {
        load(spec)
    }
}

/// Load a document that is known to be Swagger 2.0.
pub fn load_swagger_str(text: &str) -> Result<Api> {
    let value = parse_document(text)?;
    let raw: SwaggerSpec = serde_yaml::from_value(value).context("parsing Swagger 2.0 document")?;
    lower(raw.into_openapi())
}

// ── Document parsing & format detection ───────────────────────────────────────

fn parse_document(text: &str) -> Result<serde_yaml::Value> {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    let mut value: serde_yaml::Value = if trimmed.starts_with('{') {
        let json: serde_json::Value = serde_json::from_str(trimmed).context("parsing JSON spec")?;
        serde_yaml::to_value(json).context("converting JSON spec")?
    } else {
        serde_yaml::from_str(trimmed).context("parsing YAML spec")?
    };
    value
        .apply_merge()
        .context("resolving YAML merge keys (`<<`)")?;
    Ok(value)
}

fn scalar_to_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Number(n) => n.to_string(),
        other => format!("{other:?}"),
    }
}

fn lower_document(value: serde_yaml::Value) -> Result<Api> {
    let map = value
        .as_mapping()
        .ok_or_else(|| anyhow!("spec root must be a mapping/object"))?;

    if let Some(v) = map.get("swagger") {
        let version = scalar_to_string(v);
        if !version.starts_with('2') {
            bail!("unsupported Swagger version `{version}` — flap supports Swagger 2.0");
        }
        let raw: SwaggerSpec =
            serde_yaml::from_value(value).context("parsing Swagger 2.0 document")?;
        return lower(raw.into_openapi());
    }

    match map.get("openapi") {
        Some(v) => {
            let version = scalar_to_string(v);
            if !version.starts_with('3') {
                bail!("unsupported OpenAPI version `{version}` — flap supports OpenAPI 3.x");
            }
        }
        None => bail!(
            "document has neither an `openapi` nor a `swagger` field — \
             not an OpenAPI 3.x or Swagger 2.0 document"
        ),
    }

    let raw: RawSpec = serde_yaml::from_value(value).context("parsing OpenAPI document")?;
    lower(raw)
}

// ── Raw serde types ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct RawSpec {
    #[serde(default)]
    pub(crate) info: RawInfo,
    #[serde(default)]
    pub(crate) servers: Vec<RawServer>,
    #[serde(default)]
    pub(crate) paths: BTreeMap<String, RawPathItem>,
    #[serde(default)]
    pub(crate) components: RawComponents,
    #[serde(default)]
    pub(crate) security: Vec<BTreeMap<String, Vec<String>>>,
    #[serde(flatten)]
    pub(crate) extensions: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawInfo {
    pub(crate) title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawServer {
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) variables: BTreeMap<String, RawServerVariable>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawServerVariable {
    pub(crate) default: serde_yaml::Value,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawOAuth2Flows {
    pub(crate) implicit: Option<RawOAuth2Flow>,
    pub(crate) password: Option<RawOAuth2Flow>,
    #[serde(rename = "clientCredentials")]
    pub(crate) client_credentials: Option<RawOAuth2Flow>,
    #[serde(rename = "authorizationCode")]
    pub(crate) authorization_code: Option<RawOAuth2Flow>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawOAuth2Flow {
    #[serde(rename = "tokenUrl")]
    pub(crate) token_url: Option<String>,
    #[serde(rename = "authorizationUrl")]
    pub(crate) authorization_url: Option<String>,
    #[serde(default)]
    pub(crate) scopes: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawComponents {
    #[serde(default)]
    pub(crate) schemas: BTreeMap<String, RawSchemaOrRef>,
    #[serde(default)]
    pub(crate) parameters: BTreeMap<String, RawParameterOrRef>,
    #[serde(default, rename = "requestBodies")]
    pub(crate) request_bodies: BTreeMap<String, RawRequestBodyOrRef>,
    #[serde(default)]
    pub(crate) responses: BTreeMap<String, RawResponseOrRef>,
    #[serde(default)]
    pub(crate) headers: BTreeMap<String, RawResponseHeaderOrRef>,
    #[serde(default, rename = "securitySchemes")]
    pub(crate) security_schemes: BTreeMap<String, RawSecurityScheme>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawSecurityScheme {
    #[serde(rename = "type")]
    pub(crate) ty: String,
    pub(crate) name: Option<String>,
    #[serde(rename = "in")]
    pub(crate) location: Option<String>,
    pub(crate) scheme: Option<String>,
    #[serde(rename = "bearerFormat")]
    pub(crate) bearer_format: Option<String>,
    pub(crate) flows: Option<RawOAuth2Flows>,
    #[serde(rename = "openIdConnectUrl")]
    pub(crate) open_id_connect_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawPathItem {
    pub(crate) get: Option<RawOperation>,
    pub(crate) post: Option<RawOperation>,
    pub(crate) put: Option<RawOperation>,
    pub(crate) delete: Option<RawOperation>,
    pub(crate) patch: Option<RawOperation>,
    pub(crate) options: Option<RawOperation>,
    pub(crate) head: Option<RawOperation>,
    pub(crate) trace: Option<RawOperation>,
    /// Parameters shared by every operation on this path.
    #[serde(default)]
    pub(crate) parameters: Vec<RawParameterOrRef>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawOperation {
    #[serde(rename = "operationId")]
    pub(crate) operation_id: Option<String>,
    pub(crate) summary: Option<String>,
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) parameters: Vec<RawParameterOrRef>,
    #[serde(rename = "requestBody")]
    pub(crate) request_body: Option<RawRequestBodyOrRef>,
    #[serde(default)]
    pub(crate) responses: BTreeMap<String, RawResponseOrRef>,
    pub(crate) security: Option<Vec<BTreeMap<String, Vec<String>>>>,
    #[serde(default)]
    pub(crate) deprecated: bool,
    #[serde(flatten)]
    pub(crate) extensions: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawParameterOrRef {
    Ref {
        #[serde(rename = "$ref")]
        reference: String,
    },
    Inline(RawParameter),
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawParameter {
    pub(crate) name: String,
    #[serde(rename = "in")]
    pub(crate) location: String,
    #[serde(default)]
    pub(crate) required: bool,
    pub(crate) schema: Option<RawSchemaOrRef>,
    /// Alternative to `schema`: a single media type carrying the schema.
    pub(crate) content: Option<BTreeMap<String, RawMediaType>>,
    #[serde(flatten)]
    pub(crate) extensions: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawRequestBodyOrRef {
    Ref {
        #[serde(rename = "$ref")]
        reference: String,
    },
    Inline(RawRequestBody),
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawRequestBody {
    #[serde(default)]
    pub(crate) content: BTreeMap<String, RawMediaType>,
    #[serde(default)]
    pub(crate) required: bool,
    #[serde(flatten)]
    pub(crate) extensions: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawMediaType {
    pub(crate) schema: Option<RawSchemaOrRef>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawResponseOrRef {
    Ref {
        #[serde(rename = "$ref")]
        reference: String,
    },
    Inline(RawResponse),
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawResponse {
    #[allow(dead_code)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) content: BTreeMap<String, RawMediaType>,
    #[serde(default)]
    pub(crate) headers: BTreeMap<String, RawResponseHeaderOrRef>,
    #[serde(flatten)]
    pub(crate) extensions: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawResponseHeaderOrRef {
    Ref {
        #[serde(rename = "$ref")]
        reference: String,
    },
    Inline(RawResponseHeader),
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawResponseHeader {
    pub(crate) schema: Option<RawSchemaOrRef>,
    #[serde(default)]
    pub(crate) required: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawSchemaOrRef {
    Ref {
        #[serde(rename = "$ref")]
        reference: String,
    },
    Inline(Box<RawSchema>),
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawSchema {
    #[serde(
        default,
        rename = "type",
        deserialize_with = "deserialize_openapi_type"
    )]
    pub(crate) ty: Vec<String>,
    pub(crate) format: Option<String>,
    #[serde(default, deserialize_with = "deserialize_required")]
    pub(crate) required: Vec<String>,
    #[serde(default)]
    pub(crate) properties: BTreeMap<String, RawSchemaOrRef>,
    pub(crate) items: Option<Box<RawSchemaOrRef>>,
    #[serde(default, rename = "enum")]
    pub(crate) enum_values: Vec<serde_yaml::Value>,
    #[serde(rename = "additionalProperties")]
    pub(crate) additional_properties: Option<RawAdditionalProperties>,
    #[serde(default, rename = "allOf")]
    pub(crate) all_of: Vec<RawSchemaOrRef>,
    #[serde(default, rename = "anyOf")]
    pub(crate) any_of: Vec<RawSchemaOrRef>,
    #[serde(default, rename = "oneOf")]
    pub(crate) one_of: Vec<RawSchemaOrRef>,
    pub(crate) discriminator: Option<RawDiscriminator>,
    #[serde(default)]
    pub(crate) nullable: Option<bool>,
    #[serde(rename = "default")]
    pub(crate) default: Option<serde_yaml::Value>,
    #[serde(flatten)]
    pub(crate) extensions: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawDiscriminator {
    #[serde(rename = "propertyName")]
    pub(crate) property_name: String,
    #[serde(default)]
    pub(crate) mapping: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawAdditionalProperties {
    Bool(bool),
    Schema(Box<RawSchemaOrRef>),
}

// ── Deserializer helpers ──────────────────────────────────────────────────────

/// `type` may be a string (3.0) or a list of strings (3.1).
pub(crate) fn deserialize_openapi_type<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: de::Deserializer<'de>,
{
    struct TypeVisitor;
    impl<'de> de::Visitor<'de> for TypeVisitor {
        type Value = Vec<String>;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string or an array of strings")
        }
        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(vec![value.to_string()])
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut v = Vec::new();
            while let Some(s) = seq.next_element::<String>()? {
                v.push(s);
            }
            Ok(v)
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
    }
    d.deserialize_any(TypeVisitor)
}

/// `required` is a list of property names in OpenAPI, but a lot of specs in
/// the wild carry a stray Swagger-2-parameter-style `required: true` on a
/// schema. Accept both; a boolean contributes no required property names.
fn deserialize_required<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: de::Deserializer<'de>,
{
    struct RequiredVisitor;
    impl<'de> de::Visitor<'de> for RequiredVisitor {
        type Value = Vec<String>;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("an array of property names")
        }
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut v = Vec::new();
            while let Some(s) = seq.next_element::<String>()? {
                v.push(s);
            }
            Ok(v)
        }
    }
    d.deserialize_any(RequiredVisitor)
}

// ── $ref pointers ─────────────────────────────────────────────────────────────

fn parse_component_ref<'a>(reference: &'a str, section: &str) -> Result<&'a str> {
    let prefix = format!("#/components/{section}/");
    let bare = reference.strip_prefix(prefix.as_str()).ok_or_else(|| {
        anyhow!(
            "$ref `{reference}` is not a `{prefix}*` reference — \
             external and non-component references are not supported"
        )
    })?;
    if bare.is_empty() || bare.contains('/') {
        bail!("malformed $ref pointer `{reference}`");
    }
    Ok(bare)
}

fn parse_schema_ref_pointer(reference: &str) -> Result<&str> {
    parse_component_ref(reference, "schemas")
}

// ── Validation ────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct Diagnostics {
    errors: Vec<String>,
}

impl Diagnostics {
    fn error(&mut self, msg: impl Into<String>) {
        self.errors.push(msg.into());
    }

    fn into_result(self) -> Result<()> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            let joined = self.errors.join("\n  - ");
            bail!("spec validation failed:\n  - {joined}");
        }
    }
}

fn path_operations(item: &RawPathItem) -> [(HttpMethod, Option<&RawOperation>); 8] {
    [
        (HttpMethod::Delete, item.delete.as_ref()),
        (HttpMethod::Get, item.get.as_ref()),
        (HttpMethod::Head, item.head.as_ref()),
        (HttpMethod::Options, item.options.as_ref()),
        (HttpMethod::Patch, item.patch.as_ref()),
        (HttpMethod::Post, item.post.as_ref()),
        (HttpMethod::Put, item.put.as_ref()),
        (HttpMethod::Trace, item.trace.as_ref()),
    ]
}

fn validate_raw_spec(raw: &RawSpec) -> Result<()> {
    let mut d = Diagnostics::default();
    let c = &raw.components;
    let schemas = &c.schemas;

    for (schema_name, sor) in schemas {
        validate_schema_or_ref(sor, schema_name, schemas, &mut d);
    }
    for (name, p) in &c.parameters {
        validate_parameter_or_ref(p, &format!("components.parameters.{name}"), c, &mut d);
    }
    for (name, rb) in &c.request_bodies {
        validate_request_body_or_ref(rb, &format!("components.requestBodies.{name}"), c, &mut d);
    }
    for (name, r) in &c.responses {
        validate_response_or_ref(r, &format!("components.responses.{name}"), c, &mut d);
    }

    let mut seen_ids: HashMap<&str, &str> = HashMap::new();
    for (path, item) in &raw.paths {
        for (i, p) in item.parameters.iter().enumerate() {
            validate_parameter_or_ref(p, &format!("{path} parameter[{i}]"), c, &mut d);
        }
        for (method, op) in path_operations(item).into_iter() {
            let Some(op) = op else { continue };
            let ctx = op
                .operation_id
                .clone()
                .unwrap_or_else(|| format!("{method} {path}"));

            for (i, param) in op.parameters.iter().enumerate() {
                validate_parameter_or_ref(param, &format!("{ctx} parameter[{i}]"), c, &mut d);
            }
            if let Some(rb) = &op.request_body {
                validate_request_body_or_ref(rb, &format!("{ctx} requestBody"), c, &mut d);
            }
            for (code, resp) in &op.responses {
                validate_response_or_ref(resp, &format!("{ctx} response[{code}]"), c, &mut d);
            }
            if let Some(id) = &op.operation_id
                && let Some(prev) = seen_ids.insert(id.as_str(), path.as_str())
                && prev != path.as_str()
            {
                d.error(format!(
                    "operationId `{id}` is used by both `{prev}` and `{path}`"
                ));
            }
        }
    }

    let defined_schemes: HashSet<&str> = c.security_schemes.keys().map(String::as_str).collect();
    let check_security_refs =
        |reqs: &[BTreeMap<String, Vec<String>>], location: &str, d: &mut Diagnostics| {
            for req in reqs {
                for name in req.keys() {
                    if !defined_schemes.contains(name.as_str()) {
                        d.error(format!(
                            "security requirement `{name}` at {location} references an \
                             undefined security scheme"
                        ));
                    }
                }
            }
        };
    check_security_refs(&raw.security, "top-level", &mut d);
    for (path, item) in &raw.paths {
        for (method, op) in path_operations(item).into_iter() {
            let Some(op) = op else { continue };
            if let Some(reqs) = &op.security {
                let ctx = op
                    .operation_id
                    .clone()
                    .unwrap_or_else(|| format!("{method} {path}"));
                check_security_refs(reqs, &ctx, &mut d);
            }
        }
    }

    d.into_result()
}

fn validate_parameter_or_ref(
    p: &RawParameterOrRef,
    location: &str,
    c: &RawComponents,
    d: &mut Diagnostics,
) {
    match p {
        RawParameterOrRef::Ref { reference } => {
            match parse_component_ref(reference, "parameters") {
                Ok(name) if !c.parameters.contains_key(name) => d.error(format!(
                    "{location}: $ref `{reference}` points to undefined parameter `{name}`"
                )),
                Err(e) => d.error(format!("{location}: {e}")),
                _ => {}
            }
        }
        RawParameterOrRef::Inline(param) => {
            if let Some(schema) = &param.schema {
                validate_schema_or_ref(
                    schema,
                    &format!("{location} `{}`", param.name),
                    &c.schemas,
                    d,
                );
            }
            if let Some(content) = &param.content {
                for media in content.values() {
                    if let Some(s) = &media.schema {
                        validate_schema_or_ref(
                            s,
                            &format!("{location} `{}`", param.name),
                            &c.schemas,
                            d,
                        );
                    }
                }
            }
            if !["query", "path", "header", "cookie"].contains(&param.location.as_str()) {
                d.error(format!(
                    "{location} `{}` has unsupported `in: {}`",
                    param.name, param.location
                ));
            }
        }
    }
}

fn validate_request_body_or_ref(
    rb: &RawRequestBodyOrRef,
    location: &str,
    c: &RawComponents,
    d: &mut Diagnostics,
) {
    match rb {
        RawRequestBodyOrRef::Ref { reference } => {
            match parse_component_ref(reference, "requestBodies") {
                Ok(name) if !c.request_bodies.contains_key(name) => d.error(format!(
                    "{location}: $ref `{reference}` points to undefined requestBody `{name}`"
                )),
                Err(e) => d.error(format!("{location}: {e}")),
                _ => {}
            }
        }
        RawRequestBodyOrRef::Inline(body) => {
            for (ct, media) in &body.content {
                if let Some(s) = &media.schema {
                    validate_schema_or_ref(s, &format!("{location}[{ct}]"), &c.schemas, d);
                }
            }
        }
    }
}

fn validate_response_or_ref(
    r: &RawResponseOrRef,
    location: &str,
    c: &RawComponents,
    d: &mut Diagnostics,
) {
    match r {
        RawResponseOrRef::Ref { reference } => match parse_component_ref(reference, "responses") {
            Ok(name) if !c.responses.contains_key(name) => d.error(format!(
                "{location}: $ref `{reference}` points to undefined response `{name}`"
            )),
            Err(e) => d.error(format!("{location}: {e}")),
            _ => {}
        },
        RawResponseOrRef::Inline(resp) => {
            for (ct, media) in &resp.content {
                if let Some(s) = &media.schema {
                    validate_schema_or_ref(s, &format!("{location}[{ct}]"), &c.schemas, d);
                }
            }
            for (hname, h) in &resp.headers {
                match h {
                    RawResponseHeaderOrRef::Ref { reference } => {
                        match parse_component_ref(reference, "headers") {
                            Ok(name) if !c.headers.contains_key(name) => d.error(format!(
                                "{location} header `{hname}`: $ref `{reference}` points to undefined header `{name}`"
                            )),
                            Err(e) => d.error(format!("{location} header `{hname}`: {e}")),
                            _ => {}
                        }
                    }
                    RawResponseHeaderOrRef::Inline(inline) => {
                        if let Some(s) = &inline.schema {
                            validate_schema_or_ref(
                                s,
                                &format!("{location} header `{hname}`"),
                                &c.schemas,
                                d,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Recursively validate every $ref inside a schema tree.
fn validate_schema_or_ref(
    sor: &RawSchemaOrRef,
    location: &str,
    schemas: &BTreeMap<String, RawSchemaOrRef>,
    d: &mut Diagnostics,
) {
    match sor {
        RawSchemaOrRef::Ref { reference } => match parse_schema_ref_pointer(reference) {
            Ok(name) => {
                if !schemas.contains_key(name) {
                    d.error(format!(
                        "{location}: $ref `{reference}` points to undefined schema `{name}`"
                    ));
                }
            }
            Err(e) => d.error(format!("{location}: {e}")),
        },
        RawSchemaOrRef::Inline(raw) => {
            validate_inline_schema(raw, location, schemas, d);
        }
    }
}

fn validate_inline_schema(
    raw: &RawSchema,
    location: &str,
    schemas: &BTreeMap<String, RawSchemaOrRef>,
    d: &mut Diagnostics,
) {
    for (field_name, sor) in &raw.properties {
        validate_schema_or_ref(sor, &format!("{location}.{field_name}"), schemas, d);
    }
    if let Some(items) = &raw.items {
        validate_schema_or_ref(items, &format!("{location}[items]"), schemas, d);
    }
    for (i, member) in raw.all_of.iter().enumerate() {
        validate_schema_or_ref(member, &format!("{location} allOf[{i}]"), schemas, d);
    }
    for (i, member) in raw.any_of.iter().enumerate() {
        validate_schema_or_ref(member, &format!("{location} anyOf[{i}]"), schemas, d);
    }
    for (i, member) in raw.one_of.iter().enumerate() {
        validate_schema_or_ref(member, &format!("{location} oneOf[{i}]"), schemas, d);
    }
    if let Some(RawAdditionalProperties::Schema(inner)) = &raw.additional_properties {
        validate_schema_or_ref(
            inner,
            &format!("{location}[additionalProperties]"),
            schemas,
            d,
        );
    }
    if let Some(disc) = &raw.discriminator {
        for (tag, target) in &disc.mapping {
            if target.starts_with("#/") {
                match parse_schema_ref_pointer(target) {
                    Ok(name) if !schemas.contains_key(name) => d.error(format!(
                        "{location} discriminator mapping `{tag}` → `{target}` \
                         points to undefined schema `{name}`"
                    )),
                    Err(e) => d.error(format!(
                        "{location} discriminator mapping `{tag}`: malformed $ref: {e}"
                    )),
                    _ => {}
                }
            }
        }
    }
}

// ── Lowering context ──────────────────────────────────────────────────────────

struct LoweringContext<'a> {
    components: &'a RawComponents,
    /// Schema names currently being lowered — used for recursion detection
    /// and allOf cycle checks.
    visiting: HashSet<String>,
    /// Schemas synthesised for inline objects / unions / anyOf wrappers.
    synthetic_schemas: Vec<Schema>,
    /// Every schema name in use (components + synthesised) — keeps
    /// synthesised names unique.
    taken_names: HashSet<String>,
    /// Inline schema node → synthesised name, so the same inline object
    /// reached twice (e.g. through allOf flattening) yields one class.
    synth_cache: HashMap<*const RawSchema, String>,
    /// parent name → child names, for `allOf`-style discriminated unions.
    extension_map: BTreeMap<String, Vec<String>>,
    warnings: Vec<String>,
}

impl<'a> LoweringContext<'a> {
    fn new(components: &'a RawComponents, extension_map: BTreeMap<String, Vec<String>>) -> Self {
        Self {
            components,
            visiting: HashSet::new(),
            synthetic_schemas: Vec::new(),
            taken_names: components.schemas.keys().cloned().collect(),
            synth_cache: HashMap::new(),
            extension_map,
            warnings: Vec::new(),
        }
    }

    fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }

    fn resolve_schema(&self, name: &str) -> Result<TypeRef> {
        if self.visiting.contains(name) || self.components.schemas.contains_key(name) {
            return Ok(TypeRef::Named(name.to_string()));
        }
        bail!("$ref points to undefined schema `{name}` (not present in components.schemas)")
    }

    /// Reserve a unique schema name derived from `hint`.
    fn unique_name(&mut self, hint: &str) -> String {
        let base = pascal_hint(hint);
        let mut candidate = base.clone();
        let mut n = 2;
        while self.taken_names.contains(&candidate) {
            candidate = format!("{base}{n}");
            n += 1;
        }
        self.taken_names.insert(candidate.clone());
        candidate
    }

    /// Lower an inline schema into a named synthetic schema and return a
    /// reference to it.
    fn synthesize(&mut self, hint: &str, raw: &RawSchema) -> Result<TypeRef> {
        let key = raw as *const RawSchema;
        if let Some(existing) = self.synth_cache.get(&key) {
            return Ok(TypeRef::Named(existing.clone()));
        }
        let name = self.unique_name(hint);
        self.synth_cache.insert(key, name.clone());
        self.visiting.insert(name.clone());
        let result = lower_inline_schema(&name, raw, self)
            .with_context(|| format!("in inline schema `{name}`"));
        self.visiting.remove(&name);
        let kind = result?;
        self.synthetic_schemas.push(Schema {
            name: name.clone(),
            kind,
            internal: false,
            extends: None,
            extensions: collect_extensions(&raw.extensions),
        });
        Ok(TypeRef::Named(name))
    }

    /// Register an internal single-field wrapper schema for a primitive
    /// union variant.
    fn wrapper_schema(&mut self, hint: &str, inner: TypeRef) -> TypeRef {
        let name = self.unique_name(hint);
        self.synthetic_schemas.push(Schema {
            name: name.clone(),
            kind: SchemaKind::Object {
                fields: vec![Field::new("value", inner, true)],
            },
            internal: true,
            extends: None,
            extensions: BTreeMap::new(),
        });
        TypeRef::Named(name)
    }

    fn resolve_parameter<'b>(&self, p: &'b RawParameterOrRef) -> Result<&'b RawParameter>
    where
        'a: 'b,
    {
        let mut current = p;
        for _ in 0..16 {
            match current {
                RawParameterOrRef::Inline(param) => return Ok(param),
                RawParameterOrRef::Ref { reference } => {
                    let name = parse_component_ref(reference, "parameters")?;
                    current = self.components.parameters.get(name).ok_or_else(|| {
                        anyhow!("$ref `{reference}` points to undefined parameter `{name}`")
                    })?;
                }
            }
        }
        bail!("parameter $ref chain is too deep (cycle?)")
    }

    fn resolve_request_body<'b>(&self, rb: &'b RawRequestBodyOrRef) -> Result<&'b RawRequestBody>
    where
        'a: 'b,
    {
        let mut current = rb;
        for _ in 0..16 {
            match current {
                RawRequestBodyOrRef::Inline(body) => return Ok(body),
                RawRequestBodyOrRef::Ref { reference } => {
                    let name = parse_component_ref(reference, "requestBodies")?;
                    current = self.components.request_bodies.get(name).ok_or_else(|| {
                        anyhow!("$ref `{reference}` points to undefined requestBody `{name}`")
                    })?;
                }
            }
        }
        bail!("requestBody $ref chain is too deep (cycle?)")
    }

    fn resolve_response<'b>(&self, r: &'b RawResponseOrRef) -> Result<&'b RawResponse>
    where
        'a: 'b,
    {
        let mut current = r;
        for _ in 0..16 {
            match current {
                RawResponseOrRef::Inline(resp) => return Ok(resp),
                RawResponseOrRef::Ref { reference } => {
                    let name = parse_component_ref(reference, "responses")?;
                    current = self.components.responses.get(name).ok_or_else(|| {
                        anyhow!("$ref `{reference}` points to undefined response `{name}`")
                    })?;
                }
            }
        }
        bail!("response $ref chain is too deep (cycle?)")
    }

    fn resolve_header<'b>(&self, h: &'b RawResponseHeaderOrRef) -> Result<&'b RawResponseHeader>
    where
        'a: 'b,
    {
        let mut current = h;
        for _ in 0..16 {
            match current {
                RawResponseHeaderOrRef::Inline(header) => return Ok(header),
                RawResponseHeaderOrRef::Ref { reference } => {
                    let name = parse_component_ref(reference, "headers")?;
                    current = self.components.headers.get(name).ok_or_else(|| {
                        anyhow!("$ref `{reference}` points to undefined header `{name}`")
                    })?;
                }
            }
        }
        bail!("header $ref chain is too deep (cycle?)")
    }
}

/// Turn an arbitrary hint (`getPets`, `user-profile`, `Pet.owner`) into a
/// PascalCase identifier fragment. Emitters sanitise names again, but a clean
/// hint keeps synthesised class names readable and deterministic.
fn pascal_hint(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper_next = true;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            if upper_next {
                out.extend(ch.to_uppercase());
                upper_next = false;
            } else {
                out.push(ch);
            }
        } else {
            upper_next = true;
        }
    }
    if out.is_empty() {
        out.push_str("Inline");
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'N');
    }
    out
}

fn build_allof_extension_map(
    schemas: &BTreeMap<String, RawSchemaOrRef>,
) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (child_name, sor) in schemas {
        let RawSchemaOrRef::Inline(raw) = sor else {
            continue;
        };
        for member in &raw.all_of {
            let RawSchemaOrRef::Ref { reference } = member else {
                continue;
            };
            let Ok(parent_name) = parse_schema_ref_pointer(reference) else {
                continue;
            };
            map.entry(parent_name.to_string())
                .or_default()
                .push(child_name.clone());
        }
    }
    map
}

// ── Enum values ───────────────────────────────────────────────────────────────

/// Lower `enum:` entries. `null` entries are dropped (they only express
/// nullability, which is tracked separately); unsupported value kinds are
/// reported through the returned warning rather than aborting.
fn lower_enum_values(raw: &[serde_yaml::Value]) -> (Vec<EnumValue>, Option<String>) {
    let mut values = Vec::with_capacity(raw.len());
    let mut warning = None;
    for v in raw {
        match v {
            serde_yaml::Value::String(s) => values.push(EnumValue::Str(s.clone())),
            serde_yaml::Value::Number(n) => match n.as_i64() {
                Some(i) => values.push(EnumValue::Int(i)),
                None => values.push(EnumValue::Str(n.to_string())),
            },
            serde_yaml::Value::Bool(b) => values.push(EnumValue::Str(b.to_string())),
            serde_yaml::Value::Null => {}
            other => {
                warning = Some(format!(
                    "enum value `{other:?}` is not a string or integer and was ignored"
                ));
            }
        }
    }
    // Deduplicate while preserving order.
    let mut seen = HashSet::new();
    values.retain(|v| seen.insert(v.clone()));
    (values, warning)
}

// ── Default values ────────────────────────────────────────────────────────────

fn lower_default_value(
    raw: &Option<serde_yaml::Value>,
    type_ref: &TypeRef,
) -> Option<flap_ir::DefaultValue> {
    use flap_ir::DefaultValue;
    let val = raw.as_ref()?;
    match type_ref {
        TypeRef::String | TypeRef::Enum(_) => match val {
            serde_yaml::Value::String(s) => Some(DefaultValue::String(s.clone())),
            serde_yaml::Value::Number(n) if matches!(type_ref, TypeRef::Enum(_)) => {
                n.as_i64().map(DefaultValue::Integer)
            }
            _ => None,
        },
        TypeRef::Integer { .. } => match val {
            serde_yaml::Value::Number(n) => n.as_i64().map(DefaultValue::Integer),
            _ => None,
        },
        TypeRef::Number { .. } => match val {
            serde_yaml::Value::Number(n) => n.as_f64().map(DefaultValue::Number),
            _ => None,
        },
        TypeRef::Boolean => match val {
            serde_yaml::Value::Bool(b) => Some(DefaultValue::Boolean(*b)),
            _ => None,
        },
        _ => None,
    }
}

// ── Top-level lowering ────────────────────────────────────────────────────────

fn lower(raw: RawSpec) -> Result<Api> {
    validate_raw_spec(&raw)?;
    let extensions = collect_extensions(&raw.extensions);
    let title = raw
        .info
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Api".to_string());
    let base_urls: Vec<String> = raw.servers.iter().map(expand_server_url).collect();
    let extension_map = build_allof_extension_map(&raw.components.schemas);
    let mut ctx = LoweringContext::new(&raw.components, extension_map);
    let operations = lower_operations(&raw.paths, &mut ctx)?;
    let schemas = lower_schemas(&raw.components.schemas, &mut ctx)?;
    let security_schemes = lower_security_schemes(&raw.components.security_schemes, &mut ctx);
    let security = flatten_security_requirements(&raw.security);
    Ok(Api {
        title,
        base_urls,
        operations,
        schemas,
        security_schemes,
        security,
        extensions,
        warnings: ctx.warnings,
    })
}

/// Substitute `{variable}` placeholders in a server URL with their defaults.
fn expand_server_url(server: &RawServer) -> String {
    let mut url = server.url.clone();
    for (name, var) in &server.variables {
        let placeholder = format!("{{{name}}}");
        url = url.replace(&placeholder, &scalar_to_string(&var.default));
    }
    url
}

// ── Security ──────────────────────────────────────────────────────────────────

fn lower_security_schemes(
    raw: &BTreeMap<String, RawSecurityScheme>,
    ctx: &mut LoweringContext,
) -> Vec<SecurityScheme> {
    let mut out = Vec::with_capacity(raw.len());
    for (name, scheme) in raw {
        match lower_security_scheme(name, scheme) {
            Ok(Some(lowered)) => out.push(lowered),
            Ok(None) => {}
            Err(e) => ctx.warn(format!("security scheme `{name}` was skipped: {e}")),
        }
    }
    out
}

fn lower_security_scheme(name: &str, raw: &RawSecurityScheme) -> Result<Option<SecurityScheme>> {
    let kind = match raw.ty.as_str() {
        "apiKey" => {
            let parameter_name = raw.name.clone().ok_or_else(|| {
                anyhow!("apiKey security scheme is missing the required `name` field")
            })?;
            let location_str = raw.location.as_deref().ok_or_else(|| {
                anyhow!("apiKey security scheme is missing the required `in` field")
            })?;
            let location = match location_str {
                "header" => ApiKeyLocation::Header,
                "query" => ApiKeyLocation::Query,
                "cookie" => ApiKeyLocation::Cookie,
                other => bail!(
                    "apiKey `in: {other}` is invalid (expected `header`, `query`, or `cookie`)"
                ),
            };
            SecuritySchemeKind::ApiKey {
                parameter_name,
                location,
            }
        }
        "http" => {
            let scheme = raw.scheme.as_deref().unwrap_or("");
            if scheme.eq_ignore_ascii_case("bearer") {
                SecuritySchemeKind::HttpBearer {
                    bearer_format: raw.bearer_format.clone(),
                }
            } else if scheme.eq_ignore_ascii_case("basic") {
                SecuritySchemeKind::HttpBasic
            } else if scheme.is_empty() {
                bail!("http security scheme is missing the required `scheme` field")
            } else {
                bail!("http `scheme: {scheme}` is not supported (only `bearer` and `basic` are)")
            }
        }
        "oauth2" => {
            let raw_flows = raw.flows.as_ref().ok_or_else(|| {
                anyhow!("oauth2 security scheme is missing the required `flows` block")
            })?;
            let flows = lower_oauth2_flows(name, raw_flows)?;
            SecuritySchemeKind::OAuth2 { flows }
        }
        "openIdConnect" => {
            let openid_connect_url = raw.open_id_connect_url.clone().ok_or_else(|| {
                anyhow!(
                    "openIdConnect security scheme is missing the required `openIdConnectUrl` field"
                )
            })?;
            SecuritySchemeKind::OpenIdConnect { openid_connect_url }
        }
        other => bail!("unsupported security scheme type `{other}`"),
    };
    Ok(Some(SecurityScheme {
        name: name.to_string(),
        kind,
    }))
}

fn lower_oauth2_flows(scheme_name: &str, raw: &RawOAuth2Flows) -> Result<Vec<OAuth2Flow>> {
    let mut flows = Vec::new();

    if let Some(f) = &raw.implicit {
        let authorization_url = f.authorization_url.clone().ok_or_else(|| {
            anyhow!("`implicit` flow in oauth2 scheme `{scheme_name}` requires `authorizationUrl`")
        })?;
        flows.push(OAuth2Flow {
            flow_type: OAuth2FlowType::Implicit,
            token_url: None,
            authorization_url: Some(authorization_url),
            scopes: f.scopes.keys().cloned().collect(),
        });
    }
    if let Some(f) = &raw.password {
        let token_url = f.token_url.clone().ok_or_else(|| {
            anyhow!("`password` flow in oauth2 scheme `{scheme_name}` requires `tokenUrl`")
        })?;
        flows.push(OAuth2Flow {
            flow_type: OAuth2FlowType::Password,
            token_url: Some(token_url),
            authorization_url: None,
            scopes: f.scopes.keys().cloned().collect(),
        });
    }
    if let Some(f) = &raw.client_credentials {
        let token_url = f.token_url.clone().ok_or_else(|| {
            anyhow!("`clientCredentials` flow in oauth2 scheme `{scheme_name}` requires `tokenUrl`")
        })?;
        flows.push(OAuth2Flow {
            flow_type: OAuth2FlowType::ClientCredentials,
            token_url: Some(token_url),
            authorization_url: None,
            scopes: f.scopes.keys().cloned().collect(),
        });
    }
    if let Some(f) = &raw.authorization_code {
        let token_url = f.token_url.clone().ok_or_else(|| {
            anyhow!("`authorizationCode` flow in oauth2 scheme `{scheme_name}` requires `tokenUrl`")
        })?;
        let authorization_url = f.authorization_url.clone().ok_or_else(|| {
            anyhow!("`authorizationCode` flow in oauth2 scheme `{scheme_name}` requires `authorizationUrl`")
        })?;
        flows.push(OAuth2Flow {
            flow_type: OAuth2FlowType::AuthorizationCode,
            token_url: Some(token_url),
            authorization_url: Some(authorization_url),
            scopes: f.scopes.keys().cloned().collect(),
        });
    }

    if flows.is_empty() {
        bail!(
            "oauth2 scheme `{scheme_name}` defines no recognised flows \
             (expected at least one of: implicit, password, clientCredentials, authorizationCode)"
        );
    }
    Ok(flows)
}

fn flatten_security_requirements(reqs: &[BTreeMap<String, Vec<String>>]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for req in reqs {
        for name in req.keys() {
            if seen.insert(name.clone()) {
                out.push(name.clone());
            }
        }
    }
    out
}

// ── Operation lowering ────────────────────────────────────────────────────────

/// Naming hint for schemas synthesised inside an operation: the operationId
/// when present, otherwise `<method><Path>` (mirrors the emitter's method
/// naming so generated class names read naturally).
fn operation_hint(method: HttpMethod, path: &str, operation_id: Option<&str>) -> String {
    if let Some(id) = operation_id
        && !id.trim().is_empty()
    {
        return pascal_hint(id);
    }
    let mut hint = method.as_str().to_ascii_lowercase();
    hint.push_str(&pascal_hint(
        &path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_matches(|c| c == '{' || c == '}'))
            .collect::<Vec<_>>()
            .join(" "),
    ));
    pascal_hint(&hint)
}

fn lower_operations(
    paths: &BTreeMap<String, RawPathItem>,
    ctx: &mut LoweringContext,
) -> Result<Vec<Operation>> {
    let mut ops = Vec::new();
    for (path, item) in paths {
        for (method, maybe_op) in path_operations(item).into_iter() {
            let Some(raw_op) = maybe_op else { continue };
            let hint = operation_hint(method, path, raw_op.operation_id.as_deref());

            // Path-level parameters first; operation-level overrides by (name, in).
            let mut merged: Vec<&RawParameter> = Vec::new();
            for p in &item.parameters {
                merged.push(ctx.resolve_parameter(p)?);
            }
            for p in &raw_op.parameters {
                let param = ctx.resolve_parameter(p)?;
                merged.retain(|m| !(m.name == param.name && m.location == param.location));
                merged.push(param);
            }

            let mut parameters = Vec::with_capacity(merged.len());
            for (i, p) in merged.iter().enumerate() {
                let lowered = lower_parameter(&hint, p, ctx)
                    .with_context(|| format!("parameter[{i}] `{}` of {method} {path}", p.name))?;
                parameters.push(lowered);
            }

            let request_body = match &raw_op.request_body {
                Some(rb) => {
                    let body = ctx.resolve_request_body(rb)?;
                    lower_request_body(&hint, path, method, body, ctx)
                        .with_context(|| format!("requestBody of {method} {path}"))?
                }
                None => None,
            };

            let responses = lower_responses(&hint, path, method, &raw_op.responses, ctx)?;

            let security = raw_op
                .security
                .as_ref()
                .map(|reqs| flatten_security_requirements(reqs));

            let summary = raw_op
                .summary
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| {
                    raw_op
                        .description
                        .as_deref()
                        .and_then(|d| d.lines().find(|l| !l.trim().is_empty()))
                        .map(|l| l.trim().to_string())
                });

            ops.push(Operation {
                method,
                path: path.clone(),
                operation_id: raw_op.operation_id.clone(),
                summary,
                parameters,
                request_body,
                responses,
                security,
                deprecated: raw_op.deprecated,
                extensions: collect_extensions(&raw_op.extensions),
            });
        }
    }
    Ok(ops)
}

fn lower_parameter(
    op_hint: &str,
    raw: &RawParameter,
    ctx: &mut LoweringContext,
) -> Result<Parameter> {
    let location = match raw.location.as_str() {
        "query" => ParameterLocation::Query,
        "path" => ParameterLocation::Path,
        "header" => ParameterLocation::Header,
        "cookie" => ParameterLocation::Cookie,
        other => bail!(
            "unsupported parameter location `{other}` (expected query | path | header | cookie)"
        ),
    };

    let required = location == ParameterLocation::Path || raw.required;
    let hint = format!("{op_hint}{}", pascal_hint(&raw.name));

    let schema = raw.schema.as_ref().or_else(|| {
        raw.content
            .as_ref()
            .and_then(|c| c.values().next())
            .and_then(|m| m.schema.as_ref())
    });

    let type_ref = match schema {
        Some(sor) => lower_type_ref(&hint, sor, ctx)
            .with_context(|| format!("schema of parameter `{}`", raw.name))?,
        None => {
            ctx.warn(format!(
                "parameter `{}` of `{op_hint}` has no `schema` — typed as `dynamic`",
                raw.name
            ));
            TypeRef::Any
        }
    };

    Ok(Parameter {
        name: raw.name.clone(),
        location,
        type_ref,
        required,
        extensions: collect_extensions(&raw.extensions),
    })
}

/// Choose which media type of a request body to generate for. JSON is
/// preferred, then multipart, then form-urlencoded, then whatever is first.
fn pick_request_body_content(
    content: &BTreeMap<String, RawMediaType>,
) -> Option<(String, &RawMediaType)> {
    let find = |pred: &dyn Fn(&str) -> bool| {
        content
            .iter()
            .find(|(k, _)| pred(&k.to_ascii_lowercase()))
            .map(|(k, v)| (k.clone(), v))
    };
    find(&|k| k.starts_with("application/json") || k.ends_with("+json"))
        .or_else(|| find(&|k| k.starts_with("multipart/")))
        .or_else(|| find(&|k| k == "application/x-www-form-urlencoded"))
        .or_else(|| content.iter().next().map(|(k, v)| (k.clone(), v)))
}

fn lower_request_body(
    op_hint: &str,
    path: &str,
    method: HttpMethod,
    raw: &RawRequestBody,
    ctx: &mut LoweringContext,
) -> Result<Option<RequestBody>> {
    let Some((content_type, media_type)) = pick_request_body_content(&raw.content) else {
        ctx.warn(format!(
            "requestBody of {method} {path} has no content entries — ignored"
        ));
        return Ok(None);
    };

    let schema_ref = match &media_type.schema {
        Some(sor) => lower_type_ref(&format!("{op_hint}Body"), sor, ctx)?,
        None => {
            ctx.warn(format!(
                "requestBody of {method} {path} (`{content_type}`) has no schema — typed as `dynamic`"
            ));
            TypeRef::Any
        }
    };

    let is_multipart = content_type.to_ascii_lowercase().starts_with("multipart/");

    Ok(Some(RequestBody {
        content_type,
        schema_ref,
        required: raw.required,
        is_multipart,
        extensions: collect_extensions(&raw.extensions),
    }))
}

// ── Response lowering ─────────────────────────────────────────────────────────

fn lower_responses(
    op_hint: &str,
    path: &str,
    method: HttpMethod,
    raw: &BTreeMap<String, RawResponseOrRef>,
    ctx: &mut LoweringContext,
) -> Result<Vec<Response>> {
    let mut keys: Vec<&String> = raw.keys().collect();
    keys.sort_by(|a, b| {
        let key = |s: &str| -> (u8, i64, String) {
            if s == "default" {
                (2, 0, String::new())
            } else if let Ok(n) = s.parse::<i64>() {
                (0, n, String::new())
            } else {
                (1, 0, s.to_string())
            }
        };
        key(a).cmp(&key(b))
    });

    let mut out = Vec::with_capacity(raw.len());
    for status_code in keys {
        let raw_resp = ctx.resolve_response(&raw[status_code])?;
        let response = lower_response(op_hint, status_code, raw_resp, ctx)
            .with_context(|| format!("response `{status_code}` of {method} {path}"))?;
        out.push(response);
    }
    Ok(out)
}

fn lower_response(
    op_hint: &str,
    status_code: &str,
    raw: &RawResponse,
    ctx: &mut LoweringContext,
) -> Result<Response> {
    let media_type = raw
        .content
        .iter()
        .find(|(k, _)| {
            let k = k.to_ascii_lowercase();
            k.starts_with("application/json") || k.ends_with("+json")
        })
        .map(|(_, v)| v)
        .or_else(|| raw.content.values().next());

    let hint = if status_code.starts_with('2') {
        format!("{op_hint}Response")
    } else {
        format!("{op_hint}{}Response", pascal_hint(status_code))
    };

    let content_type = raw
        .content
        .iter()
        .find(|(_, v)| media_type.is_some_and(|m| std::ptr::eq(*v, m)))
        .map(|(k, _)| k.clone());

    let schema_ref = match media_type.and_then(|m| m.schema.as_ref()) {
        Some(sor) => Some(
            lower_type_ref(&hint, sor, ctx)
                .with_context(|| format!("schema of response `{status_code}`"))?,
        ),
        None => None,
    };

    let mut headers: Vec<flap_ir::ResponseHeader> = Vec::new();
    for (header_name, raw_header) in &raw.headers {
        if header_name.eq_ignore_ascii_case("authorization")
            || header_name.to_lowercase().starts_with("content-")
        {
            continue;
        }
        let header = match ctx.resolve_header(raw_header) {
            Ok(h) => h,
            Err(e) => {
                ctx.warn(format!(
                    "response header `{header_name}` of `{op_hint}` {status_code} skipped: {e}"
                ));
                continue;
            }
        };
        let Some(schema) = header.schema.as_ref() else {
            ctx.warn(format!(
                "response header `{header_name}` of `{op_hint}` {status_code} has no schema — skipped"
            ));
            continue;
        };
        let type_ref = lower_type_ref(&format!("{hint}{}", pascal_hint(header_name)), schema, ctx)
            .with_context(|| format!("schema of response header `{header_name}`"))?;
        if matches!(
            type_ref,
            TypeRef::Named(_) | TypeRef::Map(_) | TypeRef::Any | TypeRef::Binary
        ) {
            ctx.warn(format!(
                "response header `{header_name}` of `{op_hint}` {status_code} is not a scalar — skipped"
            ));
            continue;
        }
        headers.push(flap_ir::ResponseHeader {
            name: header_name.clone(),
            type_ref,
            required: header.required,
        });
    }
    headers.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Response {
        status_code: status_code.to_string(),
        content_type,
        schema_ref,
        headers,
        extensions: collect_extensions(&raw.extensions),
    })
}

// ── Schema lowering ───────────────────────────────────────────────────────────

fn lower_schemas(
    raw: &BTreeMap<String, RawSchemaOrRef>,
    ctx: &mut LoweringContext,
) -> Result<Vec<Schema>> {
    let mut out = Vec::with_capacity(raw.len());
    for (name, schema_or_ref) in raw {
        ctx.visiting.insert(name.clone());
        let result = lower_schema_kind(name, schema_or_ref, ctx)
            .with_context(|| format!("in schema `{name}`"));
        ctx.visiting.remove(name);
        let kind = result?;

        let extends = if let RawSchemaOrRef::Inline(raw_schema) = schema_or_ref {
            raw_schema.all_of.iter().find_map(|member| {
                if let RawSchemaOrRef::Ref { reference } = member {
                    parse_schema_ref_pointer(reference).ok().map(str::to_string)
                } else {
                    None
                }
            })
        } else {
            None
        };

        let extensions = match schema_or_ref {
            RawSchemaOrRef::Inline(raw) => collect_extensions(&raw.extensions),
            RawSchemaOrRef::Ref { .. } => BTreeMap::new(),
        };

        out.push(Schema {
            name: name.clone(),
            kind,
            internal: false,
            extends,
            extensions,
        });
    }
    out.append(&mut ctx.synthetic_schemas);
    Ok(out)
}

fn lower_schema_kind(
    name: &str,
    sor: &RawSchemaOrRef,
    ctx: &mut LoweringContext,
) -> Result<SchemaKind> {
    match sor {
        RawSchemaOrRef::Ref { reference } => {
            let target = parse_schema_ref_pointer(reference)
                .with_context(|| format!("top-level schema `{name}` is a bare $ref"))?;
            if !ctx.components.schemas.contains_key(target) {
                bail!(
                    "top-level schema `{name}` aliases `{target}` which is not defined in components.schemas"
                );
            }
            if target == name {
                bail!("schema `{name}` is a $ref to itself");
            }
            Ok(SchemaKind::Alias {
                target: target.to_string(),
            })
        }
        RawSchemaOrRef::Inline(raw) => lower_inline_schema(name, raw, ctx),
    }
}

/// `oneOf`/`anyOf` used purely to express nullability
/// (`oneOf: [{$ref: X}, {type: 'null'}]`) collapses to the single non-null
/// variant. Returns `None` when the list is a real union (or empty).
fn single_non_null_variant(raw: &RawSchema) -> Option<&RawSchemaOrRef> {
    let list = if !raw.one_of.is_empty() {
        &raw.one_of
    } else if !raw.any_of.is_empty() {
        &raw.any_of
    } else {
        return None;
    };
    let non_null: Vec<&RawSchemaOrRef> = list.iter().filter(|v| !is_null_schema(v)).collect();
    if non_null.len() == 1 && non_null.len() < list.len() {
        Some(non_null[0])
    } else {
        None
    }
}

/// `allOf: [X]` with nothing else on the schema is just `X` (commonly used
/// as `nullable: true` + `allOf: [$ref]` to make a reference nullable).
fn single_allof_member(raw: &RawSchema) -> Option<&RawSchemaOrRef> {
    if raw.all_of.len() == 1
        && raw.properties.is_empty()
        && raw.any_of.is_empty()
        && raw.one_of.is_empty()
        && raw.enum_values.is_empty()
        && raw.additional_properties.is_none()
        && raw.required.is_empty()
        && raw.discriminator.is_none()
    {
        raw.all_of.first()
    } else {
        None
    }
}

fn is_null_schema(sor: &RawSchemaOrRef) -> bool {
    match sor {
        RawSchemaOrRef::Inline(raw) => {
            raw.ty.len() == 1
                && raw.ty[0] == "null"
                && raw.properties.is_empty()
                && raw.all_of.is_empty()
                && raw.any_of.is_empty()
                && raw.one_of.is_empty()
        }
        RawSchemaOrRef::Ref { .. } => false,
    }
}

fn has_null_variant(raw: &RawSchema) -> bool {
    raw.one_of
        .iter()
        .chain(raw.any_of.iter())
        .any(is_null_schema)
}

fn lower_inline_schema(
    name: &str,
    raw: &RawSchema,
    ctx: &mut LoweringContext,
) -> Result<SchemaKind> {
    if let Some(single) = single_non_null_variant(raw) {
        return match single {
            RawSchemaOrRef::Ref { reference } => {
                let target = parse_schema_ref_pointer(reference)?;
                if target == name {
                    bail!("schema `{name}` is a nullable $ref to itself");
                }
                ctx.resolve_schema(target)?;
                Ok(SchemaKind::Alias {
                    target: target.to_string(),
                })
            }
            RawSchemaOrRef::Inline(inner) => lower_inline_schema(name, inner, ctx),
        };
    }

    let is_union_child = ctx
        .extension_map
        .values()
        .any(|children| children.iter().any(|c| c == name));
    if let Some(member) = single_allof_member(raw)
        && !is_union_child
    {
        return match member {
            RawSchemaOrRef::Ref { reference } => {
                let target = parse_schema_ref_pointer(reference)?;
                if target == name {
                    bail!("schema `{name}` is an allOf reference to itself");
                }
                ctx.resolve_schema(target)?;
                Ok(SchemaKind::Alias {
                    target: target.to_string(),
                })
            }
            RawSchemaOrRef::Inline(inner) => lower_inline_schema(name, inner, ctx),
        };
    }

    if !raw.any_of.is_empty() {
        return lower_untagged_union(name, &raw.any_of, ctx);
    }

    if !raw.one_of.is_empty() {
        return lower_one_of(name, raw, ctx);
    }

    if let Some(discriminator) = &raw.discriminator
        && let Some(children) = ctx.extension_map.get(name).cloned()
    {
        return lower_allof_union(name, discriminator, &children, ctx);
    }

    if !raw.all_of.is_empty() {
        let fields = collect_object_fields(name, raw, ctx)?;
        return Ok(SchemaKind::Object { fields });
    }

    if !raw.enum_values.is_empty() {
        let (values, warning) = lower_enum_values(&raw.enum_values);
        if let Some(w) = warning {
            ctx.warn(format!("schema `{name}`: {w}"));
        }
        if !values.is_empty() {
            return Ok(SchemaKind::Enum { values });
        }
    }

    match primary_type(&raw.ty) {
        Some("object") | None if !raw.properties.is_empty() => {
            let fields = collect_object_fields(name, raw, ctx)?;
            Ok(SchemaKind::Object { fields })
        }
        Some("object") | None => match &raw.additional_properties {
            Some(RawAdditionalProperties::Schema(inner)) => {
                let value = lower_type_ref(&format!("{name}Value"), inner, ctx)
                    .with_context(|| format!("in `{name}.additionalProperties`"))?;
                Ok(SchemaKind::Map { value })
            }
            Some(RawAdditionalProperties::Bool(true)) => Ok(SchemaKind::Map {
                value: TypeRef::Any,
            }),
            _ if primary_type(&raw.ty) == Some("object") => Ok(SchemaKind::Map {
                value: TypeRef::Any,
            }),
            _ => Ok(SchemaKind::Primitive {
                type_ref: TypeRef::Any,
            }),
        },
        Some("array") => {
            let item = match &raw.items {
                Some(items) => lower_type_ref(&format!("{name}Item"), items, ctx)
                    .with_context(|| format!("in `{name}.items`"))?,
                None => {
                    ctx.warn(format!(
                        "array schema `{name}` has no `items` — element type is `dynamic`"
                    ));
                    TypeRef::Any
                }
            };
            Ok(SchemaKind::Array { item })
        }
        Some(_) => {
            let type_ref = lower_primitive(name, raw, ctx);
            Ok(SchemaKind::Primitive { type_ref })
        }
    }
}

fn lower_primitive(name: &str, raw: &RawSchema, ctx: &mut LoweringContext) -> TypeRef {
    match primary_type(&raw.ty) {
        Some("string") => match raw.format.as_deref() {
            Some("date-time") => TypeRef::DateTime,
            Some("binary") => TypeRef::Binary,
            _ => TypeRef::String,
        },
        Some("integer") => TypeRef::Integer {
            format: raw.format.clone(),
        },
        Some("number") => TypeRef::Number {
            format: raw.format.clone(),
        },
        Some("boolean") => TypeRef::Boolean,
        Some("null") | None => TypeRef::Any,
        Some(other) => {
            ctx.warn(format!(
                "`{name}` has unsupported type `{other}` — typed as `dynamic`"
            ));
            TypeRef::Any
        }
    }
}

fn lower_untagged_union(
    parent_name: &str,
    members: &[RawSchemaOrRef],
    ctx: &mut LoweringContext,
) -> Result<SchemaKind> {
    let mut variants = Vec::with_capacity(members.len());
    for (i, sor) in members.iter().enumerate() {
        if is_null_schema(sor) {
            continue;
        }
        let hint = format!("{parent_name}Variant{i}");
        let type_ref = lower_type_ref(&hint, sor, ctx)?;
        match type_ref {
            TypeRef::Named(n) => variants.push(TypeRef::Named(n)),
            other => variants.push(ctx.wrapper_schema(&hint, other)),
        }
    }
    if variants.is_empty() {
        bail!("union `{parent_name}` has no non-null variants");
    }
    Ok(SchemaKind::UntaggedUnion { variants })
}

fn lower_allof_union(
    name: &str,
    discriminator: &RawDiscriminator,
    children: &[String],
    ctx: &LoweringContext,
) -> Result<SchemaKind> {
    let property_name = discriminator.property_name.trim();
    if property_name.is_empty() {
        bail!(
            "schema `{name}` has a `discriminator` with an empty `propertyName` — \
             set it to the wire-side field whose value selects the variant."
        );
    }

    let mut tag_by_schema: BTreeMap<String, String> = BTreeMap::new();
    for (wire_tag, schema_ref) in &discriminator.mapping {
        let bare = parse_mapping_target(schema_ref)
            .with_context(|| format!("discriminator mapping entry `{wire_tag}` of `{name}`"))?;
        tag_by_schema.insert(bare.to_string(), wire_tag.clone());
    }

    let mut variants = Vec::with_capacity(children.len());
    let mut variant_tags = Vec::with_capacity(children.len());
    for child_name in children {
        if !ctx.components.schemas.contains_key(child_name) {
            bail!(
                "schema `{name}` has discriminator child `{child_name}` that is not present in components.schemas"
            );
        }
        let wire_tag = tag_by_schema
            .get(child_name)
            .cloned()
            .unwrap_or_else(|| child_name.clone());
        variants.push(TypeRef::Named(child_name.clone()));
        variant_tags.push(wire_tag);
    }

    Ok(SchemaKind::Union {
        variants,
        discriminator: property_name.to_string(),
        variant_tags,
    })
}

fn lower_one_of(name: &str, raw: &RawSchema, ctx: &mut LoweringContext) -> Result<SchemaKind> {
    let Some(discriminator) = &raw.discriminator else {
        return lower_untagged_union(name, &raw.one_of, ctx);
    };

    let property_name = discriminator.property_name.trim();
    if property_name.is_empty() {
        bail!(
            "schema `{name}` has a `discriminator` with an empty `propertyName` — \
             set it to the wire-side field whose value selects the variant."
        );
    }

    let mut tag_by_schema: BTreeMap<String, String> = BTreeMap::new();
    for (wire_tag, schema_ref) in &discriminator.mapping {
        let bare = parse_mapping_target(schema_ref)
            .with_context(|| format!("discriminator mapping entry `{wire_tag}` of `{name}`"))?;
        tag_by_schema.insert(bare.to_string(), wire_tag.clone());
    }

    let mut variants = Vec::with_capacity(raw.one_of.len());
    let mut variant_tags = Vec::with_capacity(raw.one_of.len());
    for (i, member) in raw.one_of.iter().enumerate() {
        if is_null_schema(member) {
            continue;
        }
        let variant_name = match member {
            RawSchemaOrRef::Ref { reference } => {
                let bare = parse_schema_ref_pointer(reference)
                    .with_context(|| format!("oneOf[{i}] of `{name}`"))?;
                ctx.resolve_schema(bare)
                    .with_context(|| format!("oneOf[{i}] of `{name}` references `{bare}`"))?;
                bare.to_string()
            }
            RawSchemaOrRef::Inline(inline) => {
                // Inline variants get a synthesised class; the discriminator
                // tag defaults to that class name unless mapped explicitly.
                match ctx.synthesize(&format!("{name}Variant{i}"), inline)? {
                    TypeRef::Named(n) => n,
                    _ => unreachable!("synthesize always returns TypeRef::Named"),
                }
            }
        };
        let wire_tag = tag_by_schema
            .get(&variant_name)
            .cloned()
            .unwrap_or_else(|| variant_name.clone());
        variants.push(TypeRef::Named(variant_name));
        variant_tags.push(wire_tag);
    }

    if variants.is_empty() {
        bail!("union `{name}` has no non-null variants");
    }

    Ok(SchemaKind::Union {
        variants,
        discriminator: property_name.to_string(),
        variant_tags,
    })
}

fn parse_mapping_target(value: &str) -> Result<&str> {
    if value.starts_with("#/") {
        return parse_schema_ref_pointer(value);
    }
    if value.is_empty() || value.contains('/') {
        bail!("malformed mapping target `{value}` — expected schema name or $ref");
    }
    Ok(value)
}

/// Collect the fields of an object schema, flattening `allOf` members.
/// `hint` names the owning schema and prefixes any synthesised inline types.
fn collect_object_fields(
    hint: &str,
    raw: &RawSchema,
    ctx: &mut LoweringContext,
) -> Result<Vec<Field>> {
    let mut fields: Vec<Field> = Vec::new();

    for (i, member) in raw.all_of.iter().enumerate() {
        let member_fields =
            collect_member_fields(hint, member, ctx).with_context(|| format!("allOf[{i}]"))?;
        fields.extend(member_fields);
    }

    let own_required: HashSet<&str> = raw.required.iter().map(String::as_str).collect();
    for (field_name, sor) in &raw.properties {
        let field_hint = format!("{hint}{}", pascal_hint(field_name));
        let type_ref = lower_type_ref(&field_hint, sor, ctx)
            .with_context(|| format!("field `{field_name}`"))?;
        let is_required = own_required.contains(field_name.as_str());

        let is_nullable = match sor {
            RawSchemaOrRef::Inline(raw) => {
                raw.nullable.unwrap_or(false)
                    || is_nullable(&raw.ty)
                    || has_null_variant(raw)
                    || raw.enum_values.iter().any(|v| v.is_null())
            }
            RawSchemaOrRef::Ref { .. } => false,
        };

        let is_recursive = type_ref_is_recursive(&type_ref, &ctx.visiting);

        let default_value = match sor {
            RawSchemaOrRef::Inline(raw) => lower_default_value(&raw.default, &type_ref),
            RawSchemaOrRef::Ref { .. } => None,
        };

        let field_extensions = match sor {
            RawSchemaOrRef::Inline(raw) => collect_extensions(&raw.extensions),
            RawSchemaOrRef::Ref { .. } => BTreeMap::new(),
        };

        fields.push(Field {
            name: field_name.clone(),
            type_ref,
            required: is_required,
            nullable: is_nullable,
            is_recursive,
            default_value,
            extensions: field_extensions,
        });
    }

    // `allOf: [{$ref: Base}, {required: [name]}]` — a `required` list that
    // names inherited properties promotes them.
    for field in &mut fields {
        if own_required.contains(field.name.as_str()) {
            field.required = true;
        }
    }

    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut deduped: Vec<Field> = Vec::with_capacity(fields.len());
    for field in fields {
        if let Some(&idx) = seen.get(&field.name) {
            let prev = &deduped[idx];
            let merged = Field {
                required: prev.required || field.required,
                nullable: prev.nullable || field.nullable,
                is_recursive: prev.is_recursive || field.is_recursive,
                default_value: field
                    .default_value
                    .clone()
                    .or_else(|| prev.default_value.clone()),
                extensions: {
                    let mut m = prev.extensions.clone();
                    m.extend(field.extensions.clone());
                    m
                },
                ..field
            };
            deduped[idx] = merged;
        } else {
            seen.insert(field.name.clone(), deduped.len());
            deduped.push(field);
        }
    }

    Ok(deduped)
}

fn collect_member_fields(
    hint: &str,
    sor: &RawSchemaOrRef,
    ctx: &mut LoweringContext,
) -> Result<Vec<Field>> {
    match sor {
        RawSchemaOrRef::Ref { reference } => {
            let bare = parse_schema_ref_pointer(reference)?;
            if ctx.visiting.contains(bare) {
                bail!("cycle in `allOf` chain via `{bare}` — a schema cannot inherit from itself");
            }
            let target = ctx.components.schemas.get(bare).ok_or_else(|| {
                anyhow!("`allOf` $ref points to undefined schema `{bare}` (not present in components.schemas)")
            })?;
            ctx.visiting.insert(bare.to_string());
            let result = match target {
                RawSchemaOrRef::Inline(target_raw) => collect_object_fields(bare, target_raw, ctx)
                    .with_context(|| format!("flattening `{bare}` for allOf")),
                RawSchemaOrRef::Ref { .. } => collect_member_fields(bare, target, ctx)
                    .with_context(|| format!("following ref chain through `{bare}`")),
            };
            ctx.visiting.remove(bare);
            result
        }
        RawSchemaOrRef::Inline(raw) => collect_object_fields(hint, raw, ctx),
    }
}

fn primary_type(types: &[String]) -> Option<&str> {
    types.iter().find(|t| *t != "null").map(String::as_str)
}

fn is_nullable(types: &[String]) -> bool {
    types.iter().any(|t| t == "null")
}

/// Lower a schema that appears in a type position (field, parameter, body,
/// response, array item, map value). Anything that needs a Dart class is
/// synthesised into a named schema under `hint`.
fn lower_type_ref(hint: &str, sor: &RawSchemaOrRef, ctx: &mut LoweringContext) -> Result<TypeRef> {
    match sor {
        RawSchemaOrRef::Ref { reference } => {
            let bare = parse_schema_ref_pointer(reference)?;
            ctx.resolve_schema(bare)
        }
        RawSchemaOrRef::Inline(raw) => {
            if let Some(single) = single_non_null_variant(raw) {
                return lower_type_ref(hint, single, ctx);
            }
            if let Some(member) = single_allof_member(raw) {
                return lower_type_ref(hint, member, ctx);
            }
            if !raw.any_of.is_empty() || !raw.one_of.is_empty() || !raw.all_of.is_empty() {
                return ctx.synthesize(hint, raw);
            }
            if !raw.enum_values.is_empty() {
                let (values, warning) = lower_enum_values(&raw.enum_values);
                if let Some(w) = warning {
                    ctx.warn(format!("`{hint}`: {w}"));
                }
                if !values.is_empty() {
                    return Ok(TypeRef::Enum(values));
                }
            }
            match primary_type(&raw.ty) {
                Some("object") | None if !raw.properties.is_empty() => ctx.synthesize(hint, raw),
                Some("object") | None => match &raw.additional_properties {
                    Some(RawAdditionalProperties::Schema(inner)) => {
                        let value = lower_type_ref(&format!("{hint}Value"), inner, ctx)
                            .with_context(|| format!("additionalProperties of `{hint}`"))?;
                        Ok(TypeRef::Map(Box::new(value)))
                    }
                    Some(RawAdditionalProperties::Bool(true)) => {
                        Ok(TypeRef::Map(Box::new(TypeRef::Any)))
                    }
                    _ if primary_type(&raw.ty) == Some("object") => {
                        Ok(TypeRef::Map(Box::new(TypeRef::Any)))
                    }
                    _ => Ok(TypeRef::Any),
                },
                Some("array") => {
                    let inner = match &raw.items {
                        Some(items) => lower_type_ref(&format!("{hint}Item"), items, ctx)
                            .with_context(|| format!("in `{hint}.items`"))?,
                        None => {
                            ctx.warn(format!(
                                "`{hint}` is `type: array` but has no `items` — element type is `dynamic`"
                            ));
                            TypeRef::Any
                        }
                    };
                    Ok(TypeRef::Array(Box::new(inner)))
                }
                Some(_) => Ok(lower_primitive(hint, raw, ctx)),
            }
        }
    }
}

fn type_ref_is_recursive(t: &TypeRef, visiting: &HashSet<String>) -> bool {
    match t {
        TypeRef::Named(n) => visiting.contains(n),
        TypeRef::Array(inner) | TypeRef::Map(inner) => type_ref_is_recursive(inner, visiting),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_hint_cleans_arbitrary_input() {
        assert_eq!(pascal_hint("getPets"), "GetPets");
        assert_eq!(pascal_hint("user-profile"), "UserProfile");
        assert_eq!(pascal_hint("com.example.Pet"), "ComExamplePet");
        assert_eq!(pascal_hint("404"), "N404");
        assert_eq!(pascal_hint("///"), "Inline");
    }

    #[test]
    fn operation_hint_falls_back_to_method_and_path() {
        assert_eq!(
            operation_hint(HttpMethod::Get, "/pets/{petId}", None),
            "GetPetsPetId"
        );
        assert_eq!(
            operation_hint(HttpMethod::Get, "/pets", Some("listPets")),
            "ListPets"
        );
    }

    #[test]
    fn detects_swagger_and_openapi() {
        let swagger = "swagger: '2.0'\ninfo: {title: T}\npaths: {}\n";
        assert!(load_str(swagger).is_ok());
        let openapi = "openapi: 3.0.0\ninfo: {title: T}\npaths: {}\n";
        assert!(load_str(openapi).is_ok());
        let json = r#"{"openapi": "3.1.0", "info": {"title": "T"}, "paths": {}}"#;
        assert!(load_str(json).is_ok());
        let neither = "info: {title: T}\n";
        assert!(load_str(neither).is_err());
        let old = "openapi: 2.0\ninfo: {title: T}\n";
        assert!(load_str(old).is_err());
    }

    #[test]
    fn inline_objects_become_named_schemas() {
        let spec = r#"
openapi: 3.0.0
info: {title: T}
paths: {}
components:
  schemas:
    Pet:
      type: object
      properties:
        owner:
          type: object
          properties:
            name: {type: string}
        tags:
          type: array
          items:
            type: object
            properties:
              label: {type: string}
"#;
        let api = load_str(spec).unwrap();
        let names: Vec<&str> = api.schemas.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"PetOwner"), "{names:?}");
        assert!(names.contains(&"PetTagsItem"), "{names:?}");
    }

    #[test]
    fn nullable_wrapper_unions_collapse() {
        let spec = r#"
openapi: 3.1.0
info: {title: T}
paths: {}
components:
  schemas:
    A:
      type: object
      properties:
        b:
          oneOf:
            - $ref: '#/components/schemas/B'
            - type: 'null'
    B:
      type: object
      properties:
        x: {type: string}
"#;
        let api = load_str(spec).unwrap();
        let a = api.schemas.iter().find(|s| s.name == "A").unwrap();
        let SchemaKind::Object { fields } = &a.kind else {
            panic!()
        };
        assert_eq!(fields[0].type_ref, TypeRef::Named("B".into()));
        assert!(fields[0].nullable);
    }

    #[test]
    fn path_level_and_ref_parameters_are_merged() {
        let spec = r#"
openapi: 3.0.0
info: {title: T}
paths:
  /pets/{id}:
    parameters:
      - name: id
        in: path
        required: true
        schema: {type: string}
    get:
      operationId: getPet
      parameters:
        - $ref: '#/components/parameters/Verbose'
      responses:
        '200': {description: ok}
components:
  parameters:
    Verbose:
      name: verbose
      in: query
      schema: {type: boolean}
"#;
        let api = load_str(spec).unwrap();
        let op = &api.operations[0];
        let names: Vec<&str> = op.parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["id", "verbose"]);
        assert!(op.parameters[0].required);
    }

    #[test]
    fn swagger_body_and_form_params_become_request_bodies() {
        let spec = r#"
swagger: '2.0'
info: {title: T}
host: example.com
basePath: /v1
schemes: [http]
paths:
  /pets:
    post:
      operationId: addPet
      parameters:
        - name: body
          in: body
          required: true
          schema: {$ref: '#/definitions/Pet'}
      responses:
        '200': {description: ok, schema: {$ref: '#/definitions/Pet'}}
  /upload:
    post:
      operationId: upload
      consumes: [multipart/form-data]
      parameters:
        - name: file
          in: formData
          type: file
          required: true
        - name: note
          in: formData
          type: string
      responses:
        '204': {description: ok}
definitions:
  Pet:
    type: object
    required: [id]
    properties:
      id: {type: integer}
      tags:
        type: array
        items: {type: string}
securityDefinitions:
  basicAuth:
    type: basic
"#;
        let api = load_str(spec).unwrap();
        assert_eq!(api.base_urls, vec!["http://example.com/v1"]);
        let add = api
            .operations
            .iter()
            .find(|o| o.operation_id.as_deref() == Some("addPet"))
            .unwrap();
        let body = add.request_body.as_ref().unwrap();
        assert_eq!(body.schema_ref, TypeRef::Named("Pet".into()));
        assert!(body.required);
        let upload = api
            .operations
            .iter()
            .find(|o| o.operation_id.as_deref() == Some("upload"))
            .unwrap();
        let body = upload.request_body.as_ref().unwrap();
        assert!(body.is_multipart);
        let TypeRef::Named(form_name) = &body.schema_ref else {
            panic!()
        };
        let form = api.schemas.iter().find(|s| &s.name == form_name).unwrap();
        let SchemaKind::Object { fields } = &form.kind else {
            panic!()
        };
        let file = fields.iter().find(|f| f.name == "file").unwrap();
        assert_eq!(file.type_ref, TypeRef::Binary);
        assert!(matches!(
            api.security_schemes[0].kind,
            SecuritySchemeKind::HttpBasic
        ));
    }

    #[test]
    fn root_enums_and_primitives_are_supported() {
        let spec = r#"
openapi: 3.0.0
info: {title: T}
paths: {}
components:
  schemas:
    Status:
      type: string
      enum: [active, inactive]
    Id:
      type: string
      format: uuid
    Anything: {}
    Bag:
      type: object
"#;
        let api = load_str(spec).unwrap();
        let kind = |n: &str| &api.schemas.iter().find(|s| s.name == n).unwrap().kind;
        assert!(matches!(kind("Status"), SchemaKind::Enum { .. }));
        assert!(matches!(
            kind("Id"),
            SchemaKind::Primitive {
                type_ref: TypeRef::String
            }
        ));
        assert!(matches!(
            kind("Anything"),
            SchemaKind::Primitive {
                type_ref: TypeRef::Any
            }
        ));
        assert!(matches!(
            kind("Bag"),
            SchemaKind::Map {
                value: TypeRef::Any
            }
        ));
    }

    #[test]
    fn lenient_security_and_headers_produce_warnings_not_errors() {
        let spec = r#"
openapi: 3.0.0
info: {title: T}
paths:
  /x:
    get:
      operationId: x
      responses:
        '200':
          description: ok
          headers:
            X-Obj:
              schema: {$ref: '#/components/schemas/Obj'}
            X-Count:
              schema: {type: integer}
components:
  schemas:
    Obj:
      type: object
      properties: {a: {type: string}}
  securitySchemes:
    tls:
      type: mutualTLS
    digest:
      type: http
      scheme: digest
    ok:
      type: http
      scheme: bearer
"#;
        let api = load_str(spec).unwrap();
        assert_eq!(api.security_schemes.len(), 1);
        assert_eq!(api.operations[0].responses[0].headers.len(), 1);
        assert!(api.warnings.len() >= 3, "{:?}", api.warnings);
    }

    #[test]
    fn dangling_refs_are_fatal() {
        let spec = r#"
openapi: 3.0.0
info: {title: T}
paths: {}
components:
  schemas:
    A:
      type: object
      properties:
        b: {$ref: '#/components/schemas/Missing'}
"#;
        let err = load_str(spec).unwrap_err();
        assert!(format!("{err:#}").contains("Missing"));
    }
}
