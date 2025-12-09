use std::sync::LazyLock;

use eyre::Context as _;
use reqwest::Client;
use twitch_api::{
    HelixClient, HttpClient,
    twitch_oauth2::{self, AccessToken, TwitchToken, UserToken},
    types::UserId,
};

pub mod rabbit;
mod websocket;
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
    rabbit::setup_rmq().await
    // // Create the HelixClient, which is used to make requests to the Twitch API
    // let client: &'static HelixClient<_> = LazyLock::force(&HELIX_CLIENT);
    // // Get the user id of the channel we want to listen to
    // let access_token =
    //     std::env::var("TWITCH_BOT_ACCESS_TOKEN").map(twitch_oauth2::AccessToken::new)?;

    // let refresh_token =
    //     std::env::var("TWITCH_BOT_REFRESH_TOKEN").map(twitch_oauth2::RefreshToken::new)?;

    // let client_secret =
    //     std::env::var("TWITCH_BOT_CLIENT_SECRET").map(twitch_oauth2::ClientSecret::new)?;
    // let client_id = std::env::var("TWITCH_BOT_CLIENT_ID").map(twitch_oauth2::ClientId::new)?;

    // // let (access_token, duration, refresh_token) = refresh_token
    // //     .refresh_token(client.get_client(), &client_id, Some(&client_secret))
    // //     .await?;

    // let token = twitch_oauth2::UserToken::from_existing(
    //     client.get_client(),
    //     access_token,
    //     refresh_token,
    //     client_secret,
    // )
    // .await?;
    // println!("Scopes: {:#?}", token.scopes());
    // println!("Created a user token");

    // let user_id = UserId::new("129898402".to_string());

    // println!("Initializing websocket");
    // websocket::run(client, token, user_id).await
}
