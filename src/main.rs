use std::{collections::HashMap, fmt::Debug, sync::LazyLock};

use eyre::Context as _;
use reqwest::Client;
use tokio::sync::{broadcast, mpsc};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{
    EnvFilter, filter,
    fmt::{self, Formatter},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};
use twitch_api::{
    HelixClient, HttpClient,
    twitch_oauth2::{self, AccessToken, TwitchToken, UserToken},
    types::UserId,
};

use crate::{
    api::{PostRequest, get_twitch_users},
    rabbit::types::TwitchSettingsChangeContract,
};

pub mod api;
pub mod rabbit;
pub mod twitch;
type TwitchClient<'a> = HelixClient<'a, Client>;
static HELIX_CLIENT: LazyLock<TwitchClient> =
    LazyLock::new(|| HelixClient::with_client(<reqwest::Client>::default()));

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), eyre::Report> {
    // Setup dotenv, tracing and error reporting with eyre
    run().await.with_context(|| "when running application")?;

    Ok(())
}

/// Run the application
#[tracing::instrument]
pub async fn run() -> eyre::Result<()> {
    dotenvy::dotenv().unwrap();
    let format = fmt::format().pretty();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_line_number(true)
        .with_env_filter(EnvFilter::from_default_env())
        .event_format(format)
        .init();
    tracing::trace!("Initializing");
    let rmq_connection = rabbit::create_rmq_connection().await?;
    let (twitch_tx, mut twitch_rx) = mpsc::unbounded_channel();
    let consumer = rabbit::setup_twitch_settings_queue(&rmq_connection).await?;
    tokio::spawn(async move { rabbit::run_twitch_queue(twitch_tx, consumer) });

    let (osu_tx, mut osu_rx) = mpsc::unbounded_channel::<PostRequest>();
    let channel = rabbit::create_rmq_channel(&rmq_connection).await?;

    // let users: Vec<_> = get_twitch_users()?;
    let client: &'static TwitchClient = LazyLock::force(&HELIX_CLIENT);
    let access_token =
        std::env::var("TWITCH_BOT_ACCESS_TOKEN").map(twitch_oauth2::AccessToken::new)?;

    let refresh_token =
        std::env::var("TWITCH_BOT_REFRESH_TOKEN").map(twitch_oauth2::RefreshToken::new)?;

    let client_secret =
        std::env::var("TWITCH_BOT_CLIENT_SECRET").map(twitch_oauth2::ClientSecret::new)?;
    let client_id = std::env::var("TWITCH_BOT_CLIENT_ID").map(twitch_oauth2::ClientId::new)?;

    let token = twitch_oauth2::UserToken::from_existing(
        client.get_client(),
        access_token,
        refresh_token,
        client_secret,
    )
    .await?;
    tracing::trace!("Created a user token");

    let user_id: HashMap<UserId, u32> =
        HashMap::from([(UserId::new("129898402".to_string()), 11482346)]);

    tracing::trace!("Initializing websocket");
    let _task = tokio::spawn(twitch::run(token, user_id, osu_tx, twitch_rx));
    let _recv = tokio::spawn(rabbit::run_publish(channel, osu_rx));
    // let _recv = tokio::spawn(async move {
    //     while let Some(message) = osu_rx.recv().await {
    //         tracing::trace!(?message, "Received an osu message")
    //     }
    // });
    let (res, join) = tokio::join!(_task, _recv);
    Ok(())
}
