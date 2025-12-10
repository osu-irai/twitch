use eyre::{Context, Result, bail};
use futures::{FutureExt, StreamExt, stream::SplitStream};
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
        mpsc::{self, UnboundedReceiver, UnboundedSender},
    },
    task::{JoinError, JoinHandle},
    time::{Duration, Instant},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message as WsMessage, client::IntoClientRequest, protocol::WebSocketConfig},
};
use twitch_api::{
    TWITCH_EVENTSUB_WEBSOCKET_URL,
    eventsub::{
        self, Event, EventSubscription, Message, SessionData, Transport,
        channel::{
            self, ChannelBanV1, ChannelCharityCampaignDonateV1, ChannelChatMessageV1,
            ChannelChatMessageV1Payload, ChannelUnbanV1,
        },
        event::websocket::{EventsubWebsocketData, ReconnectPayload, WelcomePayload},
    },
    helix::{HelixClient, eventsub::CreateEventSubSubscription},
    twitch_oauth2::{self, ClientId, ClientSecret, RefreshToken, TwitchToken, UserToken},
    types::{self, EventSubId, UserId},
};

use crate::rabbit::types::{RequestContract, TwitchSettingsChangeContract};

/// Connect to the websocket and return the stream
async fn connect(
    request: &str,
) -> Result<SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>, eyre::Error> {
    let config = Some(WebSocketConfig {
        max_message_size: Some(64 << 20), // 64 MiB
        max_frame_size: Some(16 << 20),   // 16 MiB
        accept_unmasked_frames: false,
        ..WebSocketConfig::default()
    });
    let socket = tokio_tungstenite::connect_async_with_config(request, config, false)
        .await
        .context("Can't connect")?
        .0
        .split()
        .1;

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

// fn process_payload(event: Event) -> Result<Action> {
//     match event {
//         Event::ChannelChatMessageV1(eventsub::Payload { message, .. }) => match message {
//             Message::VerificationRequest(_) => unreachable!(),
//             Message::Revocation() => bail!("Unexpected subscription revocation"),
//             Message::Notification(e) => {
//                 process_message(e)?;
//                 Ok(Action::ResetKeepalive)
//             }
//             _ => todo!(),
//         },
//         _ => todo!(),
//     }
// }

fn process_message(e: ChannelChatMessageV1Payload) -> Result<()> {
    println!("Received message {:#?}", e.message.text);
    Ok(())
}

fn get_bot_id() -> UserId {
    UserId::new("1396690985".to_string())
}

// /// action to perform on received message
// enum Action {
//     /// do nothing with the message
//     Nothing,
//     /// reset the timeout and keep the connection alive
//     ResetKeepalive,
//     /// kill predecessor and swap the handle
//     KillPredecessor,
//     /// spawn successor and await death signal
//     AssignSuccessor(ActorHandle),
// }

pub trait WebsocketConnection {
    async fn receive_message(&mut self) -> Result<Option<String>>;
}

pub struct InitialChatWebsocketConnection<'a> {
    pub token: Mutex<UserToken>,
    pub chats: Vec<UserId>,
    pub client: HelixClient<'a, Client>,
    socket: SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>,
}

pub struct ChatWebsocketConnection<'a> {
    token: Mutex<UserToken>,
    chats: HashMap<UserId, Option<EventSubId>>,
    client: HelixClient<'a, Client>,
    socket: SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>,
    session_id: String,
    twitch_settings_change_rx: UnboundedReceiver<TwitchSettingsChangeContract>,
    request_tx: UnboundedSender<RequestContract>,
}

impl<'a> InitialChatWebsocketConnection<'a> {
    pub async fn new(token: UserToken, chats: Vec<UserId>) -> Self {
        // if the initial connection fails, the entire thing is likely unrecoverable
        let socket = connect("wss://eventsub.wss.twitch.tv/ws").await.unwrap();
        let token = Mutex::new(token);
        let client: HelixClient<'_, Client> = HelixClient::new();
        Self {
            token,
            chats,
            client,
            socket: socket,
        }
    }

    async fn create_full_client(mut self, frame: String) -> Result<ChatWebsocketConnection<'a>> {
        let message = self.receive_message().await?;
        if let Some(message) = message {
            let event_data =
                Event::parse_websocket(&frame).wrap_err("Faield to parse a Websocket frame")?;
            match event_data {
                EventsubWebsocketData::Welcome {
                    payload: WelcomePayload { session },
                    ..
                } => ChatWebsocketConnection::new(self, session, todo!(), todo!()).await,
                _ => todo!(),
            }
        } else {
            Err(eyre::eyre!("Failed to receive a message"))
        }
    }
}

impl<'a> ChatWebsocketConnection<'a> {
    pub async fn new(
        initial: InitialChatWebsocketConnection<'a>,
        session: SessionData<'a>,
        twitch_rx: UnboundedReceiver<TwitchSettingsChangeContract>,
        request_tx: UnboundedSender<RequestContract>,
    ) -> Result<Self> {
        // if the initial connection fails, the entire thing is likely unrecoverable
        let socket = connect(&session.reconnect_url.unwrap()).await.unwrap();
        let token = Mutex::new(initial.token.into_inner());
        let client: HelixClient<'_, Client> = HelixClient::new();
        Ok(Self {
            token,
            chats: initial.chats.into_iter().map(|c| (c, None)).collect(),
            client,
            socket: socket,
            session_id: session.id.to_string(),
            twitch_settings_change_rx: twitch_rx,
            request_tx,
        })
    }
    async fn handle_message(&mut self, frame: String) -> Result<()> {
        let event = Event::parse_websocket(&frame).wrap_err("Failed to parse a Websocket frame")?;
        match event {
            EventsubWebsocketData::Welcome { metadata, payload } => {
                unimplemented!("This is hypothetically unreachable I think?")
            }
            EventsubWebsocketData::Keepalive { metadata, payload } => Ok(()),
            EventsubWebsocketData::Notification { metadata, payload } => {
                self.handle_notification(payload).await
            }
            EventsubWebsocketData::Revocation { metadata, payload } => {
                todo!("I'm not sure yet how to handle revocations")
            }
            EventsubWebsocketData::Reconnect { metadata, payload } => {
                self.socket = connect(payload.session.reconnect_url.unwrap().as_ref())
                    .await
                    .wrap_err("Failed to reconnect to EventSub")?;
                Ok(())
            }
            _ => todo!(),
        }
    }
    async fn handle_notification(&mut self, event: Event) -> Result<()> {
        match event {
            Event::ChannelChatMessageV1(eventsub::Payload { message, .. }) => match message {
                Message::VerificationRequest(_) => unreachable!(),
                Message::Revocation() => bail!("Unexpected subscription revocation"),
                Message::Notification(e) => {
                    process_message(e)?;
                    Ok(())
                }
                _ => todo!(),
            },
            _ => todo!(),
        }
    }

    async fn process_chat_message(&mut self, payload: ChannelChatMessageV1Payload) -> Result<()> {
        let request = construct_message_from_payload()?;
        self.request_tx
            .send(request)
            .wrap_err("Failed to process chat message")
    }

    async fn process_settings_change(
        &mut self,
        settings_change: TwitchSettingsChangeContract,
    ) -> Result<()> {
        match settings_change.is_enabled {
            true => {
                // TODO: subscribe to channel, add eventsub ID to hashmap
                let uid: UserId = settings_change.user_id.into();
                let subscription = self.subscribe_to_channel(&uid).await?;
                self.chats
                    .entry(uid)
                    .and_modify(|v| *v = Some(subscription.clone().id))
                    .or_insert(subscription.clone().id.into());
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
    async fn subscribe_to_channel(
        &mut self,
        channel_id: &UserId,
    ) -> Result<CreateEventSubSubscription<ChannelChatMessageV1>> {
        let token = self.token.lock().await.clone();
        self.client
            .create_eventsub_subscription(
                ChannelChatMessageV1::new(channel_id.to_owned(), get_bot_id()),
                Transport::websocket(&self.session_id),
                &token,
            )
            .await
            .wrap_err("Failed to subscribe to a channel")
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

fn construct_message_from_payload() -> Result<RequestContract> {
    todo!()
}

impl<'a> WebsocketConnection for InitialChatWebsocketConnection<'a> {
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

// struct WebSocketConnection {
//     socket: SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>,
//     helix_client: HelixClient<'static, Client>,
//     token: Arc<Mutex<UserToken>>,
//     subscribed: Arc<AtomicBool>,
//     user_id: Arc<types::UserId>,
//     kill_self_tx: UnboundedSender<()>,
//     twitch_settings_rx: UnboundedReceiver<TwitchSettingsChangeContract>,
//     requests_tx: UnboundedSender<RequestContract>,
// }
// impl WebSocketConnection {
//     async fn receive_message(&mut self) -> Result<Option<String>> {
//         let Some(message) = self.socket.next().await else {
//             return Err(eyre::eyre!("websocket stream closed unexpectedly"));
//         };
//         match message.context("tungstenite error")? {
//             WsMessage::Close(frame) => {
//                 let reason = frame.map(|frame| frame.reason).unwrap_or_default();
//                 Err(eyre::eyre!(
//                     "websocket stream closed unexpectedly with reason {reason}"
//                 ))
//             }
//             WsMessage::Frame(_) => unreachable!(),
//             WsMessage::Ping(_) | WsMessage::Pong(_) => {
//                 // no need to do anything as tungstenite automatically handles pings for you
//                 // but refresh the token just in case
//                 refresh_if_expired(self.token.clone(), self.helix_client).await;
//                 Ok(None)
//             }
//             WsMessage::Binary(_) => unimplemented!(),
//             WsMessage::Text(payload) => Ok(Some(payload)),
//         }
//     }

//     async fn process_message(&self, frame: String) -> Result<Action> {
//         let event_data = Event::parse_websocket(&frame).context("parsing error")?;
//         match event_data {
//             EventsubWebsocketData::Welcome {
//                 payload: WelcomePayload { session },
//                 ..
//             } => {
//                 process_welcome(
//                     &self.subscribed,
//                     &self.token,
//                     self.helix_client,
//                     &self.user_id,
//                     session,
//                 )
//                 .await?;
//                 Ok(Action::KillPredecessor)
//             }
//             EventsubWebsocketData::Reconnect {
//                 payload: ReconnectPayload { session },
//                 ..
//             } => {
//                 let url: String = session.reconnect_url.unwrap().into_owned();
//                 let successor = ActorHandle::spawn(
//                     url,
//                     self.helix_client,
//                     self.kill_self_tx.clone(),
//                     self.token.clone(),
//                     self.subscribed.clone(),
//                     self.user_id.clone(),
//                 );
//                 Ok(Action::AssignSuccessor(successor))
//             }
//             EventsubWebsocketData::Keepalive { .. } => Ok(Action::ResetKeepalive),
//             EventsubWebsocketData::Revocation { metadata, .. } => {
//                 eyre::bail!("got revocation: {metadata:?}")
//             }
//             EventsubWebsocketData::Notification { payload: event, .. } => process_payload(event),
//             _ => Ok(Action::Nothing),
//         }
//     }
// }

// pub struct ActorHandle(JoinHandle<Result<ActorHandle>>);

// impl ActorHandle {
//     pub fn spawn(
//         url: impl IntoClientRequest + Unpin + Send + 'static,
//         helix_client: &'static HelixClient<'_, Client>,
//         kill_predecessor_tx: UnboundedSender<()>,
//         token: Arc<Mutex<UserToken>>,
//         subscribed: Arc<AtomicBool>,
//         user_id: Arc<types::UserId>,
//     ) -> Self {
//         Self(tokio::spawn(async move {
//             let socket = connect(url).await?;
//             // If we receive a reconnect message we want to spawn a new connection to twitch.
//             // The already existing session should wait on the new session to receive a welcome message before being closed.
//             // https://dev.twitch.tv/docs/eventsub/handling-websocket-events/#reconnect-message
//             let (kill_self_tx, mut kill_self_rx) = mpsc::unbounded_channel::<()>();

//             let mut connection = WebSocketConnection {
//                 socket,
//                 helix_client,
//                 token,
//                 subscribed,
//                 user_id,
//                 kill_self_tx,
//             };

//             /// default keepalive duration is 10 seconds
//             const WINDOW: u64 = 10;
//             let mut timeout: Instant = Instant::now() + Duration::from_secs(WINDOW);
//             let mut successor: Option<Self> = None;

//             loop {
//                 tokio::select! {
//                     biased;
//                     result = kill_self_rx.recv() => {
//                         result.unwrap();
//                         let Some(successor) = successor else {
//                             // can't receive death signal from successor if it isn't spawned yet
//                             unreachable!();
//                         };
//                         return Ok(successor);
//                     }
//                     result = connection.receive_message() => if let Some(frame) = result? {
//                         let side_effect = connection.process_message(frame).await?;
//                         match side_effect {
//                             Action::Nothing => {}
//                             Action::ResetKeepalive => timeout = Instant::now() + Duration::from_secs(WINDOW),
//                             Action::KillPredecessor => kill_predecessor_tx.send(())?,
//                             Action::AssignSuccessor(actor_handle) => {
//                                 successor = Some(actor_handle);
//                             },
//                         }
//                     },
//                 }
//             }
//         }))
//     }

//     pub async fn join(self) -> Result<Result<Self>, JoinError> {
//         self.0.await
//     }
// }
