use std::{collections::HashMap, sync::Arc};
use tokio::sync::{
    Mutex,
    mpsc::{self},
};
use tracing::{Instrument, info_span};
use twitch_api::twitch_oauth2::UserToken;
use twitch_api::types::{self};

use crate::{
    api::PostRequest,
    rabbit::types::TwitchSettingsChangeContract,
    twitch::websocket::{InitialChatWebsocketConnection, WebsocketConnection},
};

// use crate::twitch::websocket::ActorHandle;

mod websocket;

/// Initialize Twitch connections
#[tracing::instrument(skip(token))]
pub async fn run(
    token: UserToken,
    user_id: HashMap<types::UserId, u32>,
    osu_tx: mpsc::UnboundedSender<PostRequest>,
    mut twitch_rx: mpsc::UnboundedReceiver<TwitchSettingsChangeContract>,
) -> eyre::Result<()> {
    let mut initial_conn = InitialChatWebsocketConnection::new(token).await;
    tracing::debug!("Created initial websocket connection");

    let span = info_span!("connection creation");
    let new_conn = loop {
        match initial_conn.receive_message().await {
            Ok(Some(frame)) => {
                tracing::trace!(parent: &span, "Creating a full client");
                let client = initial_conn
                    .create_full_client(frame, osu_tx, user_id)
                    .instrument(span.clone())
                    .await;
                break client;
            }
            Ok(None) => {
                tracing::warn!(parent: &span, "Failed to create a new connection, retrying");
            }
            Err(e) => {
                tracing::error!(parent: &span, "Error receiving message: {e}");
                break Err(e.into());
            }
        }
    }?;
    tracing::debug!(parent: &span, ?new_conn, "Created a long-term connection");
    let new_conn = Arc::new(Mutex::new(new_conn));

    let conn_clone = Arc::clone(&new_conn);
    tracing::trace!("Starting a receive task");
    let message_task = tokio::spawn(async move {
        tracing::trace!("Starting a message reception loop");
        let mut lock = conn_clone.lock().await;
        lock.subscribe_to_channels_initially().await.unwrap();
        drop(lock);
        loop {
            let mut lock = conn_clone.lock().await;
            let message = lock.receive_message().await;
            drop(lock);
            match message {
                Ok(Some(str)) => {
                    let mut lock = conn_clone.lock().await;
                    lock.handle_message(str).await.unwrap();
                    drop(lock);
                }
                Ok(None) => {
                    tracing::trace!("Received an empty message");
                }
                Err(e) => {
                    tracing::warn!(?e, "Received an error");
                    break;
                }
            }
        }
    });

    let conn_clone = Arc::clone(&new_conn);
    tracing::trace!("Starting a setting change tracking task");
    let settings_task = tokio::spawn(async move {
        while let Some(message) = twitch_rx.recv().await {
            tracing::trace!(?message, "Changing user settings");
            conn_clone
                .lock()
                .await
                .process_settings_change(message)
                .await
                .unwrap();
        }
    });
    let _ = tokio::join!(message_task, settings_task);
    tracing::trace!("Returning from twitch tasks");
    Ok(())
}
