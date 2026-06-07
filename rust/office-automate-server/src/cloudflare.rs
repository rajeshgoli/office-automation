use anyhow::{Context, Result, bail};
use reqwest::{Client, Method};
use serde_json::{Map, Value, json};

use crate::config::CloudflareAccessConfig;

const EMPTY_DEVICE_COMMON_NAME: &str = "__office_automate_no_enrolled_devices__";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevicePolicyAction {
    Allow,
    Revoke,
}

pub async fn sync_device_common_name(
    config: &CloudflareAccessConfig,
    common_name: &str,
    action: DevicePolicyAction,
) -> Result<()> {
    let Some(request) = DevicePolicyRequest::from_config(config, common_name, action) else {
        return Ok(());
    };
    request.execute(&Client::new()).await
}

#[derive(Debug)]
struct DevicePolicyRequest {
    api_base_url: String,
    account_id: String,
    app_id: Option<String>,
    policy_id: String,
    api_token: String,
    common_name: String,
    action: DevicePolicyAction,
}

impl DevicePolicyRequest {
    fn from_config(
        config: &CloudflareAccessConfig,
        common_name: &str,
        action: DevicePolicyAction,
    ) -> Option<Self> {
        if !config.device_policy_sync_configured() {
            return None;
        }
        Some(Self {
            api_base_url: config.api_base_url.trim_end_matches('/').to_string(),
            account_id: config.account_id.as_ref()?.trim().to_string(),
            app_id: config.app_id.as_ref().map(|value| value.trim().to_string()),
            policy_id: config.device_policy_id.as_ref()?.trim().to_string(),
            api_token: config.api_token.as_ref()?.trim().to_string(),
            common_name: common_name.trim().to_string(),
            action,
        })
    }

    async fn execute(&self, client: &Client) -> Result<()> {
        if self.common_name.is_empty() {
            bail!("Cloudflare device policy sync requires a non-empty common name");
        }
        let url = self.policy_url();
        let current = self
            .cloudflare_request(client, Method::GET, &url, None)
            .await
            .with_context(|| {
                format!(
                    "failed to fetch Cloudflare Access policy {}",
                    self.policy_id
                )
            })?;
        let mut policy = current
            .get("result")
            .cloned()
            .context("Cloudflare Access policy response missing result")?;
        mutate_policy_common_name_allowlist(&mut policy, &self.common_name, self.action)?;
        let payload = policy_update_payload(policy)?;
        self.cloudflare_request(client, Method::PUT, &url, Some(payload))
            .await
            .with_context(|| {
                format!(
                    "failed to update Cloudflare Access policy {}",
                    self.policy_id
                )
            })?;
        Ok(())
    }

    fn policy_url(&self) -> String {
        if let Some(app_id) = &self.app_id {
            format!(
                "{}/accounts/{}/access/apps/{}/policies/{}",
                self.api_base_url, self.account_id, app_id, self.policy_id
            )
        } else {
            format!(
                "{}/accounts/{}/access/policies/{}",
                self.api_base_url, self.account_id, self.policy_id
            )
        }
    }

    async fn cloudflare_request(
        &self,
        client: &Client,
        method: Method,
        url: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut request = client
            .request(method, url)
            .bearer_auth(&self.api_token)
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .context("Cloudflare API request failed")?;
        let status = response.status();
        let value = response
            .json::<Value>()
            .await
            .context("Cloudflare API returned non-JSON response")?;
        if !status.is_success() || value.get("success").and_then(Value::as_bool) != Some(true) {
            bail!("Cloudflare API request failed with status {status}: {value}");
        }
        Ok(value)
    }
}

fn mutate_policy_common_name_allowlist(
    policy: &mut Value,
    common_name: &str,
    action: DevicePolicyAction,
) -> Result<()> {
    let include = policy
        .get_mut("include")
        .and_then(Value::as_array_mut)
        .context("Cloudflare Access policy result missing include array")?;
    include.retain(|rule| rule.get("certificate").is_none());
    include.retain(|rule| {
        common_name_from_rule(rule)
            .map(|value| value != common_name && value != EMPTY_DEVICE_COMMON_NAME)
            .unwrap_or(true)
    });
    if action == DevicePolicyAction::Allow {
        include.push(json!({"common_name": {"common_name": common_name}}));
    }
    if !include
        .iter()
        .any(|rule| common_name_from_rule(rule).is_some())
    {
        include.push(json!({"common_name": {"common_name": EMPTY_DEVICE_COMMON_NAME}}));
    }
    Ok(())
}

fn common_name_from_rule(rule: &Value) -> Option<&str> {
    rule.get("common_name")
        .and_then(|value| value.get("common_name"))
        .and_then(Value::as_str)
}

fn policy_update_payload(policy: Value) -> Result<Value> {
    let object = policy
        .as_object()
        .context("Cloudflare Access policy result must be an object")?;
    let mut payload = Map::new();
    for key in [
        "name",
        "decision",
        "include",
        "exclude",
        "require",
        "precedence",
        "session_duration",
        "approval_groups",
        "approval_required",
        "purpose_justification_required",
        "purpose_justification_prompt",
        "isolation_required",
    ] {
        if let Some(value) = object.get(key) {
            payload.insert(key.to_string(), value.clone());
        }
    }
    if !payload.contains_key("name")
        || !payload.contains_key("decision")
        || !payload.contains_key("include")
    {
        bail!("Cloudflare Access policy result missing required update fields");
    }
    Ok(Value::Object(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_name_sync_removes_valid_certificate_broad_allow() {
        let mut policy = json!({
            "include": [
                {"certificate": {}},
                {"common_name": {"common_name": "old-device"}}
            ]
        });

        mutate_policy_common_name_allowlist(&mut policy, "new-device", DevicePolicyAction::Allow)
            .expect("mutate");

        assert_eq!(
            policy["include"],
            json!([
                {"common_name": {"common_name": "old-device"}},
                {"common_name": {"common_name": "new-device"}}
            ])
        );
    }

    #[test]
    fn common_name_revoke_leaves_impossible_rule_when_empty() {
        let mut policy = json!({
            "include": [
                {"common_name": {"common_name": "phone"}}
            ]
        });

        mutate_policy_common_name_allowlist(&mut policy, "phone", DevicePolicyAction::Revoke)
            .expect("mutate");

        assert_eq!(
            policy["include"],
            json!([
                {"common_name": {"common_name": EMPTY_DEVICE_COMMON_NAME}}
            ])
        );
    }

    #[test]
    fn policy_update_payload_keeps_only_mutable_fields() {
        let payload = policy_update_payload(json!({
            "id": "readonly",
            "name": "allow-device-mtls",
            "decision": "non_identity",
            "include": [{"common_name": {"common_name": "phone"}}],
            "created_at": "readonly"
        }))
        .expect("payload");

        assert!(payload.get("id").is_none());
        assert!(payload.get("created_at").is_none());
        assert_eq!(payload["decision"], "non_identity");
    }
}
