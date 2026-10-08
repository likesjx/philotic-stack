//! Mesh identity, keys and invites.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

impl IpcServer {
    pub(super) fn mesh_private_key_ref_config_key(hotel_name: &str) -> String {
        format!("mesh_identity_private_key_ref:{hotel_name}")
    }

    pub(super) fn mesh_public_key_config_key(hotel_name: &str) -> String {
        format!("mesh_identity_public_key:{hotel_name}")
    }

    pub(super) fn mesh_transport_private_key_ref_config_key(hotel_name: &str) -> String {
        format!("mesh_transport_private_key_ref:{hotel_name}")
    }

    pub(super) fn mesh_transport_public_key_config_key(hotel_name: &str) -> String {
        format!("mesh_transport_public_key:{hotel_name}")
    }

    pub(super) fn mesh_pending_invite_config_key(nonce: &str) -> String {
        format!("mesh_pending_invite:{nonce}")
    }

    pub(super) fn mesh_auth_key_config_key(node_id: &str) -> String {
        format!("mesh_auth_key:{node_id}")
    }

    pub(super) fn read_string_config(
        graph: &GraphDomain,
        key: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(graph
            .get_config_value(key)?
            .and_then(|value| serde_json::from_str::<String>(&value).ok().or(Some(value)))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()))
    }

    pub(super) fn resolve_internal_secret(
        graph: &GraphDomain,
        secret_ref: &str,
    ) -> anyhow::Result<String> {
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

    pub(super) fn ensure_mesh_identity(
        graph: &GraphDomain,
        hotel_name: &str,
    ) -> anyhow::Result<(SigningKey, String, String)> {
        let private_ref_key = Self::mesh_private_key_ref_config_key(hotel_name);
        let public_key_key = Self::mesh_public_key_config_key(hotel_name);

        if let (Some(secret_ref), Some(public_key_b64)) = (
            Self::read_string_config(graph, &private_ref_key)?,
            Self::read_string_config(graph, &public_key_key)?,
        ) {
            let private_key_hex = Self::resolve_internal_secret(graph, &secret_ref)?;
            let signing_key = signing_key_from_hex(&private_key_hex)?;
            let fingerprint = fingerprint_from_base64url(&public_key_b64)?;
            return Ok((signing_key, public_key_b64, fingerprint));
        }

        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let public_key_b64 = verifying_key_to_base64url(&signing_key.verifying_key());
        let fingerprint = fingerprint_from_base64url(&public_key_b64)?;
        let secret_ref = store_secret(
            graph,
            SecretInput {
                secret_kind: "mesh-hotel-ed25519-private-key".into(),
                scope: format!("hotel:{hotel_name}"),
                allowed_roles: vec!["hotel.internal".into()],
                allowed_guests: Vec::new(),
                plaintext: hex::encode(signing_key.to_bytes()),
            },
        )?;
        graph.set_config_value(&private_ref_key, &serde_json::to_string(&secret_ref)?)?;
        graph.set_config_value(&public_key_key, &serde_json::to_string(&public_key_b64)?)?;
        Ok((signing_key, public_key_b64, fingerprint))
    }

    pub(super) fn ensure_mesh_transport_identity(
        graph: &GraphDomain,
        hotel_name: &str,
    ) -> anyhow::Result<(String, String)> {
        let private_ref_key = Self::mesh_transport_private_key_ref_config_key(hotel_name);
        let public_key_key = Self::mesh_transport_public_key_config_key(hotel_name);

        if let (Some(secret_ref), Some(public_key_b64)) = (
            Self::read_string_config(graph, &private_ref_key)?,
            Self::read_string_config(graph, &public_key_key)?,
        ) {
            let private_key_hex = Self::resolve_internal_secret(graph, &secret_ref)?;
            return Ok((private_key_hex, public_key_b64));
        }

        let (private_key_hex, public_key_b64) = generate_transport_keypair();
        let secret_ref = store_secret(
            graph,
            SecretInput {
                secret_kind: "mesh-hotel-x25519-private-key".into(),
                scope: format!("hotel:{hotel_name}"),
                allowed_roles: vec!["hotel.internal".into()],
                allowed_guests: Vec::new(),
                plaintext: private_key_hex.clone(),
            },
        )?;
        graph.set_config_value(&private_ref_key, &serde_json::to_string(&secret_ref)?)?;
        graph.set_config_value(&public_key_key, &serde_json::to_string(&public_key_b64)?)?;
        Ok((private_key_hex, public_key_b64))
    }

    pub(super) fn local_hotel_record(
        graph: &GraphDomain,
        local_node_id: &str,
    ) -> anyhow::Result<HotelRecord> {
        let hotel_name = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        graph
            .get_hotel(&hotel_name)?
            .ok_or_else(|| anyhow::anyhow!("hotel record missing for hotel [{hotel_name}]"))
    }

    pub(super) fn persist_hotel_mesh_host(
        graph: &GraphDomain,
        hotel_name: &str,
        mesh_host: &str,
    ) -> anyhow::Result<HotelRecord> {
        let mut hotel = graph
            .get_hotel(hotel_name)?
            .ok_or_else(|| anyhow::anyhow!("hotel '{hotel_name}' not found"))?;
        hotel.mesh_host = Some(mesh_host.to_string());
        graph.upsert_hotel(&hotel)?;
        Ok(hotel)
    }

    pub(super) async fn handle_create_mesh_invite(
        graph: &GraphDomain,
        local_node_id: &str,
        hotel_name: String,
        mesh_host: String,
        ttl_secs: Option<u64>,
    ) -> anyhow::Result<serde_json::Value> {
        let local_hotel = Self::local_hotel_record(graph, local_node_id)?;
        if local_hotel.hotel_name != hotel_name {
            bail!(
                "mesh invites may only be created by the active local hotel [{}], not [{}]",
                local_hotel.hotel_name,
                hotel_name
            );
        }

        let hotel = Self::persist_hotel_mesh_host(graph, &hotel_name, &mesh_host)?;
        let (signing_key, public_key_b64, fingerprint) =
            Self::ensure_mesh_identity(graph, &hotel_name)?;
        let (_transport_private_key_hex, transport_public_key_b64) =
            Self::ensure_mesh_transport_identity(graph, &hotel_name)?;
        let now = now_epoch_secs();
        let nonce = generate_nonce();
        let expires_at = now + ttl_secs.unwrap_or(DEFAULT_INVITE_TTL_SECS);

        let invite = sign_invite(
            MeshInvitePayload {
                version: ansible_mesh_core::membership::MESH_INVITE_VERSION,
                hotel_name: hotel.hotel_name.clone(),
                capabilities: hotel.capabilities.clone(),
                mesh_host: hotel
                    .mesh_host
                    .clone()
                    .unwrap_or_else(|| "127.0.0.1".into()),
                mesh_port: hotel.mesh_port,
                blob_port: hotel.blob_port,
                execution_port: hotel.execution_port,
                inviter_pubkey_b64: public_key_b64,
                inviter_fingerprint: fingerprint.clone(),
                inviter_transport_pubkey_b64: transport_public_key_b64,
                nonce: nonce.clone(),
                created_at: now,
                expires_at,
            },
            &signing_key,
        )?;

        graph.set_config_value(
            &Self::mesh_pending_invite_config_key(&nonce),
            &serde_json::json!({
                "hotel_name": hotel.hotel_name,
                "created_at": now,
                "expires_at": expires_at,
                "status": "pending"
            })
            .to_string(),
        )?;

        Ok(serde_json::json!({
            "invite_json": serde_json::to_string_pretty(&invite)?,
            "fingerprint": fingerprint,
            "expires_at": expires_at,
            "nonce": nonce
        }))
    }

    pub(super) async fn handle_accept_mesh_invite(
        graph: &GraphDomain,
        local_node_id: &str,
        hotel_name: String,
        mesh_host: String,
        invite_json: String,
    ) -> anyhow::Result<serde_json::Value> {
        let local_hotel = Self::local_hotel_record(graph, local_node_id)?;
        if local_hotel.hotel_name != hotel_name {
            bail!(
                "mesh invites may only be accepted by the active local hotel [{}], not [{}]",
                local_hotel.hotel_name,
                hotel_name
            );
        }

        let invite: MeshInvite =
            serde_json::from_str(&invite_json).context("parse signed mesh invite JSON")?;
        verify_invite(&invite, now_epoch_secs())?;

        let persisted_local_hotel = Self::persist_hotel_mesh_host(graph, &hotel_name, &mesh_host)?;
        let inviter_hotel = HotelRecord {
            hotel_name: invite.payload.hotel_name.clone(),
            capabilities: invite.payload.capabilities.clone(),
            mesh_host: Some(invite.payload.mesh_host.clone()),
            mesh_port: invite.payload.mesh_port,
            blob_port: invite.payload.blob_port,
            execution_port: invite.payload.execution_port,
            ipc_socket_path: String::new(),
            active_pid: None,
        };
        graph.upsert_hotel(&inviter_hotel)?;

        let (signing_key, public_key_b64, fingerprint) =
            Self::ensure_mesh_identity(graph, &hotel_name)?;
        let (transport_private_key_hex, transport_public_key_b64) =
            Self::ensure_mesh_transport_identity(graph, &hotel_name)?;
        let session_key = derive_transport_session_key(
            &invite.payload.nonce,
            &transport_private_key_hex,
            &invite.payload.inviter_transport_pubkey_b64,
        )?;
        graph.set_config_value(
            &Self::mesh_auth_key_config_key(&invite.payload.capabilities.node_id),
            &serde_json::to_string(&session_key)?,
        )?;
        let join_request = sign_join_request(
            MeshJoinRequestPayload {
                version: ansible_mesh_core::membership::MESH_INVITE_VERSION,
                invite_nonce: invite.payload.nonce.clone(),
                hotel_name: persisted_local_hotel.hotel_name.clone(),
                capabilities: persisted_local_hotel.capabilities.clone(),
                mesh_host: persisted_local_hotel
                    .mesh_host
                    .clone()
                    .unwrap_or_else(|| "127.0.0.1".into()),
                mesh_port: persisted_local_hotel.mesh_port,
                blob_port: persisted_local_hotel.blob_port,
                execution_port: persisted_local_hotel.execution_port,
                joiner_pubkey_b64: public_key_b64,
                joiner_fingerprint: fingerprint,
                joiner_transport_pubkey_b64: transport_public_key_b64,
                requested_at: now_epoch_secs(),
            },
            &signing_key,
        )?;

        let payload = serde_json::to_vec(&join_request)?;
        let msg_id = Uuid::new_v4();
        let timestamp = now_epoch_secs();
        let packet = ansible_mesh_core::BeaconMessage {
            version: ansible_mesh_core::membership::MESH_INVITE_VERSION,
            msg_id,
            src_node: persisted_local_hotel.capabilities.node_id.clone(),
            dest_node: invite.payload.capabilities.node_id.clone(),
            msg_type: ansible_mesh_core::MsgType::MeshMembershipAccept,
            seq: 0,
            total: 1,
            payload: payload.into(),
            timestamp,
            hmac: ansible_mesh_core::BeaconPayload::default(),
        };

        let target_addr = format!("{}:{}", invite.payload.mesh_host, invite.payload.mesh_port);
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .context("bind local UDP socket for mesh join request")?;
        socket
            .send_to(&serde_json::to_vec(&packet)?, &target_addr)
            .await
            .with_context(|| format!("send mesh join request to {target_addr}"))?;

        Ok(serde_json::json!({
            "inviter_hotel": invite.payload.hotel_name,
            "target_addr": target_addr,
            "nonce": invite.payload.nonce
        }))
    }
}
