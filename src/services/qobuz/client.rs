use super::auth::{
    auth_headers, build_url, headers_for_optional_session, sign_request, sign_search, timestamp,
};
use super::{QobuzService, qobuz_reqwest_error};
use serde_json::Value;

impl QobuzService {
    pub(super) async fn optional_get_value(
        &self,
        path: &str,
        params: Vec<(&str, String)>,
        request_error: &str,
        json_error: &str,
        qobuz_error: &str,
    ) -> Result<Value, String> {
        let tokens = self.ensure_tokens().await?;
        let session = self.session.read().await.clone();
        let auth = session.as_ref().map(|s| s.user_auth_token.as_str());
        self.get_value_with_optional_auth(
            path,
            params,
            &tokens.app_id,
            auth,
            request_error,
            json_error,
            qobuz_error,
        )
        .await
    }

    // Qobuz client calls keep endpoint, auth, and three error labels explicit for call-site clarity.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn get_value_with_optional_auth(
        &self,
        path: &str,
        params: Vec<(&str, String)>,
        app_id: &str,
        auth_token: Option<&str>,
        request_error: &str,
        json_error: &str,
        qobuz_error: &str,
    ) -> Result<Value, String> {
        let response = self
            .http
            .get(build_url(path))
            .headers(headers_for_optional_session(app_id, auth_token)?)
            .query(&params)
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error(request_error, e))?;
        qobuz_json_message_response(response, json_error, qobuz_error).await
    }

    // Signed Qobuz searches expose the signing inputs and error labels used by multiple endpoints.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn signed_search_value(
        &self,
        path: &str,
        method: &str,
        query: &str,
        limit: u32,
        offset: u32,
        search_type: Option<&str>,
        request_error: &str,
        json_error: &str,
        qobuz_error: &str,
    ) -> Result<Value, String> {
        let secret = self.ensure_secret().await?;
        let timestamp = timestamp();
        let signature = sign_search(
            method,
            query,
            limit,
            offset,
            search_type,
            timestamp,
            &secret,
        );
        let mut params = vec![
            ("query", query.to_string()),
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
            ("request_ts", timestamp.to_string()),
            ("request_sig", signature),
        ];
        if let Some(search_type) = search_type {
            params.push(("type", search_type.to_string()));
        }

        self.optional_get_value(path, params, request_error, json_error, qobuz_error)
            .await
    }

    pub(super) async fn authenticated_get_value(
        &self,
        path: &str,
        params: Vec<(&str, String)>,
    ) -> Result<Value, String> {
        let tokens = self.ensure_tokens().await?;
        let session = self
            .session
            .read()
            .await
            .clone()
            .ok_or_else(|| "Log in to Qobuz to load this section".to_string())?;

        let response = self
            .http
            .get(build_url(path))
            .headers(auth_headers(&tokens.app_id, &session.user_auth_token)?)
            .query(&params)
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error(&format!("Qobuz {path} request failed"), e))?;
        qobuz_json_status_response(path, response).await
    }

    pub(super) async fn signed_get_value(
        &self,
        path: &str,
        method: &str,
        params: Vec<(&str, String)>,
        require_auth: bool,
    ) -> Result<Value, String> {
        let tokens = self.ensure_tokens().await?;
        let secret = self.ensure_secret().await?;
        let session = self.session.read().await.clone();
        let auth = session.as_ref().map(|s| s.user_auth_token.as_str());
        if require_auth && auth.is_none() {
            return Err("Log in to Qobuz to load this section".to_string());
        }

        let timestamp = timestamp();
        let kv = params
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect::<Vec<_>>();
        let signature = sign_request(method, &kv, timestamp, &secret);
        let mut query = params;
        query.push(("request_ts", timestamp.to_string()));
        query.push(("request_sig", signature));

        let response = self
            .http
            .get(build_url(path))
            .headers(headers_for_optional_session(&tokens.app_id, auth)?)
            .query(&query)
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error(&format!("Qobuz {path} request failed"), e))?;
        qobuz_json_status_response(path, response).await
    }
}

async fn qobuz_json_status_response(
    path: &str,
    response: reqwest::Response,
) -> Result<Value, String> {
    let status = response.status();
    let json = response
        .json::<Value>()
        .await
        .map_err(|e| qobuz_reqwest_error(&format!("Qobuz {path} response was not JSON"), e))?;

    if !status.is_success() || json.get("status").and_then(Value::as_str) == Some("error") {
        let message = json
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Qobuz discovery request failed");
        return Err(format!(
            "{} ({status})",
            crate::diagnostics::logging::sanitize_error(message)
        ));
    }

    Ok(json)
}

async fn qobuz_json_message_response(
    response: reqwest::Response,
    json_error: &str,
    qobuz_error: &str,
) -> Result<Value, String> {
    let json = response
        .json::<Value>()
        .await
        .map_err(|e| qobuz_reqwest_error(json_error, e))?;

    if json.get("status").and_then(Value::as_str) == Some("error") {
        let message = json
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(qobuz_error);
        return Err(crate::diagnostics::logging::sanitize_error(message));
    }

    Ok(json)
}
