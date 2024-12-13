use axum::{
    body::Bytes, extract::{Path, State},
};
use tracing::debug;
use tokio::{
    fs,
    io::{self, BufWriter},
};
use uuid::Uuid;

use crate::{
    api::errors::internal_and_log,
    ApiResult, AppState, AVATARS_VAR
};
use crate::api::figura::profile::send_event;
use super::super::figura::websocket::S2CMessage;
use super::super::figura::websocket::SessionMessage;

pub async fn temp_avatar(
    Path(uuid): Path<Uuid>,
    State(state): State<AppState>,
    body: Bytes,
) -> ApiResult<String> {
    let request_data = body;

    if let Some(user_info) = state.user_manager.get_by_uuid(&uuid) {
        tracing::info!(
            "internal api trying upload temp avatar for {} ({})",
            user_info.uuid,
            user_info.nickname
        );
        let avatar_file = format!("{}/temp/{}.moon", *AVATARS_VAR, user_info.uuid);
        let mut file = BufWriter::new(fs::File::create(&avatar_file).await.map_err(internal_and_log)?);
        io::copy(&mut request_data.as_ref(), &mut file).await.map_err(internal_and_log)?;
    }
    Ok("ok".to_string())
}

pub async fn upload_avatar(
    Path(uuid): Path<Uuid>,
    State(state): State<AppState>,
    body: Bytes,
) -> ApiResult<String> {
    let request_data = body;

    if let Some(user_info) = state.user_manager.get_by_uuid(&uuid) {
        tracing::info!(
            "internal api trying upload avatar for {} ({})",
            user_info.uuid,
            user_info.nickname
        );
        let avatar_file = format!("{}/{}.moon", *AVATARS_VAR, user_info.uuid);
        let mut file = BufWriter::new(fs::File::create(&avatar_file).await.map_err(internal_and_log)?);
        io::copy(&mut request_data.as_ref(), &mut file).await.map_err(internal_and_log)?;
    }
    Ok("ok".to_string())
}

pub async fn delete_avatar(
    Path(uuid): Path<Uuid>,
    State(state): State<AppState>
) -> ApiResult<String> {
    if let Some(user_info) = state.user_manager.get_by_uuid(&uuid) {
        tracing::info!(
            "internal api trying to delete avatar for {} ({})",
            user_info.uuid,
            user_info.nickname
        );
        let avatar_file = format!("{}/{}.moon", *AVATARS_VAR, user_info.uuid);
        fs::remove_file(avatar_file).await.map_err(internal_and_log)?;
        send_event(&state, &user_info.uuid).await;
    }
    Ok("ok".to_string())
}

pub async fn user_event(
    Path(uuid): Path<Uuid>,
    State(state): State<AppState>,
) -> ApiResult<String> {
    tracing::info!("internal api request update avatar for user {}", uuid);
    if let Some(session) = state.session.get(&uuid) {
        if session.send(SessionMessage::Ping(S2CMessage::Event(uuid).into())).await.is_err() {
            debug!("[WebSocket] Failed to send Event! WS doesn't connected? UUID: {uuid}")
        };
    } else {
        debug!("[WebSocket] Failed to send Event! Can't find UUID: {uuid}")
    };
    Ok("ok".to_string())
}
