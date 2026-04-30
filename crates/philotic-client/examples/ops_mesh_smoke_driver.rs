use anyhow::{Context, Result, bail};
use philotic_client::{
    GuestIdentity, HookRoute, IdleBehavior, IpcRequest, IpcResponse, PhiloticClient,
    SubagentLeaseTerms,
};

fn env_required(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("{name} must be set"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "targets".to_string());
    let _socket_path = env_required("PHILOTIC_HOTEL_SOCKET")?;

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "ops-mesh-smoke-driver".into(),
        role: "membrane".into(),
        supported_tools: Vec::new(),
    })
    .await?;

    match mode.as_str() {
        "targets" => {
            let response = client.send_request(IpcRequest::ListDesktopMembraneTargets).await?;
            match response {
                IpcResponse::DesktopMembraneTargetsView { membrane_targets } => {
                    println!("{}", serde_json::to_string_pretty(&membrane_targets)?);
                }
                other => bail!("unexpected targets response: {other:?}"),
            }
        }
        "register-skill" => {
            let skill_name = env_required("PHILOTIC_SKILL_NAME")?;
            let response = client
                .send_request(IpcRequest::RegisterSkill {
                    skill_name: skill_name.clone(),
                    description: format!("mesh smoke skill [{}]", skill_name),
                    subagent_kind: "philote-worker".into(),
                    goal: "Return a structured mesh smoke acknowledgment.".into(),
                    allowed_tools: vec!["session.status".into()],
                    allowed_classes: Vec::new(),
                    hook_subscriptions: Vec::new(),
                    completion_route: HookRoute::PersonaAgent,
                    failure_route: HookRoute::PersonaAgent,
                    idle_behavior: IdleBehavior::Terminate,
                    lease_terms: SubagentLeaseTerms::default(),
                })
                .await?;
            match response {
                IpcResponse::SkillRegistered {
                    skill_name,
                    validation_state,
                    validation_errors,
                } => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "skill_name": skill_name,
                            "validation_state": validation_state,
                            "validation_errors": validation_errors,
                        }))?
                    );
                }
                other => bail!("unexpected register-skill response: {other:?}"),
            }
        }
        "verify-skill" => {
            let skill_name = env_required("PHILOTIC_SKILL_NAME")?;
            let response = client.send_request(IpcRequest::ListSkills {}).await?;
            match response {
                IpcResponse::SkillList { skills } => {
                    let found = skills.iter().any(|skill| {
                        skill
                            .get("skill_name")
                            .and_then(serde_json::Value::as_str)
                            == Some(skill_name.as_str())
                    });
                    if !found {
                        bail!("skill [{}] not present in list", skill_name);
                    }
                    println!("skill [{}] present", skill_name);
                }
                other => bail!("unexpected verify-skill response: {other:?}"),
            }
        }
        "verify-profile" => {
            let profile_name =
                std::env::var("PHILOTIC_PROFILE_NAME").unwrap_or_else(|_| "admin".into());
            let response = client
                .send_request(IpcRequest::ListToolsetProfiles {})
                .await?;
            match response {
                IpcResponse::Standard {
                    ok: true,
                    data: Some(data),
                    ..
                } => {
                    let found = data.as_array().is_some_and(|profiles| {
                        profiles.iter().any(|profile| {
                            profile
                                .get("profile_name")
                                .and_then(serde_json::Value::as_str)
                                == Some(profile_name.as_str())
                        })
                    });
                    if !found {
                        bail!("profile [{}] not present in list", profile_name);
                    }
                    println!("profile [{}] present", profile_name);
                }
                other => bail!("unexpected verify-profile response: {other:?}"),
            }
        }
        "best-place" => {
            let tool_name = std::env::var("PHILOTIC_TOOL_NAME").ok();
            let role_name = std::env::var("PHILOTIC_ROLE_NAME").ok();
            let agent_id = std::env::var("PHILOTIC_AGENT_ID").ok();
            let response = client
                .send_request(IpcRequest::BestPlaceToRun {
                    agent_id,
                    role_name,
                    tool_name,
                    required_markers: Vec::new(),
                    prefer_locality: true,
                })
                .await?;
            match response {
                IpcResponse::Standard {
                    ok: true,
                    data: Some(data),
                    ..
                } => {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                }
                other => bail!("unexpected best-place response: {other:?}"),
            }
        }
        other => bail!("unknown mode [{other}]"),
    }

    Ok(())
}
