use eyre::{Context, bail, eyre};
use futures::{StreamExt, stream::SplitStream};
use reqwest::Client;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    sync::{
        Mutex,
        mpsc::{self, UnboundedSender},
    },
    task::{JoinError, JoinHandle},
    time::{Duration, Instant},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message as WsMessage, client::IntoClientRequest, protocol::WebSocketConfig},
};
use twitch_api::{HelixClient, twitch_oauth2::UserToken};
use twitch_api::{
    eventsub::{
        self, Event, EventSubscription, Message, SessionData, Transport,
        channel::{ChannelBanV1, ChannelChatMessageV1, ChannelUnbanV1},
        event::websocket::{EventsubWebsocketData, ReconnectPayload, WelcomePayload},
    },
    helix::eventsub::CreateEventSubSubscription,
    twitch_oauth2::{self, ClientId, ClientSecret, RefreshToken, TwitchToken},
    types::{self, UserId},
};

use crate::{
    api::PostRequest,
    rabbit::types::TwitchSettingsChangeContract,
    twitch::websocket::{InitialChatWebsocketConnection, WebsocketConnection},
};

// use crate::twitch::websocket::ActorHandle;

mod websocket;

#[tracing::instrument]
pub async fn run(
    // helix_client: &'static HelixClient<'_, Client>,
    token: UserToken,
    user_id: HashMap<types::UserId, u32>,
    osu_tx: mpsc::UnboundedSender<PostRequest>,
    mut twitch_rx: mpsc::UnboundedReceiver<TwitchSettingsChangeContract>,
) -> eyre::Result<()> {
    let mut initial_conn = InitialChatWebsocketConnection::new(token).await;
    tracing::debug!("Created initial websocket connection");
    let mut new_conn = Err(eyre!("Failed to create a client"));
    while let Ok(message) = initial_conn.receive_message().await {
        match message {
            Some(frame) => {
                tracing::trace!("Creating a full client");
                new_conn = initial_conn
                    .create_full_client(frame, osu_tx, user_id)
                    .await;
                tracing::debug!(?new_conn, "Created a long-term connection");
                break;
            }
            None => {
                tracing::error!("Failed to create a new connection, retrying");
            }
        }
    }
    let new_conn = Arc::new(Mutex::new(new_conn?));
    let conn_clone = Arc::clone(&new_conn);
    tracing::trace!("Starting a receive task");
    let a = tokio::spawn(async move {
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
    let b = tokio::spawn(async move {
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
    let _ = tokio::join!(a, b);
    tracing::trace!("Returning from twitch tasks");
    Ok(())
}
