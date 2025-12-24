use eyre::{Context, OptionExt, Result, bail};
use futures::{StreamExt, stream::SplitStream};
use reqwest::Client;
use std::{
    collections::HashMap,
    panic,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::{
    sync::{
        Mutex,
        mpsc::{self, UnboundedSender},
    },
    time::Duration,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message as WsMessage, protocol::WebSocketConfig},
};
use twitch_api::{
    eventsub::{
        self, Event, EventSubscription, Message, SessionData, Transport,
        channel::{ChannelChatMessageV1, ChannelChatMessageV1Payload},
        event::websocket::{EventsubWebsocketData, WelcomePayload},
    },
    helix::{HelixClient, eventsub::CreateEventSubSubscription},
    twitch_oauth2::{ClientId, ClientSecret, TwitchToken, UserToken},
    types::{self, EventSubId, UserId},
};

use crate::{TwitchClient, api::PostRequest, rabbit::types::TwitchSettingsChangeContract};

/// Connect to the websocket and return the stream
async fn connect(
    request: impl AsRef<str>,
) -> Result<SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>, eyre::Error> {
    let config = Some(WebSocketConfig {
        max_message_size: Some(64 << 20), // 64 MiB
        max_frame_size: Some(16 << 20),   // 16 MiB
        accept_unmasked_frames: false,
        ..WebSocketConfig::default()
    });
    let socket = tokio_tungstenite::connect_async_with_config(request.as_ref(), config, false)
        .await
        .context("Can't connect")?
        .0
        .split()
        .1;

    tracing::debug!(url = request.as_ref(), "Created a websocket");
    Ok(socket)
}

async fn refresh_if_expired(token: &Mutex<UserToken>, helix_client: &HelixClient<'_, Client>) {
    let mut lock = token.lock().await;

    if lock.expires_in() >= Duration::from_secs(60) {
        return;
    }

    let client = helix_client.get_client();

    let _ = lock.refresh_token(client).await;
    drop(lock);
}

fn get_client_id_from_env() -> Result<ClientId, std::env::VarError> {
    Ok(ClientId::from(std::env::var("TWITCH_BOT_CLIENT_ID")?))
}

fn get_client_secret_from_env() -> Result<ClientSecret, std::env::VarError> {
    Ok(ClientSecret::from(std::env::var(
        "TWITCH_BOT_CLIENT_SECRET",
    )?))
}
async fn subscribe(
    helix_client: &HelixClient<'_, Client>,
    session_id: String,
    token: &UserToken,
    subscription: impl EventSubscription + Send,
) -> Result<()> {
    let transport: Transport = Transport::websocket(session_id);
    tracing::trace!(?transport, "Created a transport");
    let _event_info: CreateEventSubSubscription<_> = helix_client
        .create_eventsub_subscription(subscription, transport, token)
        .await?;
    Ok(())
}

async fn process_welcome(
    subscribed: &AtomicBool,
    token: &Mutex<UserToken>,
    helix_client: &HelixClient<'_, Client>,
    user_id: &types::UserId,
    session: SessionData<'_>,
) -> Result<()> {
    if subscribed.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }
    println!("Processing welcome");
    let user_token = token.lock().await;
    subscribe(
        helix_client,
        session.id.to_string(),
        &user_token,
        ChannelChatMessageV1::new(user_id.clone(), get_bot_id()),
    )
    .await?;
    subscribed.store(true, Ordering::Relaxed);
    Ok(())
}

pub fn get_bot_id() -> UserId {
    // TODO: Unhardcode this before deploying
    UserId::new("1396690985".to_string())
}

pub trait WebsocketConnection {
    async fn receive_message(&mut self) -> Result<Option<String>>;
}

pub struct InitialChatWebsocketConnection<'a> {
    pub token: Mutex<UserToken>,
    pub client: HelixClient<'a, Client>,
    socket: SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>,
}

pub struct ChatWebsocketConnection<'a> {
    token: Mutex<UserToken>,
    /// Twitch UID -> subscription ID
    chats: HashMap<UserId, Option<EventSubId>>,
    /// Twitch UID -> osu! ID
    osu_users: HashMap<UserId, u32>,
    client: TwitchClient<'a>,
    socket: SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>,
    session_id: String,
    request_tx: UnboundedSender<PostRequest>,
    done_subscribing: bool,
}

impl<'a> std::fmt::Debug for ChatWebsocketConnection<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatWebsocketConnection")
            .field("chats", &self.chats)
            .field("socket", &self.socket)
            .field("session_id", &self.session_id)
            .field("request_tx", &self.request_tx)
            .finish()
    }
}

const BASE_WEBSOCKET_URL: &str = "wss://eventsub.wss.twitch.tv/ws";
impl<'a> InitialChatWebsocketConnection<'a> {
    #[tracing::instrument(skip_all)]
    pub async fn new(token: UserToken) -> Self {
        // if the initial connection fails, the entire thing is likely unrecoverable
        let socket = connect("wss://eventsub.wss.twitch.tv/ws").await.unwrap();
        tracing::trace!("Connected to base EventSub at {BASE_WEBSOCKET_URL}");
        let token = Mutex::new(token);
        let client: TwitchClient = HelixClient::new();
        tracing::trace!("Successfully created a Twitch API client");
        Self {
            token,
            client,
            socket,
        }
    }

    pub async fn create_full_client(
        self,
        frame: String,
        osu_tx: mpsc::UnboundedSender<PostRequest>,
        user_id: HashMap<UserId, u32>,
    ) -> Result<ChatWebsocketConnection<'a>> {
        let event_data =
            Event::parse_websocket(&frame).wrap_err("Failed to parse a Websocket frame")?;
        tracing::trace!(?event_data, "Handling message for full client");
        match event_data {
            EventsubWebsocketData::Welcome {
                payload: WelcomePayload { session },
                ..
            } => {
                tracing::debug!("Received a Welcome message, creating a new client");
                let token = Mutex::new(self.token.into_inner());
                let client: HelixClient<'_, Client> = HelixClient::new();
                return Ok(ChatWebsocketConnection {
                    token,
                    chats: user_id.keys().map(|c| (c.clone(), None)).collect(),
                    osu_users: user_id,
                    client,
                    socket: self.socket,
                    session_id: session.id.to_string(),
                    request_tx: osu_tx,
                    done_subscribing: false,
                });
            }
            EventsubWebsocketData::Keepalive {
                metadata,
                payload: _,
            } => {
                tracing::trace!(
                    ?metadata,
                    "Received a Keepalive message before init is done"
                );
                bail!("Received a Keepalive");
            }
            _ => {
                tracing::error!(?event_data, "Received an unexpected message, bailing");
                bail!("Received an unexpected message");
            }
        }
    }
}

impl<'a> ChatWebsocketConnection<'a> {
    #[tracing::instrument(skip_all)]
    pub async fn handle_message(&mut self, frame: String) -> Result<()> {
        let event = Event::parse_websocket(&frame).wrap_err("Failed to parse a Websocket frame")?;
        match event {
            EventsubWebsocketData::Welcome {
                metadata: _,
                payload: _,
            } => {
                tracing::error!("Received an unexpected Welcome message");
                Ok(())
            }
            EventsubWebsocketData::Keepalive {
                metadata: _,
                payload: _,
            } => {
                tracing::trace!("Received a KeepAlive heartbeat");
                Ok(())
            }
            EventsubWebsocketData::Notification {
                metadata: metadata,
                payload,
            } => {
                tracing::trace!(
                    notification_type = metadata.subscription_type.to_str(),
                    "Received a notification"
                );
                self.handle_notification(payload).await
            }
            EventsubWebsocketData::Revocation {
                metadata: _,
                payload: _,
            } => {
                todo!("I'm not sure yet how to handle revocations")
            }
            EventsubWebsocketData::Reconnect {
                metadata: _,
                payload,
            } => {
                tracing::trace!("Received a reconnect event");
                self.socket = connect(payload.session.reconnect_url.unwrap().as_ref())
                    .await
                    .wrap_err("Failed to reconnect to EventSub")?;
                tracing::trace!("Reconnected to EventSub");
                Ok(())
            }
            _ => todo!(),
        }
    }
    async fn handle_notification(&mut self, event: Event) -> Result<()> {
        match event {
            Event::ChannelChatMessageV1(eventsub::Payload { message, .. }) => {
                tracing::trace!("Message is a channel.chat.message");
                match message {
                    Message::VerificationRequest(_) => unreachable!(
                        "Verification requests shouldn't come through for WebSocket connections"
                    ),
                    Message::Revocation() => bail!("Unexpected subscription revocation"),
                    Message::Notification(e) => self.process_chat_message(e).await,
                    _ => todo!(),
                }
            }
            _ => {
                tracing::error!("Unexpected message type, bailing");
                panic!("Unexpected message type");
            }
        }
    }

    async fn process_chat_message(&mut self, payload: ChannelChatMessageV1Payload) -> Result<()> {
        let osu_id = self
            .osu_users
            .get(&payload.broadcaster_user_id)
            .ok_or_eyre("User not found");
        let Ok(osu_id) = osu_id else {
            tracing::trace!(?self.chats, "User not found");
            return Ok(());
        };
        let request = self.construct_message_from_payload(*osu_id, payload);
        match request {
            Ok(Some(request)) => {
                tracing::trace!("Constructed a valid request");
                self.request_tx
                    .send(request)
                    .wrap_err("Failed to process chat message")
            }
            Ok(None) => {
                tracing::trace!("Not a valid request");
                Ok(())
            }
            Err(err) => {
                tracing::error!(?err, "Failed to parse a message");
                Err(err)
            }
        }
    }
    fn construct_message_from_payload(
        &self,
        osu_id: u32,
        payload: ChannelChatMessageV1Payload,
    ) -> Result<Option<PostRequest>> {
        tracing::trace!(
            from = %payload.broadcaster_user_name,
            to = %payload.chatter_user_name,
            message = payload.message.text,
            "Creating a message");
        if let Some(id) = get_osu_map_id(&payload.message.text) {
            Ok(Some(PostRequest {
                destination_id: osu_id,
                beatmap_id: id,
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn process_settings_change(
        &mut self,
        settings_change: TwitchSettingsChangeContract,
    ) -> Result<()> {
        match settings_change.is_enabled {
            true => {
                // TODO: subscribe to channel, add eventsub ID to hashmap
                let uid: UserId = settings_change.user_id.into();
                let subscription = self.subscribe_to_channel(&uid).await?;
                self.osu_users
                    .entry(uid.clone())
                    .and_modify(|v| *v = settings_change.osu_id)
                    .or_insert(settings_change.osu_id);
                self.chats
                    .entry(uid)
                    .and_modify(|v| *v = Some(subscription.clone().id))
                    .or_insert(Some(subscription.clone().id));
                Ok(())
            }
            false => {
                // TODO: delete subscription by ID, drop both ID and username from hashmap
                let id = self.chats.remove::<UserId>(&settings_change.user_id.into());
                match id {
                    Some(Some(id)) => {
                        self.unsubscribe_from_channel(id).await;
                    }
                    Some(None) => println!("Weren't subscribed to the channel"),
                    None => println!("I think you weren't even allowed to be here?"),
                }
                Ok(())
            }
        }
    }
    pub async fn subscribe_to_channels_initially(&mut self) -> Result<()> {
        if self.done_subscribing {
            tracing::warn!("Tried resubscribing again");
            return Ok(());
        }
        tracing::trace!("Attempting to subscribe to multiple channels");
        let ids = self.chats.clone();
        let ids = ids.keys().clone();
        let keys: Vec<_> = ids.collect();
        tracing::trace!(?keys, "Subscribing");
        for key in keys {
            let subscription_result = self.subscribe_to_channel(key).await?;

            tracing::trace!(
                streamer_id = ?subscription_result.condition.broadcaster_user_id,
                "Created new subscription"
            )
        }
        self.done_subscribing = true;
        Ok(())
    }
    async fn subscribe_to_channel(
        &mut self,
        channel_id: &UserId,
    ) -> Result<CreateEventSubSubscription<ChannelChatMessageV1>> {
        let token = self.token.lock().await.clone();
        let result = self
            .client
            .create_eventsub_subscription(
                ChannelChatMessageV1::new(channel_id.to_owned(), get_bot_id()),
                Transport::websocket(&self.session_id),
                &token,
            )
            .await
            .wrap_err("Failed to subscribe to a channel")?;
        // TODO: this needs to propagate user IDs
        let event_id = result.id.clone();
        tracing::trace!(result = event_id.clone().as_str(), "Subscribed to user");
        self.chats
            .entry(channel_id.clone())
            .and_modify(|v| *v = Some(event_id.clone()))
            .or_insert(Some(event_id.clone()));
        tracing::trace!(event_id = %event_id.clone(), "New event subscription ID");
        Ok(result)
    }
    async fn unsubscribe_from_channel(&mut self, event_id: EventSubId) -> Result<()> {
        let token = self.token.lock().await;
        self.client
            .delete_eventsub_subscription(event_id, &token.clone())
            .await
            .map(|_| ())
            .wrap_err("Failed to unsubscribe from a channel")
    }
}
macro_rules! define_regex {
    ( $( $vis:vis $name:ident: $pat:literal; )* ) => {
        $(
            $vis static $name: std::sync::LazyLock<regex::Regex> =
                std::sync::LazyLock::new(|| regex::Regex::new($pat).unwrap());
        )*
    }
}
define_regex! {
    OSU_URL_MAP_NEW_MATCHER: r"https://osu\.ppy\.sh/beatmapsets/(\d+)(?:(?:/?#(?:osu|mania|taiko|fruits)|<#\d+>)/(\d+))?";
    OSU_URL_MAP_OLD_MATCHER: r"https://osu\.ppy\.sh/b(?:eatmaps)?/(\d+)";
}
pub fn get_osu_map_id(msg: &str) -> Option<u32> {
    if let Some(id) = msg.parse().ok().filter(|_| !msg.starts_with('+')) {
        return Some(id);
    }

    let matcher = if let Some(c) = OSU_URL_MAP_OLD_MATCHER.captures(msg) {
        c.get(1)
    } else {
        OSU_URL_MAP_NEW_MATCHER.captures(msg).and_then(|c| c.get(2))
    };

    matcher.and_then(|c| c.as_str().parse().ok())
}

impl<'a> WebsocketConnection for InitialChatWebsocketConnection<'a> {
    async fn receive_message(&mut self) -> Result<Option<String>> {
        let Some(message) = self.socket.next().await else {
            return Err(eyre::eyre!("websocket stream closed unexpectedly"));
        };
        match message.context("tungstenite error")? {
            WsMessage::Close(frame) => {
                let reason = frame.map(|frame| frame.reason).unwrap_or_default();
                tracing::error!("Connection closed with reason: {reason}");
                Err(eyre::eyre!(
                    "websocket stream closed unexpectedly with reason {reason}"
                ))
            }
            WsMessage::Frame(_) => unreachable!(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => {
                // no need to do anything as tungstenite automatically handles pings for you
                // but refresh the token just in case
                refresh_if_expired(&self.token, &self.client).await;
                Ok(None)
            }
            WsMessage::Binary(_) => unimplemented!(),
            WsMessage::Text(payload) => {
                tracing::trace!(%payload, "Received message");
                Ok(Some(payload))
            }
        }
    }
}
impl<'a> WebsocketConnection for ChatWebsocketConnection<'a> {
    async fn receive_message(&mut self) -> Result<Option<String>> {
        let Some(message) = self.socket.next().await else {
            return Err(eyre::eyre!("websocket stream closed unexpectedly"));
        };
        match message.context("tungstenite error")? {
            WsMessage::Close(frame) => {
                let reason = frame.map(|frame| frame.reason).unwrap_or_default();
                Err(eyre::eyre!(
                    "websocket stream closed unexpectedly with reason {reason}"
                ))
            }
            WsMessage::Frame(_) => unreachable!(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => {
                // no need to do anything as tungstenite automatically handles pings for you
                // but refresh the token just in case
                refresh_if_expired(&self.token, &self.client).await;
                Ok(None)
            }
            WsMessage::Binary(_) => unimplemented!(),
            WsMessage::Text(payload) => Ok(Some(payload)),
        }
    }
}
