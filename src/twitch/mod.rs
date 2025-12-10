use eyre::{Context, bail};
use futures::{StreamExt, stream::SplitStream};
use reqwest::Client;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
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

// use crate::twitch::websocket::ActorHandle;

mod websocket;

pub async fn run(
    helix_client: &'static HelixClient<'_, Client>,
    token: UserToken,
    user_id: types::UserId,
) -> eyre::Result<()> {
    let url = twitch_api::TWITCH_EVENTSUB_WEBSOCKET_URL.clone();
    let token = Arc::new(Mutex::new(token));
    let user_id = Arc::new(user_id);
    let subscribed = Arc::new(AtomicBool::new(false));

    todo!();
    // since this is a root actor without a predecessor it has no previous connection to kill
    // but we still need to give it a sender to satisfy the function signature.
    // `_` and `_unused` have different semantics where `_` is dropped immediately and sender gets a recv error
    // let (dummy_tx, _unused_rx) = mpsc::unbounded_channel();
    // let mut handle = ActorHandle::spawn(
    //     url.clone(),
    //     helix_client,
    //     dummy_tx.clone(),
    //     token.clone(),
    //     subscribed.clone(),
    //     user_id.clone(),
    // );
    // println!("Created a root actor");

    // loop {
    //     handle = match handle.join().await? {
    //         Ok(handle) => handle,
    //         Err(err) => {
    //             println!("Error on join, {err:#?}");
    //             subscribed.store(false, Ordering::Relaxed);
    //             ActorHandle::spawn(
    //                 url.clone(),
    //                 helix_client,
    //                 dummy_tx.clone(),
    //                 token.clone(),
    //                 subscribed.clone(),
    //                 user_id.clone(),
    //             )
    //         }
    //     }
    // }
}
