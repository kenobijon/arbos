//! Shared decoding of relay error bodies for agentd → relay HTTP helpers.

use arbos_core::api::ApiErrorDto;

/// Prefer `ApiErrorDto.error.message` when the body is that shape; else the raw text.
pub(crate) fn message(body: &str) -> String {
    serde_json::from_str::<ApiErrorDto>(body)
        .map(|dto| dto.error.message)
        .unwrap_or_else(|_| body.to_owned())
}
