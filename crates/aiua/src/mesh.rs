use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::membership::derive_transport_shared_key;
use ansible_mesh_core::storage::HotelRecord;
use anyhow::Result;

use crate::vault::{SecretAccess, resolve_secret};

pub fn mesh_member_public_key_config_key(hotel_name: &str) -> String {
    format!("mesh_member_public_key:{hotel_name}")
}

pub fn mesh_member_transport_public_key_config_key(hotel_name: &str) -> String {
    format!("mesh_member_transport_public_key:{hotel_name}")
}

pub fn mesh_public_key_config_key(hotel_name: &str) -> String {
    format!("mesh_identity_public_key:{hotel_name}")
}

pub fn mesh_transport_public_key_config_key(hotel_name: &str) -> String {
    format!("mesh_transport_public_key:{hotel_name}")
}

pub fn mesh_auth_key_config_key(node_id: &str) -> String {
    format!("mesh_auth_key:{node_id}")
}

pub fn mesh_transport_private_key_ref_config_key(hotel_name: &str) -> String {
    format!("mesh_transport_private_key_ref:{hotel_name}")
}

pub fn read_string_config(graph: &GraphDomain, key: &str) -> Result<Option<String>> {
    Ok(graph
        .get_config_value(key)?
        .and_then(|value| serde_json::from_str::<String>(&value).ok().or(Some(value)))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty()))
}

pub fn resolve_internal_secret(graph: &GraphDomain, secret_ref: &str) -> Result<String> {
    resolve_secret(
        graph,
        secret_ref,
        &SecretAccess {
            role: "hotel.internal".into(),
            guest_id: "aiua".into(),
        },
    )?
    .ok_or_else(|| anyhow::anyhow!("vault secret not found: {secret_ref}"))
}

pub fn local_hotel_for_node(graph: &GraphDomain, node_id: &str) -> Result<Option<HotelRecord>> {
    Ok(graph
        .list_hotels()?
        .into_iter()
        .find(|hotel| hotel.capabilities.node_id == node_id))
}

fn peer_auth_context(local_node_id: &str, remote_node_id: &str) -> String {
    let mut ids = [local_node_id, remote_node_id];
    ids.sort_unstable();
    format!("philotic-mesh-peer-auth-v2:{}:{}", ids[0], ids[1])
}

pub fn mesh_auth_key_for_node(
    graph: &GraphDomain,
    local_node_id: &str,
    target_node_id: &str,
) -> Result<Option<String>> {
    if let Some(stored) = read_string_config(graph, &mesh_auth_key_config_key(target_node_id))? {
        return Ok(Some(stored));
    }

    let Some(local_hotel) = local_hotel_for_node(graph, local_node_id)? else {
        return Ok(None);
    };
    let Some(remote_hotel) = graph
        .list_hotels()?
        .into_iter()
        .find(|hotel| hotel.capabilities.node_id == target_node_id)
    else {
        return Ok(None);
    };

    let Some(secret_ref) = read_string_config(
        graph,
        &mesh_transport_private_key_ref_config_key(&local_hotel.hotel_name),
    )?
    else {
        return Ok(None);
    };
    let Some(remote_transport_pubkey) = read_string_config(
        graph,
        &mesh_member_transport_public_key_config_key(&remote_hotel.hotel_name),
    )?
    else {
        return Ok(None);
    };

    let local_private_key_hex = resolve_internal_secret(graph, &secret_ref)?;
    derive_transport_shared_key(
        &peer_auth_context(local_node_id, target_node_id),
        &local_private_key_hex,
        &remote_transport_pubkey,
    )
    .map(Some)
}
