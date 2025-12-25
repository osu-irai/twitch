use std::{collections::HashMap, sync::LazyLock};

use eyre::Context as _;
use reqwest::{Client, Url};
use tokio::sync::mpsc;
use tracing_subscriber::{
    EnvFilter,
    fmt::{self},
    util::SubscriberInitExt,
};
use twitch_api::{
    HelixClient,
    twitch_oauth2::{self, UserToken},
    types::UserId,
};

use crate::api::PostRequest;

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
    tracing::debug!("Initializing");

    let rmq_connection = rabbit::create_rmq_connection().await?;
    let (twitch_tx, twitch_rx) = mpsc::unbounded_channel();
    let consumer = rabbit::setup_twitch_settings_queue(&rmq_connection).await?;
    let _queue_task = tokio::spawn(async move { rabbit::run_twitch_queue(twitch_tx, consumer) });

    let (osu_tx, osu_rx) = mpsc::unbounded_channel::<PostRequest>();
    let channel = rabbit::create_rmq_channel(&rmq_connection).await?;

    // let users: Vec<_> = get_twitch_users()?;
    LazyLock::force(&HELIX_CLIENT);
    let access_token =
        std::env::var("TWITCH_BOT_ACCESS_TOKEN").map(twitch_oauth2::AccessToken::new)?;

    let refresh_token =
        std::env::var("TWITCH_BOT_REFRESH_TOKEN").map(twitch_oauth2::RefreshToken::new)?;
    let client_secret =
        std::env::var("TWITCH_BOT_CLIENT_SECRET").map(twitch_oauth2::ClientSecret::new)?;
    let client_id = std::env::var("TWITCH_BOT_CLIENT_ID").map(twitch_oauth2::ClientId::new)?;
    let redirect_url = std::env::var("TWITCH_BOT_REDIRECT_URL")
        .map(|arg| Url::parse(&arg).unwrap())
        .unwrap();

    let token = {
        // These may be needed later
        // let scopes = vec![
        //     Scope::ChannelBot,
        //     Scope::UserReadChat,
        //     Scope::UserWriteChat,
        //     Scope::UserBot,
        // ];
        let builder = UserToken::from_existing_or_refresh_token(
            &*HELIX_CLIENT,
            access_token,
            refresh_token,
            client_id,
            client_secret,
        )
        .await?;
        builder
    };

    tracing::trace!("Created a user token");

    let user_id: HashMap<UserId, u32> =
        HashMap::from([(UserId::new("129898402".to_string()), 11482346)]);

    tracing::trace!("Initializing websocket");
    let _twitch_task = tokio::spawn(twitch::run(token, user_id, osu_tx, twitch_rx));
    let _publish_task = tokio::spawn(rabbit::run_publish(channel, osu_rx));
    let _res = tokio::join!(_queue_task, _twitch_task, _publish_task);
    Ok(())
}
