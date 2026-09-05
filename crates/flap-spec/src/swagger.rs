//! Swagger 2.0 raw document types and their conversion into the OpenAPI 3
//! raw shape used by the main lowering pass in `lib.rs`.
//!
//! Rather than maintaining a second lowering implementation, a Swagger
//! document is translated field-by-field into [`RawSpec`](super::RawSpec):
//!
//! * `definitions`          → `components.schemas`
//! * `parameters` (global)  → `components.parameters`
//! * `responses` (global)   → `components.responses`
//! * `securityDefinitions`  → `components.securitySchemes`
//! * `host` + `basePath`    → `servers[0].url`
//! * body parameters        → `requestBody` (`application/json`)
//! * formData parameters    → `requestBody` (`multipart/form-data` or
//!   `application/x-www-form-urlencoded`, depending on `consumes`)
//! * `#/definitions/X`      → `#/components/schemas/X`
//!
//! Everything downstream (validation, lowering, emission) is shared.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{
    RawAdditionalProperties, RawComponents, RawInfo, RawMediaType, RawOAuth2Flow, RawOAuth2Flows,
    RawOperation, RawParameter, RawParameterOrRef, RawPathItem, RawRequestBody,
    RawRequestBodyOrRef, RawResponse, RawResponseHeader, RawResponseHeaderOrRef, RawResponseOrRef,
    RawSchema, RawSchemaOrRef, RawSecurityScheme, RawServer, RawSpec,
};

// ── Raw types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SwaggerSpec {
    #[serde(default)]
    pub info: SwaggerInfo,
    pub host: Option<String>,
    #[serde(rename = "basePath")]
    pub base_path: Option<String>,
    #[serde(default)]
    pub schemes: Vec<String>,
    #[serde(default)]
    pub consumes: Vec<String>,
    #[serde(default)]
    pub paths: BTreeMap<String, SwaggerPathItem>,
    #[serde(default)]
    pub definitions: BTreeMap<String, SwaggerSchemaOrRef>,
    #[serde(default)]
    pub parameters: BTreeMap<String, SwaggerParameter>,
    #[serde(default)]
    pub responses: BTreeMap<String, SwaggerResponse>,
    #[serde(default, rename = "securityDefinitions")]
    pub security_definitions: BTreeMap<String, SwaggerSecurityDefinition>,
    #[serde(default)]
    pub security: Vec<BTreeMap<String, Vec<String>>>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SwaggerInfo {
    pub title: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SwaggerPathItem {
    pub get: Option<SwaggerOperation>,
    pub put: Option<SwaggerOperation>,
    pub post: Option<SwaggerOperation>,
    pub delete: Option<SwaggerOperation>,
    pub options: Option<SwaggerOperation>,
    pub head: Option<SwaggerOperation>,
    pub patch: Option<SwaggerOperation>,
    /// Path-level parameters, inherited by every operation on the path.
    #[serde(default)]
    pub parameters: Vec<SwaggerParameterOrRef>,
}

#[derive(Debug, Deserialize)]
pub struct SwaggerOperation {
    #[serde(rename = "operationId")]
    pub operation_id: Option<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Vec<SwaggerParameterOrRef>,
    #[serde(default)]
    pub responses: BTreeMap<String, SwaggerResponseOrRef>,
    #[serde(default)]
    pub consumes: Vec<String>,
    #[serde(default)]
    pub security: Option<Vec<BTreeMap<String, Vec<String>>>>,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SwaggerParameterOrRef {
    Ref {
        #[serde(rename = "$ref")]
        reference: String,
    },
    Inline(Box<SwaggerParameter>),
}

/// Swagger 2.0 parameter. Non-body parameters carry their type inline
/// (`type`, `format`, `items`, `enum`); body parameters use `schema`.
#[derive(Debug, Clone, Deserialize)]
pub struct SwaggerParameter {
    pub name: String,
    #[serde(rename = "in")]
    pub location: String,
    #[serde(default)]
    pub required: bool,
    #[serde(rename = "type")]
    pub ty: Option<String>,
    pub format: Option<String>,
    pub items: Option<Box<SwaggerItems>>,
    #[serde(default, rename = "enum")]
    pub enum_values: Vec<serde_yaml::Value>,
    pub schema: Option<SwaggerSchemaOrRef>,
    pub default: Option<serde_yaml::Value>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_yaml::Value>,
}

/// `items` for array-typed parameters and headers.
#[derive(Debug, Clone, Deserialize)]
pub struct SwaggerItems {
    #[serde(rename = "type")]
    pub ty: Option<String>,
    pub format: Option<String>,
    pub items: Option<Box<SwaggerItems>>,
    #[serde(default, rename = "enum")]
    pub enum_values: Vec<serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SwaggerResponseOrRef {
    Ref {
        #[serde(rename = "$ref")]
        reference: String,
    },
    Inline(SwaggerResponse),
}

#[derive(Debug, Deserialize)]
pub struct SwaggerResponse {
    pub description: Option<String>,
    pub schema: Option<SwaggerSchemaOrRef>,
    #[serde(default)]
    pub headers: BTreeMap<String, SwaggerHeader>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
pub struct SwaggerHeader {
    #[serde(rename = "type")]
    pub ty: Option<String>,
    pub format: Option<String>,
    pub items: Option<Box<SwaggerItems>>,
    #[serde(default, rename = "enum")]
    pub enum_values: Vec<serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SwaggerSchemaOrRef {
    Ref {
        #[serde(rename = "$ref")]
        reference: String,
    },
    Inline(Box<SwaggerSchema>),
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SwaggerSchema {
    #[serde(
        default,
        rename = "type",
        deserialize_with = "super::deserialize_openapi_type"
    )]
    pub ty: Vec<String>,
    pub format: Option<String>,
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub properties: BTreeMap<String, SwaggerSchemaOrRef>,
    pub items: Option<Box<SwaggerSchemaOrRef>>,
    #[serde(default, rename = "enum")]
    pub enum_values: Vec<serde_yaml::Value>,
    #[serde(rename = "additionalProperties")]
    pub additional_properties: Option<SwaggerAdditionalProperties>,
    #[serde(default, rename = "allOf")]
    pub all_of: Vec<SwaggerSchemaOrRef>,
    /// Swagger 2.0 `discriminator` is just the property name.
    pub discriminator: Option<String>,
    pub default: Option<serde_yaml::Value>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SwaggerAdditionalProperties {
    Bool(bool),
    Schema(Box<SwaggerSchemaOrRef>),
}

#[derive(Debug, Deserialize)]
pub struct SwaggerSecurityDefinition {
    #[serde(rename = "type")]
    pub ty: String,
    pub name: Option<String>,
    #[serde(rename = "in")]
    pub location: Option<String>,
    pub flow: Option<String>,
    #[serde(rename = "authorizationUrl")]
    pub authorization_url: Option<String>,
    #[serde(rename = "tokenUrl")]
    pub token_url: Option<String>,
    #[serde(default)]
    pub scopes: BTreeMap<String, String>,
}

// ── Conversion to the OpenAPI 3 raw shape ────────────────────────────────────

const DEFINITIONS_PREFIX: &str = "#/definitions/";
const PARAMETERS_PREFIX: &str = "#/parameters/";
const RESPONSES_PREFIX: &str = "#/responses/";

fn rewrite_ref(reference: &str) -> String {
    if let Some(rest) = reference.strip_prefix(DEFINITIONS_PREFIX) {
        format!("#/components/schemas/{rest}")
    } else if let Some(rest) = reference.strip_prefix(PARAMETERS_PREFIX) {
        format!("#/components/parameters/{rest}")
    } else if let Some(rest) = reference.strip_prefix(RESPONSES_PREFIX) {
        format!("#/components/responses/{rest}")
    } else {
        reference.to_string()
    }
}

fn is_multipart(consumes: &[String]) -> bool {
    consumes
        .iter()
        .any(|c| c.to_ascii_lowercase().starts_with("multipart/"))
}

impl SwaggerSpec {
    /// Translate this Swagger 2.0 document into the OpenAPI 3 raw shape.
    pub(crate) fn into_openapi(self) -> RawSpec {
        let SwaggerSpec {
            info,
            host,
            base_path,
            schemes,
            consumes,
            paths,
            definitions,
            parameters,
            responses,
            security_definitions,
            security,
            extensions,
        } = self;

        let servers = build_server(host.as_deref(), base_path.as_deref(), &schemes)
            .into_iter()
            .map(|url| RawServer {
                url,
                variables: BTreeMap::new(),
            })
            .collect();

        let mut components = RawComponents::default();
        for (name, sor) in definitions {
            components.schemas.insert(name, convert_schema_or_ref(sor));
        }
        // Global parameters: body / formData parameters cannot be expressed
        // as `components.parameters`; they are resolved inline at the
        // operation level instead (see `convert_operation`).
        for (name, param) in &parameters {
            if param.location != "body" && param.location != "formData" {
                components.parameters.insert(
                    name.clone(),
                    RawParameterOrRef::Inline(convert_parameter(param)),
                );
            }
        }
        for (name, resp) in responses {
            components
                .responses
                .insert(name, RawResponseOrRef::Inline(convert_response(resp)));
        }
        for (name, def) in security_definitions {
            components
                .security_schemes
                .insert(name, convert_security(def));
        }

        let raw_paths = paths
            .into_iter()
            .map(|(path, item)| (path, convert_path_item(item, &consumes, &parameters)))
            .collect();

        RawSpec {
            info: RawInfo { title: info.title },
            servers,
            paths: raw_paths,
            components,
            security,
            extensions,
        }
    }
}

fn build_server(host: Option<&str>, base_path: Option<&str>, schemes: &[String]) -> Option<String> {
    let scheme = schemes
        .iter()
        .find(|s| s.eq_ignore_ascii_case("https"))
        .or_else(|| schemes.first())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "https".to_string());
    let base_path = base_path.map(|bp| bp.trim_end_matches('/').to_string());
    match (host, base_path) {
        (Some(h), Some(bp)) => Some(format!("{scheme}://{h}{bp}")),
        (Some(h), None) => Some(format!("{scheme}://{h}")),
        (None, Some(bp)) if !bp.is_empty() => Some(bp),
        _ => None,
    }
}

fn convert_path_item(
    item: SwaggerPathItem,
    spec_consumes: &[String],
    global_params: &BTreeMap<String, SwaggerParameter>,
) -> RawPathItem {
    // Path-level body/formData parameters are pushed down into every
    // operation so they can become request bodies.
    let mut path_level_params: Vec<RawParameterOrRef> = Vec::new();
    let mut path_level_body: Vec<Box<SwaggerParameter>> = Vec::new();
    for p in item.parameters {
        match resolve_param(p, global_params) {
            Resolved::Ref(reference) => {
                path_level_params.push(RawParameterOrRef::Ref { reference })
            }
            Resolved::Inline(param) => {
                if param.location == "body" || param.location == "formData" {
                    path_level_body.push(param);
                } else {
                    path_level_params.push(RawParameterOrRef::Inline(convert_parameter(&param)));
                }
            }
        }
    }

    let convert = |op: Option<SwaggerOperation>| {
        op.map(|op| convert_operation(op, spec_consumes, global_params, &path_level_body))
    };

    RawPathItem {
        get: convert(item.get),
        put: convert(item.put),
        post: convert(item.post),
        delete: convert(item.delete),
        options: convert(item.options),
        head: convert(item.head),
        patch: convert(item.patch),
        trace: None,
        parameters: path_level_params,
    }
}

enum Resolved {
    Ref(String),
    Inline(Box<SwaggerParameter>),
}

/// Resolve `#/parameters/X` references whose target is a body or formData
/// parameter (those cannot live in `components.parameters`). Other refs are
/// rewritten to `#/components/parameters/X` and resolved later.
fn resolve_param(
    p: SwaggerParameterOrRef,
    global_params: &BTreeMap<String, SwaggerParameter>,
) -> Resolved {
    match p {
        SwaggerParameterOrRef::Inline(param) => Resolved::Inline(param),
        SwaggerParameterOrRef::Ref { reference } => {
            if let Some(name) = reference.strip_prefix(PARAMETERS_PREFIX)
                && let Some(target) = global_params.get(name)
                && (target.location == "body" || target.location == "formData")
            {
                return Resolved::Inline(Box::new(target.clone()));
            }
            Resolved::Ref(rewrite_ref(&reference))
        }
    }
}

fn convert_operation(
    op: SwaggerOperation,
    spec_consumes: &[String],
    global_params: &BTreeMap<String, SwaggerParameter>,
    path_level_body: &[Box<SwaggerParameter>],
) -> RawOperation {
    let mut parameters: Vec<RawParameterOrRef> = Vec::new();
    let mut body_param: Option<Box<SwaggerParameter>> = None;
    let mut form_params: Vec<Box<SwaggerParameter>> = Vec::new();

    for p in path_level_body {
        if p.location == "body" {
            body_param = Some(p.clone());
        } else {
            form_params.push(p.clone());
        }
    }

    for p in op.parameters {
        match resolve_param(p, global_params) {
            Resolved::Ref(reference) => parameters.push(RawParameterOrRef::Ref { reference }),
            Resolved::Inline(param) => match param.location.as_str() {
                "body" => body_param = Some(param),
                "formData" => {
                    form_params.retain(|f| f.name != param.name);
                    form_params.push(param);
                }
                _ => parameters.push(RawParameterOrRef::Inline(convert_parameter(&param))),
            },
        }
    }

    let consumes: &[String] = if op.consumes.is_empty() {
        spec_consumes
    } else {
        &op.consumes
    };

    let request_body = if let Some(body) = body_param {
        let schema = body
            .schema
            .map(convert_schema_or_ref)
            .unwrap_or_else(|| RawSchemaOrRef::Inline(Box::default()));
        let content_type = consumes
            .iter()
            .find(|c| !c.to_ascii_lowercase().starts_with("multipart/"))
            .cloned()
            .unwrap_or_else(|| "application/json".to_string());
        let mut content = BTreeMap::new();
        content.insert(
            content_type,
            RawMediaType {
                schema: Some(schema),
            },
        );
        Some(RawRequestBodyOrRef::Inline(RawRequestBody {
            content,
            required: body.required,
            extensions: body.extensions,
        }))
    } else if !form_params.is_empty() {
        let mut properties = BTreeMap::new();
        let mut required = Vec::new();
        let mut any_required = false;
        for p in &form_params {
            if p.required {
                required.push(p.name.clone());
                any_required = true;
            }
            properties.insert(
                p.name.clone(),
                RawSchemaOrRef::Inline(Box::new(param_schema(p))),
            );
        }
        let schema = RawSchema {
            ty: vec!["object".to_string()],
            required,
            properties,
            ..RawSchema::default()
        };
        let content_type = if is_multipart(consumes) {
            "multipart/form-data"
        } else {
            "application/x-www-form-urlencoded"
        };
        let mut content = BTreeMap::new();
        content.insert(
            content_type.to_string(),
            RawMediaType {
                schema: Some(RawSchemaOrRef::Inline(Box::new(schema))),
            },
        );
        Some(RawRequestBodyOrRef::Inline(RawRequestBody {
            content,
            required: any_required,
            extensions: BTreeMap::new(),
        }))
    } else {
        None
    };

    let responses = op
        .responses
        .into_iter()
        .map(|(code, r)| {
            let converted = match r {
                SwaggerResponseOrRef::Ref { reference } => RawResponseOrRef::Ref {
                    reference: rewrite_ref(&reference),
                },
                SwaggerResponseOrRef::Inline(resp) => {
                    RawResponseOrRef::Inline(convert_response(resp))
                }
            };
            (code, converted)
        })
        .collect();

    RawOperation {
        operation_id: op.operation_id,
        summary: op.summary,
        description: op.description,
        parameters,
        request_body,
        responses,
        security: op.security,
        deprecated: op.deprecated,
        extensions: op.extensions,
    }
}

fn convert_parameter(p: &SwaggerParameter) -> RawParameter {
    RawParameter {
        name: p.name.clone(),
        location: p.location.clone(),
        required: p.required,
        schema: Some(RawSchemaOrRef::Inline(Box::new(param_schema(p)))),
        content: None,
        extensions: p.extensions.clone(),
    }
}

/// Build an inline schema from a Swagger parameter's top-level type fields.
fn param_schema(p: &SwaggerParameter) -> RawSchema {
    if let Some(schema) = &p.schema
        && let SwaggerSchemaOrRef::Inline(inline) = schema
    {
        return convert_schema((**inline).clone());
    }
    let mut schema = inline_type_schema(p.ty.as_deref(), p.format.as_deref(), p.items.as_deref());
    schema.enum_values = p.enum_values.clone();
    schema.default = p.default.clone();
    schema.nullable = x_nullable(&p.extensions);
    schema
}

/// `type`/`format`/`items` triple → inline schema. `file` becomes
/// `string`/`binary` so uploads surface as `List<int>`.
fn inline_type_schema(
    ty: Option<&str>,
    format: Option<&str>,
    items: Option<&SwaggerItems>,
) -> RawSchema {
    let mut schema = RawSchema::default();
    match ty {
        Some("file") => {
            schema.ty = vec!["string".to_string()];
            schema.format = Some("binary".to_string());
        }
        Some("array") => {
            schema.ty = vec!["array".to_string()];
            let inner = items
                .map(|i| {
                    let mut s = inline_type_schema(
                        i.ty.as_deref(),
                        i.format.as_deref(),
                        i.items.as_deref(),
                    );
                    s.enum_values = i.enum_values.clone();
                    s
                })
                .unwrap_or_default();
            schema.items = Some(Box::new(RawSchemaOrRef::Inline(Box::new(inner))));
        }
        Some(t) => {
            schema.ty = vec![t.to_string()];
            schema.format = format.map(str::to_string);
        }
        None => {}
    }
    schema
}

fn x_nullable(ext: &BTreeMap<String, serde_yaml::Value>) -> Option<bool> {
    ext.get("x-nullable").and_then(|v| v.as_bool())
}

fn convert_response(resp: SwaggerResponse) -> RawResponse {
    let mut content = BTreeMap::new();
    if let Some(schema) = resp.schema {
        content.insert(
            "application/json".to_string(),
            RawMediaType {
                schema: Some(convert_schema_or_ref(schema)),
            },
        );
    }
    let headers = resp
        .headers
        .into_iter()
        .map(|(name, h)| {
            let mut schema =
                inline_type_schema(h.ty.as_deref(), h.format.as_deref(), h.items.as_deref());
            schema.enum_values = h.enum_values;
            (
                name,
                RawResponseHeaderOrRef::Inline(RawResponseHeader {
                    schema: Some(RawSchemaOrRef::Inline(Box::new(schema))),
                    required: false,
                }),
            )
        })
        .collect();
    RawResponse {
        description: resp.description,
        content,
        headers,
        extensions: resp.extensions,
    }
}

fn convert_schema_or_ref(sor: SwaggerSchemaOrRef) -> RawSchemaOrRef {
    match sor {
        SwaggerSchemaOrRef::Ref { reference } => RawSchemaOrRef::Ref {
            reference: rewrite_ref(&reference),
        },
        SwaggerSchemaOrRef::Inline(raw) => RawSchemaOrRef::Inline(Box::new(convert_schema(*raw))),
    }
}

fn convert_schema(raw: SwaggerSchema) -> RawSchema {
    let nullable = x_nullable(&raw.extensions);
    RawSchema {
        ty: raw.ty,
        format: raw.format,
        required: raw.required,
        properties: raw
            .properties
            .into_iter()
            .map(|(k, v)| (k, convert_schema_or_ref(v)))
            .collect(),
        items: raw.items.map(|i| Box::new(convert_schema_or_ref(*i))),
        enum_values: raw.enum_values,
        additional_properties: raw.additional_properties.map(|ap| match ap {
            SwaggerAdditionalProperties::Bool(b) => RawAdditionalProperties::Bool(b),
            SwaggerAdditionalProperties::Schema(inner) => {
                RawAdditionalProperties::Schema(Box::new(convert_schema_or_ref(*inner)))
            }
        }),
        all_of: raw.all_of.into_iter().map(convert_schema_or_ref).collect(),
        any_of: Vec::new(),
        one_of: Vec::new(),
        discriminator: raw
            .discriminator
            .map(|property_name| super::RawDiscriminator {
                property_name,
                mapping: BTreeMap::new(),
            }),
        nullable,
        default: raw.default,
        extensions: raw.extensions,
    }
}

fn convert_security(def: SwaggerSecurityDefinition) -> RawSecurityScheme {
    match def.ty.as_str() {
        "basic" => RawSecurityScheme {
            ty: "http".to_string(),
            name: None,
            location: None,
            scheme: Some("basic".to_string()),
            bearer_format: None,
            flows: None,
            open_id_connect_url: None,
        },
        "oauth2" => {
            let flow = RawOAuth2Flow {
                token_url: def.token_url,
                authorization_url: def.authorization_url,
                scopes: def.scopes,
            };
            let mut flows = RawOAuth2Flows::default();
            match def.flow.as_deref().unwrap_or("implicit") {
                "password" => flows.password = Some(flow),
                "application" => flows.client_credentials = Some(flow),
                "accessCode" => flows.authorization_code = Some(flow),
                _ => flows.implicit = Some(flow),
            }
            RawSecurityScheme {
                ty: "oauth2".to_string(),
                name: None,
                location: None,
                scheme: None,
                bearer_format: None,
                flows: Some(flows),
                open_id_connect_url: None,
            }
        }
        // `apiKey` maps 1:1; anything else is passed through and rejected
        // (with a warning) by the shared security lowering.
        other => RawSecurityScheme {
            ty: other.to_string(),
            name: def.name,
            location: def.location,
            scheme: None,
            bearer_format: None,
            flows: None,
            open_id_connect_url: None,
        },
    }
}
