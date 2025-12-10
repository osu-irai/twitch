use std::sync::LazyLock;

use eyre::Context as _;
use reqwest::Client;
use tokio::sync::{broadcast, mpsc};
use twitch_api::{
    HelixClient, HttpClient,
    twitch_oauth2::{self, AccessToken, TwitchToken, UserToken},
    types::UserId,
};

use crate::{api::get_twitch_users, rabbit::types::TwitchSettingsChangeContract};

pub mod api;
pub mod rabbit;
pub mod twitch;
static HELIX_CLIENT: LazyLock<HelixClient<'_, Client>> =
    LazyLock::new(|| HelixClient::with_client(<reqwest::Client>::default()));

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), eyre::Report> {
    // Setup dotenv, tracing and error reporting with eyre
    run().await.with_context(|| "when running application")?;

    Ok(())
}

/// Run the application
pub async fn run() -> eyre::Result<()> {
    let rmq_connection = rabbit::create_rmq_connection().await?;
    let (twitch_tx, mut twitch_rx) = mpsc::unbounded_channel();
    let consumer = rabbit::setup_twitch_settings_queue(&rmq_connection).await?;
    tokio::spawn(async move { rabbit::run_twitch_queue(twitch_tx, consumer) });

    let (request_tx, request_rx) = mpsc::unbounded_channel();
    let channel = rmq_connection.create_channel().await?;
    tokio::spawn(async move { rabbit::run_publish(channel, request_rx) });
    let users: Vec<_> = get_twitch_users()?;
    let client: &'static HelixClient<_> = LazyLock::force(&HELIX_CLIENT);
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
    println!("Scopes: {:#?}", token.scopes());
    println!("Created a user token");

    let user_id = UserId::new("129898402".to_string());

    println!("Initializing websocket");
    twitch::run(client, token, user_id).await
}
