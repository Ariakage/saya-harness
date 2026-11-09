use super::{
    BUILDER_VERSION, ComponentCount, ContextAccounting, MAX_HISTORY_ITEMS, MAX_SERIALIZED_BYTES,
    MAX_TOOLS, error,
};
use crate::{
    boundary::{ModelRequest, ToolDefinition},
    contract::{BoundaryError, HistoryItem, ModelProtocol},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{self, Write};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CountingIdentity {
    pub protocol: ModelProtocol,
    pub model: String,
    pub adapter_version: String,
}
impl CountingIdentity {
    pub(super) fn validate(&self) -> Result<(), BoundaryError> {
        if [&self.model, &self.adapter_version]
            .iter()
            .any(|text| text.is_empty() || text.len() > 256 || text.chars().any(char::is_control))
        {
            return Err(error("context_counter", "Counting identity is invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CountMethod {
    /// One token per serialized UTF-8 byte of the complete neutral request.
    /// This is intentionally conservative but is not a tokenizer or guaranteed
    /// upper bound on an adapter's wire format or any model's true token count.
    SerializedBytes,
    /// A local adapter counted its actual serialized wire body heuristically.
    SerializedHeuristic { name: String, version: String },
    /// The host verified this tokenizer for this model, adapter and wire format.
    VerifiedTokenizer { name: String, version: String },
}
impl CountMethod {
    fn estimated(&self) -> bool {
        !matches!(self, Self::VerifiedTokenizer { .. })
    }

    fn validate(&self) -> Result<(), BoundaryError> {
        let fields = match self {
            Self::SerializedBytes => return Ok(()),
            Self::SerializedHeuristic { name, version }
            | Self::VerifiedTokenizer { name, version } => [name, version],
        };
        if fields
            .iter()
            .any(|text| text.is_empty() || text.len() > 128 || text.chars().any(char::is_control))
        {
            return Err(error("context_counter", "Counter provenance is invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCount {
    pub tokens: u64,
    pub estimated: bool,
    pub method: CountMethod,
}

/// Evidence from a trusted local adapter, not deserialized model output.
#[derive(Debug, Clone)]
pub struct CountEvidence {
    pub request_fingerprint: String,
    pub identity: CountingIdentity,
    pub input_tokens: u64,
    pub method: CountMethod,
}

/// Count only the supplied request. Implementations must perform no network I/O.
/// The actual provider serializer remains the final protocol/budget guard.
pub trait RequestCounter: Send + Sync {
    fn count(
        &self,
        request: &ModelRequest,
        identity: &CountingIdentity,
        fingerprint: &str,
    ) -> Result<CountEvidence, BoundaryError>;
}

pub(super) struct Measurement {
    pub bytes: u64,
    pub sha256: String,
}

struct MeasureWriter {
    bytes: u64,
    limit: u64,
    hash: Sha256,
}
impl Write for MeasureWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(bytes.len()).map_err(|_| io::ErrorKind::InvalidData)?;
        let next = self
            .bytes
            .checked_add(length)
            .filter(|next| *next <= self.limit)
            .ok_or(io::ErrorKind::FileTooLarge)?;
        self.bytes = next;
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn measure<T: Serialize + ?Sized>(
    value: &T,
    limit: u64,
) -> Result<Measurement, BoundaryError> {
    let mut writer = MeasureWriter {
        bytes: 0,
        limit,
        hash: Sha256::new(),
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        error(
            "context_limit",
            "Context serialization exceeds its bounded capacity",
        )
    })?;
    let sha256 = writer
        .hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(Measurement {
        bytes: writer.bytes,
        sha256,
    })
}

#[derive(Serialize)]
pub(super) struct RequestView<'a> {
    request_id: &'a str,
    purpose: crate::contract::ModelPurpose,
    profile: &'a str,
    instructions: &'a str,
    dynamic_instructions: &'a Option<String>,
    max_output_tokens: Option<u32>,
    history: &'a [HistoryItem],
    tools: Vec<ToolView<'a>>,
}
#[derive(Serialize)]
struct ToolView<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a Value,
}
impl<'a> From<&'a ToolDefinition> for ToolView<'a> {
    fn from(value: &'a ToolDefinition) -> Self {
        Self {
            name: &value.name,
            description: &value.description,
            input_schema: &value.input_schema,
        }
    }
}
pub(super) fn request_view(request: &ModelRequest) -> RequestView<'_> {
    RequestView {
        request_id: &request.request_id,
        purpose: request.purpose,
        profile: &request.profile,
        instructions: &request.instructions,
        dynamic_instructions: &request.dynamic_instructions,
        max_output_tokens: request.max_output_tokens,
        history: &request.history,
        tools: request.tools.iter().map(ToolView::from).collect(),
    }
}

pub(super) fn validate_value(value: &Value) -> Result<(), BoundaryError> {
    let mut stack = vec![(value, 0usize)];
    let mut nodes = 0usize;
    while let Some((value, depth)) = stack.pop() {
        nodes += 1;
        if depth > 128 || nodes > 1_000_000 {
            return Err(error(
                "context_limit",
                "Structured context exceeds its nesting or item limit",
            ));
        }
        let children: Box<dyn Iterator<Item = &Value> + '_> = match value {
            Value::Array(values) => Box::new(values.iter()),
            Value::Object(values) => Box::new(values.values()),
            _ => continue,
        };
        for child in children {
            if nodes.saturating_add(stack.len()) >= 1_000_000 {
                return Err(error(
                    "context_limit",
                    "Structured context exceeds its item limit",
                ));
            }
            stack.push((child, depth + 1));
        }
    }
    Ok(())
}

pub(super) fn validate_history(history: &[HistoryItem]) -> Result<(), BoundaryError> {
    if history.len() > MAX_HISTORY_ITEMS {
        return Err(error("context_limit", "History contains too many items"));
    }
    for item in history {
        match item {
            HistoryItem::ProviderItem { value, .. } => validate_value(value)?,
            HistoryItem::ToolResult { result, .. } => {
                if let Some(value) = &result.data {
                    validate_value(value)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn validate_request(request: &ModelRequest) -> Result<(), BoundaryError> {
    if request.tools.len() > MAX_TOOLS {
        return Err(error(
            "context_limit",
            "Request contains too many tool definitions",
        ));
    }
    validate_history(&request.history)?;
    for tool in &request.tools {
        validate_value(&tool.input_schema)?;
    }
    Ok(())
}

pub(super) fn request_fingerprint(
    request: &ModelRequest,
    identity: &CountingIdentity,
) -> Result<String, BoundaryError> {
    identity.validate()?;
    validate_request(request)?;
    measure(
        &(BUILDER_VERSION, identity, request_view(request)),
        MAX_SERIALIZED_BYTES,
    )
    .map(|measurement| measurement.sha256)
}

fn component(bytes: u64) -> ComponentCount {
    ComponentCount {
        serialized_bytes: bytes,
        tokens: bytes,
        estimated: true,
    }
}

pub(super) fn account(
    request: &ModelRequest,
    identity: &CountingIdentity,
    history_offset: usize,
    counter: Option<&dyn RequestCounter>,
) -> Result<ContextAccounting, BoundaryError> {
    let view = request_view(request);
    let bytes = measure(&view, MAX_SERIALIZED_BYTES)?.bytes;
    let static_bytes = measure(&request.instructions, MAX_SERIALIZED_BYTES)?.bytes;
    let dynamic = measure(&request.dynamic_instructions, MAX_SERIALIZED_BYTES)?.bytes;
    let tools = measure(&view.tools, MAX_SERIALIZED_BYTES)?.bytes;
    let mut host = 0u64;
    let mut history = 0u64;
    let mut provider = 0u64;
    for (index, item) in request.history.iter().enumerate() {
        let size = measure(item, MAX_SERIALIZED_BYTES)?.bytes;
        if matches!(item, HistoryItem::ProviderItem { .. }) {
            provider += size;
        } else if index < history_offset {
            host += size;
        } else {
            history += size;
        }
    }
    // These serialized fields and item payloads occupy disjoint byte ranges.
    let payload = static_bytes + dynamic + tools + host + history + provider;
    let envelopes = bytes.checked_sub(payload).ok_or_else(|| {
        error(
            "context_counter",
            "Context component accounting is inconsistent",
        )
    })?;
    let total = if let Some(counter) = counter {
        let fingerprint = request_fingerprint(request, identity)?;
        let evidence = counter.count(request, identity, &fingerprint)?;
        evidence.method.validate()?;
        if evidence.request_fingerprint != fingerprint
            || evidence.identity != *identity
            || evidence.input_tokens == 0
            || (evidence.method == CountMethod::SerializedBytes && evidence.input_tokens != bytes)
        {
            return Err(error(
                "context_counter",
                "Token count evidence belongs to a different request",
            ));
        }
        TokenCount {
            tokens: evidence.input_tokens,
            estimated: evidence.method.estimated(),
            method: evidence.method,
        }
    } else {
        TokenCount {
            tokens: bytes,
            estimated: true,
            method: CountMethod::SerializedBytes,
        }
    };
    Ok(ContextAccounting {
        total,
        static_instructions: component(static_bytes),
        dynamic_instructions: component(dynamic),
        tools: component(tools),
        host_context: component(host),
        history: component(history),
        provider_items: component(provider),
        envelopes: component(envelopes),
        serialized_bytes: bytes,
    })
}
