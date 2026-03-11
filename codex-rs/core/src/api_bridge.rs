use base64::Engine;
use chrono::DateTime;
use chrono::Utc;
use codex_api::AuthProvider as ApiAuthProvider;
use codex_api::TransportError;
use codex_api::error::ApiError;
use codex_api::rate_limits::parse_promo_message;
use codex_api::rate_limits::parse_rate_limit_for_limit;
use http::HeaderMap;
use serde::Deserialize;
use serde_json::Value;

use crate::auth::CodexAuth;
use crate::error::CodexErr;
use crate::error::RetryLimitReachedError;
use crate::error::UnexpectedResponseError;
use crate::error::UsageLimitReachedError;
use crate::model_provider_info::ModelProviderInfo;
use crate::token_data::PlanType;

pub(crate) fn map_api_error(err: ApiError) -> CodexErr {
    match err {
        ApiError::ContextWindowExceeded => CodexErr::ContextWindowExceeded,
        ApiError::QuotaExceeded => CodexErr::QuotaExceeded,
        ApiError::UsageNotIncluded => CodexErr::UsageNotIncluded,
        ApiError::Retryable { message, delay } => CodexErr::Stream(message, delay),
        ApiError::Stream(msg) => CodexErr::Stream(msg, None),
        ApiError::ServerOverloaded => CodexErr::ServerOverloaded,
        ApiError::Api { status, message } => CodexErr::UnexpectedStatus(UnexpectedResponseError {
            status,
            body: message,
            url: None,
            cf_ray: None,
            request_id: None,
            identity_authorization_error: None,
            identity_error_code: None,
        }),
        ApiError::InvalidRequest { message } => CodexErr::InvalidRequest(message),
        ApiError::Transport(transport) => match transport {
            TransportError::Http {
                status,
                url,
                headers,
                body,
            } => {
                let body_text = body.unwrap_or_default();

                if status == http::StatusCode::SERVICE_UNAVAILABLE
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&body_text)
                    && matches!(
                        value
                            .get("error")
                            .and_then(|error| error.get("code"))
                            .and_then(serde_json::Value::as_str),
                        Some("server_is_overloaded" | "slow_down")
                    )
                {
                    return CodexErr::ServerOverloaded;
                }

                if status == http::StatusCode::BAD_REQUEST {
                    if body_text
                        .contains("The image data you provided does not represent a valid image")
                    {
                        CodexErr::InvalidImageRequest()
                    } else {
                        CodexErr::InvalidRequest(body_text)
                    }
                } else if status == http::StatusCode::INTERNAL_SERVER_ERROR {
                    CodexErr::InternalServerError
                } else if status == http::StatusCode::TOO_MANY_REQUESTS {
                    if let Ok(err) = serde_json::from_str::<UsageErrorResponse>(&body_text) {
                        if err.error.error_type.as_deref() == Some("usage_limit_reached") {
                            let limit_id = extract_header(headers.as_ref(), ACTIVE_LIMIT_HEADER);
                            let rate_limits = headers.as_ref().and_then(|map| {
                                parse_rate_limit_for_limit(map, limit_id.as_deref())
                            });
                            let promo_message = headers.as_ref().and_then(parse_promo_message);
                            let resets_at = err
                                .error
                                .resets_at
                                .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0));
                            return CodexErr::UsageLimitReached(UsageLimitReachedError {
                                plan_type: err.error.plan_type,
                                resets_at,
                                rate_limits: rate_limits.map(Box::new),
                                promo_message,
                            });
                        } else if err.error.error_type.as_deref() == Some("usage_not_included") {
                            return CodexErr::UsageNotIncluded;
                        }
                    }

                    CodexErr::RetryLimit(RetryLimitReachedError {
                        status,
                        request_id: extract_request_tracking_id(headers.as_ref()),
                    })
                } else {
                    CodexErr::UnexpectedStatus(UnexpectedResponseError {
                        status,
                        body: body_text,
                        url,
                        cf_ray: extract_header(headers.as_ref(), CF_RAY_HEADER),
                        request_id: extract_request_id(headers.as_ref()),
                        identity_authorization_error: extract_header(
                            headers.as_ref(),
                            X_OPENAI_AUTHORIZATION_ERROR_HEADER,
                        ),
                        identity_error_code: extract_x_error_json_code(headers.as_ref()),
                    })
                }
            }
            TransportError::RetryLimit => CodexErr::RetryLimit(RetryLimitReachedError {
                status: http::StatusCode::INTERNAL_SERVER_ERROR,
                request_id: None,
            }),
            TransportError::Timeout => CodexErr::Timeout,
            TransportError::Network(msg) | TransportError::Build(msg) => {
                CodexErr::Stream(msg, None)
            }
        },
        ApiError::RateLimit(msg) => CodexErr::Stream(msg, None),
    }
}

const ACTIVE_LIMIT_HEADER: &str = "x-codex-active-limit";
const REQUEST_ID_HEADER: &str = "x-request-id";
const OAI_REQUEST_ID_HEADER: &str = "x-oai-request-id";
const CF_RAY_HEADER: &str = "cf-ray";
const X_OPENAI_AUTHORIZATION_ERROR_HEADER: &str = "x-openai-authorization-error";
const X_ERROR_JSON_HEADER: &str = "x-error-json";

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use pretty_assertions::assert_eq;

    #[test]
    fn map_api_error_maps_server_overloaded() {
        let err = map_api_error(ApiError::ServerOverloaded);
        assert!(matches!(err, CodexErr::ServerOverloaded));
    }

    #[test]
    fn map_api_error_maps_server_overloaded_from_503_body() {
        let body = serde_json::json!({
            "error": {
                "code": "server_is_overloaded"
            }
        })
        .to_string();
        let err = map_api_error(ApiError::Transport(TransportError::Http {
            status: http::StatusCode::SERVICE_UNAVAILABLE,
            url: Some("http://example.com/v1/responses".to_string()),
            headers: None,
            body: Some(body),
        }));

        assert!(matches!(err, CodexErr::ServerOverloaded));
    }

    #[test]
    fn map_api_error_maps_usage_limit_limit_name_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACTIVE_LIMIT_HEADER,
            http::HeaderValue::from_static("codex_other"),
        );
        headers.insert(
            "x-codex-other-limit-name",
            http::HeaderValue::from_static("codex_other"),
        );
        let body = serde_json::json!({
            "error": {
                "type": "usage_limit_reached",
                "plan_type": "pro",
            }
        })
        .to_string();
        let err = map_api_error(ApiError::Transport(TransportError::Http {
            status: http::StatusCode::TOO_MANY_REQUESTS,
            url: Some("http://example.com/v1/responses".to_string()),
            headers: Some(headers),
            body: Some(body),
        }));

        let CodexErr::UsageLimitReached(usage_limit) = err else {
            panic!("expected CodexErr::UsageLimitReached, got {err:?}");
        };
        assert_eq!(
            usage_limit
                .rate_limits
                .as_ref()
                .and_then(|snapshot| snapshot.limit_name.as_deref()),
            Some("codex_other")
        );
    }

    #[test]
    fn map_api_error_does_not_fallback_limit_name_to_limit_id() {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACTIVE_LIMIT_HEADER,
            http::HeaderValue::from_static("codex_other"),
        );
        let body = serde_json::json!({
            "error": {
                "type": "usage_limit_reached",
                "plan_type": "pro",
            }
        })
        .to_string();
        let err = map_api_error(ApiError::Transport(TransportError::Http {
            status: http::StatusCode::TOO_MANY_REQUESTS,
            url: Some("http://example.com/v1/responses".to_string()),
            headers: Some(headers),
            body: Some(body),
        }));

        let CodexErr::UsageLimitReached(usage_limit) = err else {
            panic!("expected CodexErr::UsageLimitReached, got {err:?}");
        };
        assert_eq!(
            usage_limit
                .rate_limits
                .as_ref()
                .and_then(|snapshot| snapshot.limit_name.as_deref()),
            None
        );
    }

    #[test]
    fn map_api_error_extracts_identity_auth_details_from_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(REQUEST_ID_HEADER, http::HeaderValue::from_static("req-401"));
        headers.insert(CF_RAY_HEADER, http::HeaderValue::from_static("ray-401"));
        headers.insert(
            X_OPENAI_AUTHORIZATION_ERROR_HEADER,
            http::HeaderValue::from_static("missing_authorization_header"),
        );
        let x_error_json = base64::engine::general_purpose::STANDARD
            .encode(r#"{"error":{"code":"token_expired"}}"#);
        headers.insert(
            X_ERROR_JSON_HEADER,
            http::HeaderValue::from_str(&x_error_json).expect("valid x-error-json header"),
        );

        let err = map_api_error(ApiError::Transport(TransportError::Http {
            status: http::StatusCode::UNAUTHORIZED,
            url: Some("https://chatgpt.com/backend-api/codex/models".to_string()),
            headers: Some(headers),
            body: Some(r#"{"detail":"Unauthorized"}"#.to_string()),
        }));

        let CodexErr::UnexpectedStatus(err) = err else {
            panic!("expected CodexErr::UnexpectedStatus, got {err:?}");
        };
        assert_eq!(err.request_id.as_deref(), Some("req-401"));
        assert_eq!(err.cf_ray.as_deref(), Some("ray-401"));
        assert_eq!(
            err.identity_authorization_error.as_deref(),
            Some("missing_authorization_header")
        );
        assert_eq!(err.identity_error_code.as_deref(), Some("token_expired"));
    }
}

fn extract_request_tracking_id(headers: Option<&HeaderMap>) -> Option<String> {
    extract_request_id(headers).or_else(|| extract_header(headers, CF_RAY_HEADER))
}

fn extract_request_id(headers: Option<&HeaderMap>) -> Option<String> {
    extract_header(headers, REQUEST_ID_HEADER)
        .or_else(|| extract_header(headers, OAI_REQUEST_ID_HEADER))
}

fn extract_header(headers: Option<&HeaderMap>, name: &str) -> Option<String> {
    headers.and_then(|map| {
        map.get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    })
}

fn extract_x_error_json_code(headers: Option<&HeaderMap>) -> Option<String> {
    let encoded = extract_header(headers, X_ERROR_JSON_HEADER)?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let parsed = serde_json::from_slice::<Value>(&decoded).ok()?;
    parsed
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(crate) fn auth_provider_from_auth(
    auth: Option<CodexAuth>,
    provider: &ModelProviderInfo,
) -> crate::error::Result<CoreAuthProvider> {
    if let Some(api_key) = provider.api_key()? {
        return Ok(CoreAuthProvider {
            token: Some(api_key),
            account_id: None,
        });
    }

    if let Some(token) = provider.experimental_bearer_token.clone() {
        return Ok(CoreAuthProvider {
            token: Some(token),
            account_id: None,
        });
    }

    if let Some(auth) = auth {
        let token = auth.get_token()?;
        Ok(CoreAuthProvider {
            token: Some(token),
            account_id: auth.get_account_id(),
        })
    } else {
        Ok(CoreAuthProvider {
            token: None,
            account_id: None,
        })
    }
}

#[derive(Debug, Deserialize)]
struct UsageErrorResponse {
    error: UsageErrorBody,
}

#[derive(Debug, Deserialize)]
struct UsageErrorBody {
    #[serde(rename = "type")]
    error_type: Option<String>,
    plan_type: Option<PlanType>,
    resets_at: Option<i64>,
}

#[derive(Clone, Default)]
pub(crate) struct CoreAuthProvider {
    token: Option<String>,
    account_id: Option<String>,
}

impl ApiAuthProvider for CoreAuthProvider {
    fn bearer_token(&self) -> Option<String> {
        self.token.clone()
    }

    fn account_id(&self) -> Option<String> {
        self.account_id.clone()
    }
}
