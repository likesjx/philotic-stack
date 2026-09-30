use std::time::Duration;

use ansible_mesh_core::integration::{
    EgressPlacementDecision, EgressPlacementPolicy, EgressTrafficClass, IntegrationTarget,
};
use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::Value;
use tokio::time::{Instant, sleep};

const BINDING_ID: &str = "model-catalog-openrouter";
const CATALOG_KEY: &str = "model_catalog.openrouter";
const MODEL_ID: &str = "smoke/governed-catalog-model";
const SYSTEM_GUEST_ID: &str = "system:model-catalog-sync";
const SYSTEM_ROLE: &str = "model-catalog-sync";

#[tokio::main]
async fn main() -> Result<()> {
    let expected_node =
        std::env::var("PHILOTIC_TARGET_NODE").context("PHILOTIC_TARGET_NODE must be set")?;
    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "model-catalog-smoke-probe".into(),
        role: "operator".into(),
        supported_tools: Vec::new(),
    })
    .await
    .context("connect model-catalog smoke probe")?;

    wait_for_catalog(&mut client).await?;

    match client
        .send_request(IpcRequest::GetIntegrationBindings {})
        .await?
    {
        IpcResponse::IntegrationBindingsState {
            integration_bindings,
        } => {
            let entry = integration_bindings
                .into_iter()
                .find(|entry| entry.binding.binding_id == BINDING_ID)
                .context("model catalog binding was not registered")?;
            if entry.binding.owner_agent_id != SYSTEM_GUEST_ID
                || entry.binding.traffic_class != EgressTrafficClass::GeneralApi
                || entry.binding.placement != EgressPlacementPolicy::Local
                || entry.binding.requires_approval
                || entry.execution_node_id.as_deref() != Some(expected_node.as_str())
            {
                bail!("model catalog binding authority was not narrow/local: {entry:?}");
            }
            let IntegrationTarget::Http(target) = entry.binding.target else {
                bail!("model catalog binding was not HTTP");
            };
            if target.credential.is_some()
                || target.allowed_methods != ["GET"]
                || target.allowed_path_prefixes != ["/api/v1/models"]
            {
                bail!("model catalog target policy was broader than expected: {target:?}");
            }
        }
        other => bail!("unexpected integration binding response: {other:?}"),
    }

    match client
        .send_request(IpcRequest::GetIntegrationAudit {
            binding_id: Some(BINDING_ID.into()),
            limit: Some(10),
        })
        .await?
    {
        IpcResponse::IntegrationAuditState { integration_audits }
            if integration_audits.iter().any(|audit| {
                audit.binding_id == BINDING_ID
                    && audit.agent_id == SYSTEM_GUEST_ID
                    && audit.caller_role == SYSTEM_ROLE
                    && audit.traffic_class == EgressTrafficClass::GeneralApi
                    && audit.executor_node_id == expected_node
                    && audit.placement
                        == EgressPlacementDecision::ExecuteLocal {
                            audit_fallback: false,
                        }
                    && audit.outcome == "http_200"
                    && !audit.credential_injected
            }) => {}
        other => bail!("durable governed model-catalog audit was not observable: {other:?}"),
    }

    println!(
        "model catalog governed egress smoke ok: system binding -> runner -> compact catalog -> durable audit"
    );
    Ok(())
}

async fn wait_for_catalog(client: &mut PhiloticClient) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let response = client
            .send_request(IpcRequest::GetConfig {
                key: CATALOG_KEY.into(),
            })
            .await?;
        if let IpcResponse::ConfigData {
            value_json: Some(value_json),
            ..
        } = response
        {
            let catalog: Value =
                serde_json::from_str(&value_json).context("decode compact model catalog")?;
            if catalog
                .as_array()
                .into_iter()
                .flatten()
                .any(|entry| entry.get("id").and_then(Value::as_str) == Some(MODEL_ID))
            {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for governed model catalog sync");
        }
        sleep(Duration::from_millis(250)).await;
    }
}
